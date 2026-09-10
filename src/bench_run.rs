use crate::{
    asset, config, game_judge, legacy_judge, lock, model_candidate, run_input, runtime, task,
    workspace,
};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

struct JudgeModel {
    reference: String,
    route: config::ModelRoute,
}

struct RuntimeExecution<'a> {
    provider: &'a str,
    resolved_images: &'a std::collections::BTreeMap<String, String>,
}

struct CandidateRun {
    candidate_event_log_digest: Option<String>,
    execution: crate::result_record::CandidateExecution,
    model_usage: Option<model_candidate::ModelExecution>,
}

struct TransientRunDirs {
    workspace: Option<PathBuf>,
    seed_workspace: Option<PathBuf>,
    submission: Option<PathBuf>,
}

impl TransientRunDirs {
    fn new() -> Self {
        Self {
            workspace: None,
            seed_workspace: None,
            submission: None,
        }
    }
}

impl Drop for TransientRunDirs {
    fn drop(&mut self) {
        // Recycle the per-run workspace and submission directories once the
        // result has been persisted. They are transient intermediates and
        // would otherwise accumulate indefinitely under .a3s/bench/. The
        // submission tree is sealed read-only, so a plain `remove_dir_all`
        // would silently fail to reclaim it; `remove_tree` restores
        // writability first.
        if let Some(path) = self.workspace.take() {
            let _ = crate::state_fs::remove_tree(&path);
        }
        if let Some(path) = self.seed_workspace.take() {
            let _ = crate::state_fs::remove_tree(&path);
        }
        if let Some(path) = self.submission.take() {
            let _ = crate::state_fs::remove_tree(&path);
        }
    }
}

pub struct CompletedRun {
    pub record: crate::result_record::LocalResultRecord,
}

pub fn execute(args: &[String]) -> Result<u8> {
    let options = run_input::RunOptions::parse(args)?;
    execute_options(options, true)?;
    Ok(0)
}

pub fn execute_options(options: run_input::RunOptions, emit_result: bool) -> Result<CompletedRun> {
    let state_root = workspace::state_root()?;
    let mut journal =
        crate::run_journal::RunJournal::begin(&state_root, &options.task, &options.agent)?;
    match execute_inner(&options, &state_root, &mut journal, emit_result) {
        Ok(completed) => Ok(completed),
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
    emit_result: bool,
) -> Result<CompletedRun> {
    use crate::run_journal::RunStage;

    let config = config::discover(&std::env::current_dir()?)?;
    if let (Some(path), Some(model)) = (config.path.as_deref(), config.judge_model.as_deref()) {
        config::resolve_model_route(path, model)?;
    }
    let status = runtime::preflight(&config.runtime)?;
    journal.advance(RunStage::RuntimeReady)?;
    let mut loaded = options.load(
        state_root,
        &journal.run_id,
        config.judge_model.clone(),
        config.codex_reasoning_effort.clone(),
        &status.provider,
    )?;
    journal.bind_locks(&loaded.task_lock_digest, &loaded.candidate_lock_digest)?;
    let judge_model = resolve_judge_model(&loaded.task, loaded.judge_model.as_deref(), &config)?;

    match status.provider.as_str() {
        "docker" => resolve_task_images(&mut loaded.task, &loaded.resolved_images)?,
        crate::os_runtime::PROVIDER => validate_os_runtime_task(
            &loaded.task,
            &loaded.candidate,
            loaded.model.as_deref(),
            &loaded.task_lock_schema,
            loaded.task_lock_resources,
        )?,
        provider => anyhow::bail!(
            "execution through configured Runtime {provider:?} is not implemented yet"
        ),
    }
    journal.advance(RunStage::InputsResolved)?;
    let game = start_game(&loaded.task, state_root, config.parallel_game_sessions)?;
    let mut transient = TransientRunDirs::new();
    let native_codex = loaded.candidate.protocol == asset::CandidateProtocol::CodexExec;
    let candidate_workspace = if native_codex {
        workspace::create_empty(&loaded.task)?
    } else {
        workspace::create(&loaded.task)?
    };
    transient.workspace = Some(candidate_workspace.clone());
    let seed_workspace = native_codex
        .then(|| requires_host_workspace(&loaded.task))
        .filter(|required| *required)
        .map(|_| workspace::create(&loaded.task))
        .transpose()?;
    transient.seed_workspace = seed_workspace.clone();
    let workspace_imports = if status.provider == crate::runtime_selection::DOCKER_PROVIDER {
        runtime::prepare_workspace_imports(&loaded.task)?
    } else {
        anyhow::ensure!(
            loaded.task.work_workspace_imports.is_empty(),
            "Task workspace imports require the Docker Runtime"
        );
        runtime::PreparedWorkspaceImports::default()
    };
    journal.advance(RunStage::CandidateRunning)?;
    let runtime_execution = RuntimeExecution {
        provider: &status.provider,
        resolved_images: &loaded.resolved_images,
    };
    let candidate_run = execute_candidate(
        &loaded.task,
        &loaded.candidate,
        loaded.model.as_deref(),
        loaded.reasoning_effort.as_deref(),
        loaded.codex_package.as_ref(),
        &config,
        &candidate_workspace,
        seed_workspace.as_deref(),
        &workspace_imports,
        game.as_ref(),
        &runtime_execution,
        &journal.run_id,
        state_root,
    )?;
    drop(workspace_imports);
    journal.advance(RunStage::CandidateCompleted)?;
    let submission = workspace::create_submission(&loaded.task, &candidate_workspace)?;
    transient.submission = Some(submission.clone());
    journal.advance(RunStage::Judging)?;
    let judge_result = execute_judge(
        &loaded.task,
        &loaded.judge,
        &submission,
        game.as_ref(),
        judge_model.as_ref().map(|model| &model.route),
        &status.provider,
        &loaded.resolved_images,
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
            candidate_execution: &candidate_run.execution,
            model_usage: candidate_run.model_usage.as_ref(),
            candidate_event_log_digest: candidate_run.candidate_event_log_digest.as_deref(),
            primary_metric: &primary.name,
            score,
            judge_result: &judge_result,
        },
    )?;
    journal.complete(&path, &record.result_digest)?;
    crate::result_record::LocalResultRecord::publish_latest(state_root, &record.run_id)?;
    if emit_result {
        print_result(
            options,
            &loaded.task.id,
            score,
            &record.run_id,
            &path,
            &candidate_run.execution,
        )?;
    }
    Ok(CompletedRun { record })
}

