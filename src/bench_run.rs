use crate::{
    asset, config, game_judge, legacy_judge, lock, model_candidate, run_input, runtime, task,
    workspace,
};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;

struct JudgeModel {
    reference: String,
    route: config::ModelRoute,
}

pub fn execute(args: &[String]) -> Result<u8> {
    let options = run_input::RunOptions::parse(args)?;
    let state_root = workspace::state_root()?;
    let mut journal =
        crate::run_journal::RunJournal::begin(&state_root, &options.task, &options.agent)?;
    match execute_inner(&options, &state_root, &mut journal) {
        Ok(code) => Ok(code),
        Err(error) => match journal.fail(&error) {
            Ok(()) => Err(error.context(format!("run {} failed", journal.run_id))),
            Err(journal_error) => Err(error.context(format!(
                "could not persist terminal run failure: {journal_error:#}"
            ))),
        },
    }
}

fn execute_inner(
    options: &run_input::RunOptions,
    state_root: &Path,
    journal: &mut crate::run_journal::RunJournal,
) -> Result<u8> {
    use crate::run_journal::RunStage;

    let config = config::discover(&std::env::current_dir()?)?;
    if let (Some(path), Some(model)) = (config.path.as_deref(), config.judge_model.as_deref()) {
        config::resolve_model_route(path, model)?;
    }
    let status = runtime::preflight(&config.runtime)?;
    journal.advance(RunStage::RuntimeReady)?;
    let mut loaded = options.load(state_root, &journal.run_id, config.judge_model.clone())?;
    journal.bind_locks(&loaded.task_lock_digest, &loaded.candidate_lock_digest)?;
    let judge_model = resolve_judge_model(&loaded.task, loaded.judge_model.as_deref(), &config)?;
    anyhow::ensure!(
        status.provider == "docker",
        "execution through configured Runtime {:?} is not implemented yet",
        status.provider
    );
    resolve_task_images(&mut loaded.task, &loaded.resolved_images)?;
    journal.advance(RunStage::InputsResolved)?;
    let game = start_game(&loaded.task, state_root)?;
    let candidate_workspace = workspace::create(&loaded.task)?;
    journal.advance(RunStage::CandidateRunning)?;
    let model_execution = execute_candidate(
        &loaded.task,
        &loaded.candidate,
        loaded.model.as_deref(),
        &config,
        &candidate_workspace,
        game.as_ref(),
    )?;
    journal.advance(RunStage::CandidateCompleted)?;
    let submission = workspace::create_submission(&loaded.task, &candidate_workspace)?;
    journal.advance(RunStage::Judging)?;
    let judge_result = execute_judge(
        &loaded.task,
        &loaded.judge,
        &submission,
        game.as_ref(),
        judge_model.as_ref().map(|model| &model.route),
    )?;
    let primary = primary_metric(&loaded.task);
    let score = judge_result
        .metrics
        .get(&primary.name)
        .and_then(serde_json::Value::as_str)
        .expect("validated JudgeResult contains the primary metric");
    let (record, path) = crate::result_record::LocalResultRecord::save(
        state_root,
        crate::result_record::NewLocalResult {
            run_id: &journal.run_id,
            task_id: &loaded.task.id,
            task_lock_digest: &loaded.task_lock_digest,
            agent: &options.agent,
            candidate_lock_digest: &loaded.candidate_lock_digest,
            agent_identity: &loaded.candidate.identity,
            judge_identity: &judge_identity(&loaded.judge.identity, judge_model.as_ref()),
            runtime_provider: &status.provider,
            model: loaded.model.as_deref(),
            model_usage: model_execution.as_ref(),
            primary_metric: &primary.name,
            score,
            judge_result: &judge_result,
        },
    )?;
    journal.complete(&path, &record.result_digest)?;
    print_result(options, &loaded.task.id, score, &record.run_id, &path)?;
    Ok(0)
}

fn resolve_judge_model(
    task: &task::TaskInfo,
    locked_reference: Option<&str>,
    config: &config::LocalConfig,
) -> Result<Option<JudgeModel>> {
    let requires_model = task
        .legacy_judge
        .as_ref()
        .is_some_and(|source| source.requires_model_gateway);
    if !requires_model {
        return Ok(None);
    }
    let reference = locked_reference.map(str::to_owned).ok_or_else(|| {
        anyhow::anyhow!(
            "Task {:?} requires bench.judge_model in .a3s/config.acl",
            task.id
        )
    })?;
    let path = config.path.as_deref().ok_or_else(|| {
        anyhow::anyhow!("Judge model gateway requires project-local or user-local .a3s/config.acl")
    })?;
    let route = config::resolve_model_route(path, &reference)?;
    Ok(Some(JudgeModel { reference, route }))
}

