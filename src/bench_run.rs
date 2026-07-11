use crate::{
    asset, config, game_judge, legacy_judge, lock, model_candidate, runtime, task, workspace,
};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

pub fn execute(args: &[String]) -> Result<u8> {
    let options = RunOptions::parse(args)?;
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
    options: &RunOptions,
    state_root: &Path,
    journal: &mut crate::run_journal::RunJournal,
) -> Result<u8> {
    use crate::run_journal::RunStage;

    let config = config::discover(&std::env::current_dir()?)?;
    let status = runtime::preflight(&config.runtime)?;
    journal.advance(RunStage::RuntimeReady)?;
    let mut loaded = options.load(state_root)?;
    anyhow::ensure!(
        status.provider == "docker",
        "execution through configured Runtime {:?} is not implemented yet",
        status.provider
    );
    resolve_task_images(&mut loaded.task, loaded.locked_images.as_ref())?;
    let judge = resolve_judge(&loaded.task, state_root)?;
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
    let judge_result = execute_judge(&loaded.task, &judge, &submission, game.as_ref())?;
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
            agent: &options.agent,
            agent_identity: &loaded.candidate.identity,
            judge_identity: &judge.identity,
            runtime_provider: &status.provider,
            model: loaded.model.as_deref(),
            model_usage: model_execution.as_ref(),
            primary_metric: &primary.name,
            score,
            judge_result: &judge_result,
        },
    )?;
    journal.complete(&path)?;
    print_result(options, &loaded.task.id, score, &record.run_id, &path)?;
    Ok(0)
}

struct RunOptions {
    task: String,
    agent: String,
    model: Option<String>,
    json: bool,
    locked: bool,
}

struct LoadedRun {
    task: task::TaskInfo,
    candidate: asset::LocalAgentAsset,
    model: Option<String>,
    locked_images: Option<BTreeMap<String, String>>,
}

impl RunOptions {
    fn parse(args: &[String]) -> Result<Self> {
        anyhow::ensure!(!args.is_empty(), "run requires one Task reference");
        let mut agent = None;
        let mut model = None;
        let mut json = false;
        let mut locked = false;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--agent" if agent.is_none() && index + 1 < args.len() => {
                    agent = Some(args[index + 1].clone());
                    index += 2;
                }
                "--model" if model.is_none() && index + 1 < args.len() => {
                    model = Some(args[index + 1].clone());
                    index += 2;
                }
                "--json" if !json => {
                    json = true;
                    index += 1;
                }
                "--locked" if !locked => {
                    locked = true;
                    index += 1;
                }
                value => anyhow::bail!("invalid or duplicate run option {value:?}"),
            }
        }
        Ok(Self {
            task: args[0].clone(),
            agent: agent.ok_or_else(|| anyhow::anyhow!("run requires exactly one --agent"))?,
            model,
            json,
            locked,
        })
    }

    fn load(&self, state_root: &Path) -> Result<LoadedRun> {
        if self.locked {
            anyhow::ensure!(
                self.model.is_none(),
                "--model cannot alter a locked Candidate"
            );
            let (task_lock, task_artifact) = lock::load_task(Path::new(&self.task), state_root)?;
            let (candidate_lock, candidate_artifact) =
                lock::load_candidate(Path::new(&self.agent), state_root)?;
            return Ok(LoadedRun {
                task: task::load_local(&task_artifact)?,
                candidate: asset::load_local(&candidate_artifact)?,
                model: candidate_lock.model,
                locked_images: Some(task_lock.resolved_images),
            });
        }
        anyhow::ensure!(
            self.task.starts_with("./") || self.task.starts_with("../"),
            "this development build currently executes local Tasks only"
        );
        Ok(LoadedRun {
            task: task::load_local(Path::new(&self.task))?,
            candidate: asset::resolve(&self.agent, state_root)?,
            model: self.model.clone(),
            locked_images: None,
        })
    }
}

fn resolve_judge(task: &task::TaskInfo, state_root: &Path) -> Result<asset::LocalAgentAsset> {
    if task.judge_asset.starts_with("oci://") {
        asset::resolve(&task.judge_asset, state_root)
    } else {
        asset::load_local(&task.root.join(&task.judge_asset))
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
    candidate: &asset::LocalAgentAsset,
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
    let instructions_path = candidate.root.join("agent.md");
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
        },
    )?))
}

fn execute_judge(
    task: &task::TaskInfo,
    judge: &asset::LocalAgentAsset,
    submission: &Path,
    game: Option<&game_judge::GameSession>,
) -> Result<runtime::JudgeResult> {
    if let (Some(session), Some(source)) = (game, &task.legacy_judge) {
        session.finish(task, source)
    } else if let Some(source) = &task.legacy_judge {
        legacy_judge::execute(task, source, submission)
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
    locked: Option<&BTreeMap<String, String>>,
) -> Result<()> {
    fn resolve(
        reference: &str,
        platform: Option<&str>,
        locked: Option<&BTreeMap<String, String>>,
    ) -> Result<String> {
        if let Some(images) = locked {
            return images
                .get(&lock::image_key(reference, platform))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("TaskLock does not bind image {reference:?}"));
        }
        Ok(runtime::resolve_image(reference, platform)?.immutable_ref)
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
    options: &RunOptions,
    task_id: &str,
    score: &str,
    run_id: &str,
    path: &Path,
) -> Result<()> {
    if options.json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "schema":"a3s.bench.output.v1", "status":"completed",
                "governance_status":"local_unofficial", "run_id":run_id,
                "task_id":task_id, "score":score, "result_path":path
            }))?
        );
    } else {
        println!("COMPLETED  score={score}  task={task_id}");
        println!("run:    {run_id}");
        println!("result: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