fn requires_host_workspace(task: &task::TaskInfo) -> bool {
    task.root.join("public/workspace").is_dir()
        || task
            .workspace_seed
            .as_ref()
            .is_none_or(|seed| seed.image != task.work_image)
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

fn start_game(
    task: &task::TaskInfo,
    state_root: &Path,
    parallel_game_sessions: bool,
) -> Result<Option<game_judge::GameSession>> {
    match task.legacy_judge.as_ref() {
        Some(source) if source.mode == "game_server" => {
            Ok(Some(game_judge::GameSession::start_with_parallel_sessions(
                source,
                task.resources.judge,
                state_root,
                parallel_game_sessions,
            )?))
        }
        _ => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_candidate(
    task: &task::TaskInfo,
    candidate: &asset::LocalAssetPackage,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    codex_package: Option<&crate::codex_package::CachedCodexPackage>,
    config: &config::LocalConfig,
    candidate_workspace: &Path,
    candidate_seed_workspace: Option<&Path>,
    workspace_imports: &runtime::PreparedWorkspaceImports,
    game: Option<&game_judge::GameSession>,
    runtime_execution: &RuntimeExecution<'_>,
    run_id: &str,
    state_root: &Path,
) -> Result<CandidateRun> {
    if candidate.protocol == asset::CandidateProtocol::A3sCodeExec {
        anyhow::ensure!(
            game.is_none(),
            "A3S Code Candidate does not support game Tasks"
        );
        let config_path = config.path.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "A3S Code Candidate requires project-local or user-local .a3s/config.acl"
            )
        })?;
        let prompt = std::fs::read_to_string(task.root.join("public/prompt.md"))?;
        let instructions = std::fs::read_to_string(candidate.model_instructions_path()?)?;
        return Ok(
            match crate::a3s_code_candidate::execute(crate::a3s_code_candidate::A3sCodeRequest {
                workspace: candidate_workspace,
                config_path,
                instructions: &instructions,
                task_prompt: &prompt,
                model,
                workspace_source_path: task
                    .workspace_seed
                    .as_ref()
                    .map(|seed| seed.source_path.as_str()),
                public_internet: task.work_network_need == "public_internet",
                timeout_sec: task.candidate_timeout_sec,
            })? {
                crate::a3s_code_candidate::A3sCodeOutcome::Completed(model_usage) => CandidateRun {
                    execution: crate::result_record::CandidateExecution::completed(),
                    model_usage: Some(model_usage),
                    candidate_event_log_digest: None,
                },
                crate::a3s_code_candidate::A3sCodeOutcome::TimedOut => CandidateRun {
                    execution: crate::result_record::CandidateExecution::timed_out(
                        task.candidate_timeout_sec,
                    ),
                    model_usage: None,
                    candidate_event_log_digest: None,
                },
            },
        );
    }
    if candidate.protocol == asset::CandidateProtocol::CodexExec {
        anyhow::ensure!(
            runtime_execution.provider == "docker",
            "containerized Codex requires the Docker Runtime"
        );
        let codex_package = codex_package.ok_or_else(|| {
            anyhow::anyhow!(
                "containerized Codex Candidate is missing its verified package cache entry"
            )
        })?;
        let prompt = std::fs::read_to_string(task.root.join("public/prompt.md"))?;
        let instructions = std::fs::read_to_string(candidate.model_instructions_path()?)?;
        let log_dir = state_root.join("runs").join(run_id);
        crate::state_fs::secure_directory(&log_dir)?;
        let event_log = log_dir.join("codex-events.jsonl");
        let game_url = game.map(game_judge::GameSession::url);
        let request = crate::codex_candidate::CodexExecutionRequest {
            task,
            package: codex_package,
            workspace: candidate_workspace,
            seed_workspace: candidate_seed_workspace,
            workspace_imports,
            instructions: &instructions,
            task_prompt: &prompt,
            model,
            reasoning_effort,
            disable_auto_resume: config.disable_auto_resume,
            parallel_game_sessions: config.parallel_game_sessions,
            game_network: game.map(|session| {
                (
                    session.network(),
                    game_url
                        .as_deref()
                        .expect("game URL accompanies a game session"),
                )
            }),
            timeout_sec: task.candidate_timeout_sec,
            state_root,
            event_log: Some(&event_log),
        };
        let outcome = crate::codex_candidate::execute(request)?;
        let event_bytes =
            crate::state_fs::read_regular_file(&event_log, "Codex event log for result binding")?;
        let event_log_digest = crate::result_identity::digest_bytes(&event_bytes);
        return Ok(match outcome {
            crate::codex_candidate::CodexOutcome::Completed(model_usage) => CandidateRun {
                execution: crate::result_record::CandidateExecution::completed(),
                model_usage,
                candidate_event_log_digest: Some(event_log_digest.clone()),
            },
            crate::codex_candidate::CodexOutcome::TimedOut => CandidateRun {
                execution: crate::result_record::CandidateExecution::timed_out(
                    task.candidate_timeout_sec,
                ),
                model_usage: None,
                candidate_event_log_digest: Some(event_log_digest),
            },
        });
    }
    let Some(model) = model else {
        anyhow::ensure!(
            game.is_none(),
            "interactive game Tasks require a model-backed Candidate"
        );
        let execution = match runtime_execution.provider {
            "docker" => {
                match runtime::execute_docker_candidate(
                    task,
                    candidate,
                    candidate_workspace,
                    workspace_imports,
                )? {
                    runtime::CandidateProcessOutcome::Completed => {
                        crate::result_record::CandidateExecution::completed()
                    }
                    runtime::CandidateProcessOutcome::TimedOut => {
                        crate::result_record::CandidateExecution::timed_out(
                            task.candidate_timeout_sec,
                        )
                    }
                }
            }
            crate::os_runtime::PROVIDER => crate::os_runtime::execute_candidate(
                task,
                candidate,
                candidate_workspace,
                runtime_execution.resolved_images,
            )
            .map(|()| crate::result_record::CandidateExecution::completed())?,
            provider => anyhow::bail!("Candidate Runtime {provider:?} is not implemented"),
        };
        return Ok(CandidateRun {
            execution,
            model_usage: None,
            candidate_event_log_digest: None,
        });
    };
    anyhow::ensure!(
        runtime_execution.provider == "docker",
        "model-backed Candidates are not supported by Runtime {:?}",
        runtime_execution.provider
    );
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
    // Persist the full candidate conversation under the bench state root
    // so it can be inspected post-hoc (mirrors EdgeBench agent_output.txt).
    let log_dir = state_root.join("runs").join(run_id);
    std::fs::create_dir_all(&log_dir)?;
    let outcome = model_candidate::execute(model_candidate::ModelCandidateRequest {
        config_path,
        model,
        task_prompt: &prompt,
        candidate_instructions: &instructions,
        workspace: candidate_workspace,
        workspace_source_path: task
            .workspace_seed
            .as_ref()
            .map(|seed| seed.source_path.as_str()),
        work_image: &task.work_image,
        work_platform: task.work_platform.as_deref(),
        work_resources: task.resources.work,
        workspace_imports,
        game_network: game.map(|session| {
            (
                session.network(),
                game_url.as_deref().expect("game URL accompanies session"),
            )
        }),
        public_internet: task.work_network_need == "public_internet",
        timeout_sec: task.candidate_timeout_sec,
        max_tool_rounds: candidate.model_max_steps()?,
        parallel_game_sessions: config.parallel_game_sessions,
        log_dir: Some(&log_dir),
    })?;
    Ok(match outcome {
        model_candidate::ModelCandidateOutcome::Completed(model_usage) => CandidateRun {
            execution: crate::result_record::CandidateExecution::completed(),
            model_usage: Some(model_usage),
            candidate_event_log_digest: None,
        },
        model_candidate::ModelCandidateOutcome::TimedOut { timeout_sec } => CandidateRun {
            execution: crate::result_record::CandidateExecution::timed_out(timeout_sec),
            model_usage: None,
            candidate_event_log_digest: None,
        },
    })
}