fn judge_identity(asset_identity: &str, model: Option<&JudgeModel>) -> String {
    match model {
        Some(model) => format!("{asset_identity};model={}", model.reference),
        None => asset_identity.to_owned(),
    }
}

fn start_game(task: &task::TaskInfo, state_root: &Path) -> Result<Option<game_judge::GameSession>> {
    match task.legacy_judge.as_ref() {
        Some(source) if source.mode == "game_server" => {
            Ok(Some(game_judge::GameSession::start(source, state_root)?))
        }
        _ => Ok(None),
    }
}

fn execute_candidate(
    task: &task::TaskInfo,
    candidate: &asset::LocalAssetPackage,
    model: Option<&str>,
    config: &config::LocalConfig,
    candidate_workspace: &Path,
    game: Option<&game_judge::GameSession>,
) -> Result<Option<model_candidate::ModelExecution>> {
    let Some(model) = model else {
        anyhow::ensure!(
            game.is_none(),
            "interactive game Tasks require a model-backed Candidate"
        );
        runtime::execute_docker_candidate(task, candidate, candidate_workspace)?;
        return Ok(None);
    };
    let config_path = config.path.as_deref().ok_or_else(|| {
        anyhow::anyhow!("--model requires project-local or user-local .a3s/config.acl")
    })?;
    let prompt = std::fs::read_to_string(task.root.join("public/prompt.md"))?;
    let instructions_path = candidate.model_instructions_path()?;
    let instructions = std::fs::read_to_string(&instructions_path).with_context(|| {
        format!(
            "model Candidate is missing instructions at {}",
            instructions_path.display()
        )
    })?;
    let game_url = game.map(game_judge::GameSession::url);
    Ok(Some(model_candidate::execute(
        model_candidate::ModelCandidateRequest {
            config_path,
            model,
            task_prompt: &prompt,
            candidate_instructions: &instructions,
            workspace: candidate_workspace,
            work_image: &task.work_image,
            work_platform: task.work_platform.as_deref(),
            game_network: game.map(|session| {
                (
                    session.network(),
                    game_url.as_deref().expect("game URL accompanies session"),
                )
            }),
            public_internet: task.work_network_need == "public_internet",
            timeout_sec: task.candidate_timeout_sec,
            max_tool_rounds: candidate.model_max_steps()?,
        },
    )?))
}

fn execute_judge(
    task: &task::TaskInfo,
    judge: &asset::LocalAssetPackage,
    submission: &Path,
    game: Option<&game_judge::GameSession>,
    model: Option<&config::ModelRoute>,
) -> Result<runtime::JudgeResult> {
    if let (Some(session), Some(source)) = (game, &task.legacy_judge) {
        session.finish(task, source)
    } else if let Some(source) = &task.legacy_judge {
        legacy_judge::execute(task, source, submission, model)
    } else {
        runtime::execute_docker_judge(task, judge, submission)
    }
}

fn primary_metric(task: &task::TaskInfo) -> &task::MetricInfo {
    task.metrics
        .iter()
        .find(|metric| metric.role == "primary")
        .expect("Task parser guarantees one primary metric")
}

fn resolve_task_images(
    task: &mut task::TaskInfo,
    locked: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    fn resolve(
        reference: &str,
        platform: Option<&str>,
        locked: &std::collections::BTreeMap<String, String>,
    ) -> Result<String> {
        locked
            .get(&lock::image_key(reference, platform))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("TaskLock does not bind image {reference:?}"))
    }
    task.work_image = resolve(
        &task.work_image.clone(),
        task.work_platform.as_deref(),
        locked,
    )?;
    if let Some(seed) = &mut task.workspace_seed {
        seed.image = resolve(&seed.image.clone(), seed.platform.as_deref(), locked)?;
    }
    if let Some(judge) = &mut task.legacy_judge {
        judge.image = resolve(&judge.image.clone(), judge.platform.as_deref(), locked)?;
    }
    Ok(())
}

fn print_result(
    options: &run_input::RunOptions,
    task_id: &str,
    score: &str,
    run_id: &str,
    path: &Path,
) -> Result<()> {
    if options.json {
        crate::output::print_success(
            "run",
            json!({
                "status":"completed", "governance_status":"local_unofficial",
                "run_id":run_id, "task_id":task_id, "score":score,
                "result_path":path
            }),
        )?;
    } else {
        println!("COMPLETED  score={score}  task={task_id}");
        println!("run:    {run_id}");
        println!("result: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