fn execute_judge(
    task: &task::TaskInfo,
    judge: &asset::LocalAssetPackage,
    submission: &Path,
    game: Option<&game_judge::GameSession>,
    model: Option<&config::ModelRoute>,
    runtime_provider: &str,
    resolved_images: &std::collections::BTreeMap<String, String>,
) -> Result<runtime::JudgeResult> {
    if let (Some(session), Some(source)) = (game, &task.legacy_judge) {
        session.finish(task, source)
    } else if let Some(source) = &task.legacy_judge {
        legacy_judge::execute(task, source, submission, model)
    } else {
        match runtime_provider {
            "docker" => runtime::execute_docker_judge(task, judge, submission),
            crate::os_runtime::PROVIDER => {
                crate::os_runtime::execute_judge(task, judge, submission, resolved_images)
            }
            provider => anyhow::bail!("Judge Runtime {provider:?} is not implemented"),
        }
    }
}

fn validate_os_runtime_task(
    task: &task::TaskInfo,
    candidate: &asset::LocalAssetPackage,
    model: Option<&str>,
    task_lock_schema: &str,
    task_lock_resources: Option<task::TaskResources>,
) -> Result<()> {
    anyhow::ensure!(
        task.schema == "a3s-bench/task/v1",
        "os-runtime cannot enforce explicit a3s-bench/task/v2 resources; use the Docker Runtime"
    );
    anyhow::ensure!(
        task_lock_schema == "a3s.bench.task-lock.v1" && task_lock_resources.is_none(),
        "os-runtime requires task/v1 to be bound by TaskLock v1 without explicit resources"
    );
    anyhow::ensure!(
        candidate.protocol != asset::CandidateProtocol::CodexExec
            && (model.is_none() || candidate.protocol == asset::CandidateProtocol::A3sCodeExec),
        "containerized Codex and model-backed Candidates require the Docker Runtime"
    );
    anyhow::ensure!(
        task.legacy_judge.is_none(),
        "os-runtime does not support legacy/game Judges yet"
    );
    anyhow::ensure!(
        task.workspace_seed.is_none(),
        "os-runtime does not support OCI workspace seeds yet"
    );
    anyhow::ensure!(
        task.root.join("public/workspace").is_dir(),
        "os-runtime requires an embedded public/workspace"
    );
    Ok(())
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
    candidate_execution: &crate::result_record::CandidateExecution,
) -> Result<()> {
    if options.json {
        crate::output::print_success(
            "run",
            json!({
                "status":"completed", "governance_status":"local_unofficial",
                "run_id":run_id, "task_id":task_id, "score":score,
                "result_path":path,
                "candidate_execution":candidate_execution
            }),
        )?;
    } else {
        println!("COMPLETED  score={score}  task={task_id}");
        if let Some(timeout_sec) = candidate_execution
            .is_timed_out()
            .then_some(candidate_execution.timeout_sec)
            .flatten()
        {
            println!("candidate: timed_out ({timeout_sec}s)");
        }
        println!("run:    {run_id}");
        println!("result: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
