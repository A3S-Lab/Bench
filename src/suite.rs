use a3s_acl::{Block, Value};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SUITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateSpec {
    agent: String,
    model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuiteSpec {
    id: String,
    tasks: Vec<String>,
    baseline: CandidateSpec,
    candidate: CandidateSpec,
    digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteMemberState {
    task_reference: String,
    baseline_run_id: Option<String>,
    candidate_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteRunState {
    schema: String,
    suite_run_id: String,
    suite_id: String,
    spec_digest: String,
    members: Vec<SuiteMemberState>,
}

pub fn execute(args: &[String]) -> Result<u8> {
    anyhow::ensure!(
        args.first().map(String::as_str) == Some("run"),
        "usage: suite run <suite.acl> [--resume <suite-run-id>] [--json]"
    );
    let (source, resume, json) = parse_run_args(&args[1..])?;
    let spec = load_spec(Path::new(&source))?;
    let state_root = crate::workspace::state_root()?;
    let (suite_root, mut state) = match resume {
        Some(run_id) => resume_state(&state_root, &run_id, &spec)?,
        None => plan_suite(&state_root, &spec)?,
    };
    if !json {
        println!("suite:  {}", state.suite_run_id);
    }
    let baseline_lock = suite_root.join("baseline.candidate-lock.json");
    let candidate_lock = suite_root.join("candidate.candidate-lock.json");
    for index in 0..state.members.len() {
        let task_lock = suite_root.join(format!("task-{index}.lock.json"));
        if state.members[index].baseline_run_id.is_none() {
            let completed = crate::bench_run::execute_options(
                crate::run_input::RunOptions::locked(&task_lock, &baseline_lock),
                false,
            )
            .with_context(|| {
                format!(
                    "suite {} baseline member {index} failed; resume with --resume {}",
                    state.suite_id, state.suite_run_id
                )
            })?;
            state.members[index].baseline_run_id = Some(completed.record.run_id);
            persist_state(&suite_root, &state)?;
        }
        if state.members[index].candidate_run_id.is_none() {
            let completed = crate::bench_run::execute_options(
                crate::run_input::RunOptions::locked(&task_lock, &candidate_lock),
                false,
            )
            .with_context(|| {
                format!(
                    "suite {} candidate member {index} failed; resume with --resume {}",
                    state.suite_id, state.suite_run_id
                )
            })?;
            state.members[index].candidate_run_id = Some(completed.record.run_id);
            persist_state(&suite_root, &state)?;
        }
    }
    let pairs = load_pairs(&state_root, &state)?;
    let summary = crate::comparison::compare_pairs(&pairs)?;
    if json {
        crate::output::print_success(
            "suite run",
            serde_json::json!({
                "suite_run_id": state.suite_run_id,
                "suite_id": state.suite_id,
                "comparison": summary,
            }),
        )?;
    } else {
        println!(
            "COMPLETED  pairs={} candidate_wins={} ties={} baseline_wins={}",
            summary.pair_count, summary.candidate_wins, summary.ties, summary.baseline_wins
        );
    }
    Ok(0)
}

fn parse_run_args(args: &[String]) -> Result<(String, Option<String>, bool)> {
    anyhow::ensure!(!args.is_empty(), "suite run requires one suite.acl");
    let source = args[0].clone();
    let mut resume = None;
    let mut json = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--resume" if resume.is_none() && index + 1 < args.len() => {
                resume = Some(args[index + 1].clone());
                index += 2;
            }
            "--json" if !json => {
                json = true;
                index += 1;
            }
            value => anyhow::bail!("invalid or duplicate suite run option {value:?}"),
        }
    }
    Ok((source, resume, json))
}

fn load_spec(path: &Path) -> Result<SuiteSpec> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("suite source is unavailable: {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "suite source must be a real regular file"
    );
    let source = std::fs::read_to_string(path)?;
    let document = a3s_acl::parse(&source)
        .map_err(|error| anyhow::anyhow!("invalid {}: {error}", path.display()))?;
    anyhow::ensure!(
        document.blocks.len() == 1,
        "suite ACL must have one root block"
    );
    let root = &document.blocks[0];
    validate_schema(root)?;
    anyhow::ensure!(
        string(root, "schema")? == "a3s-bench/suite/v1",
        "unsupported suite schema"
    );
    let tasks = string_list(root, "tasks")?;
    anyhow::ensure!(!tasks.is_empty(), "suite tasks must not be empty");
    anyhow::ensure!(tasks.len() <= 128, "suite may contain at most 128 tasks");
    let baseline = candidate(root, "baseline")?;
    let candidate = candidate(root, "candidate")?;
    let id = root.labels[0].clone();
    let digest = spec_digest(&id, &tasks, &baseline, &candidate)?;
    Ok(SuiteSpec {
        id,
        tasks,
        baseline,
        candidate,
        digest,
    })
}

fn validate_schema(root: &Block) -> Result<()> {
    use crate::acl_schema::{validate_block, BlockSchema, Labels};
    validate_block(
        root,
        "bench_suite",
        BlockSchema {
            attributes: &["schema", "tasks"],
            children: &["candidate"],
            labels: Labels::Exactly(1),
        },
    )?;
    anyhow::ensure!(root.name == "bench_suite", "suite root must be bench_suite");
    anyhow::ensure!(root.blocks.len() == 2, "suite must define two candidates");
    for block in &root.blocks {
        validate_block(
            block,
            "bench_suite.candidate",
            BlockSchema {
                attributes: &["agent", "model"],
                children: &[],
                labels: Labels::Exactly(1),
            },
        )?;
    }
    Ok(())
}

fn candidate(root: &Block, role: &str) -> Result<CandidateSpec> {
    let matches = root
        .blocks
        .iter()
        .filter(|block| block.labels.first().map(String::as_str) == Some(role))
        .collect::<Vec<_>>();
    anyhow::ensure!(matches.len() == 1, "suite requires one candidate {role:?}");
    let block = matches[0];
    Ok(CandidateSpec {
        agent: string(block, "agent")?.to_owned(),
        model: block
            .attributes
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn string<'a>(block: &'a Block, name: &str) -> Result<&'a str> {
    block
        .attributes
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{}.{} must be a string", block.name, name))
}

fn string_list(block: &Block, name: &str) -> Result<Vec<String>> {
    let Some(Value::List(items)) = block.attributes.get(name) else {
        anyhow::bail!("{}.{} must be a list", block.name, name);
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("{}.{} items must be strings", block.name, name))
        })
        .collect()
}

fn spec_digest(
    id: &str,
    tasks: &[String],
    baseline: &CandidateSpec,
    candidate: &CandidateSpec,
) -> Result<String> {
    let value = serde_json::json!({
        "id": id,
        "tasks": tasks,
        "baseline": {"agent": baseline.agent, "model": baseline.model},
        "candidate": {"agent": candidate.agent, "model": candidate.model},
    });
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&value)?)
    ))
}

fn plan_suite(state_root: &Path, spec: &SuiteSpec) -> Result<(PathBuf, SuiteRunState)> {
    let suites = state_root.join("suites");
    crate::state_fs::secure_directory(&suites)?;
    let staging = crate::state_fs::create_unique_staging_directory(&suites, "suite")?;
    let run_id = suite_run_id()?;
    let destination = suites.join(&run_id);
    let plan = (|| -> Result<SuiteRunState> {
        crate::lock::create_candidate(
            &spec.baseline.agent,
            spec.baseline.model.clone(),
            state_root,
            &staging.join("baseline.candidate-lock.json"),
        )?;
        crate::lock::create_candidate(
            &spec.candidate.agent,
            spec.candidate.model.clone(),
            state_root,
            &staging.join("candidate.candidate-lock.json"),
        )?;
        let config = crate::config::discover(&std::env::current_dir()?)?;
        let runtime = crate::runtime::preflight(&config.runtime)?;
        for (index, task) in spec.tasks.iter().enumerate() {
            let source = crate::catalog::resolve_task_reference(task)?;
            crate::lock::create_task_with_provider(
                &source,
                config.judge_model.clone(),
                state_root,
                &staging.join(format!("task-{index}.lock.json")),
                &runtime.provider,
            )?;
        }
        let state = SuiteRunState {
            schema: "a3s.bench.suite-run.v1".into(),
            suite_run_id: run_id.clone(),
            suite_id: spec.id.clone(),
            spec_digest: spec.digest.clone(),
            members: spec
                .tasks
                .iter()
                .map(|task| SuiteMemberState {
                    task_reference: task.clone(),
                    baseline_run_id: None,
                    candidate_run_id: None,
                })
                .collect(),
        };
        persist_state(&staging, &state)?;
        crate::state_fs::sync_tree(&staging)?;
        Ok(state)
    })();
    let state = match plan {
        Ok(state) => state,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    std::fs::rename(&staging, &destination)?;
    Ok((destination, state))
}

fn resume_state(
    state_root: &Path,
    run_id: &str,
    spec: &SuiteSpec,
) -> Result<(PathBuf, SuiteRunState)> {
    validate_suite_run_id(run_id)?;
    let root = state_root.join("suites").join(run_id);
    let bytes = crate::state_fs::read_regular_file(&root.join("state.json"), "suite state")?;
    let state: SuiteRunState = serde_json::from_slice(&bytes)?;
    validate_state(&state, run_id, spec)?;
    Ok((root, state))
}

fn validate_state(state: &SuiteRunState, run_id: &str, spec: &SuiteSpec) -> Result<()> {
    anyhow::ensure!(
        state.schema == "a3s.bench.suite-run.v1",
        "unsupported suite state"
    );
    anyhow::ensure!(
        state.suite_run_id == run_id,
        "suite state identity mismatch"
    );
    anyhow::ensure!(
        state.suite_id == spec.id && state.spec_digest == spec.digest,
        "suite source changed since the run was planned"
    );
    anyhow::ensure!(
        state.members.len() == spec.tasks.len(),
        "suite member count changed"
    );
    for (member, task) in state.members.iter().zip(&spec.tasks) {
        anyhow::ensure!(member.task_reference == *task, "suite task order changed");
        for run in [
            member.baseline_run_id.as_deref(),
            member.candidate_run_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            crate::run_journal::validate_run_id(run)?;
        }
    }
    Ok(())
}

fn persist_state(root: &Path, state: &SuiteRunState) -> Result<()> {
    crate::state_fs::secure_atomic_write(
        &root.join("state.json"),
        &serde_json::to_vec_pretty(state)?,
    )
}

fn load_pairs(
    state_root: &Path,
    state: &SuiteRunState,
) -> Result<
    Vec<(
        crate::result_record::LocalResultRecord,
        crate::result_record::LocalResultRecord,
    )>,
> {
    state
        .members
        .iter()
        .map(|member| {
            let baseline_id = member
                .baseline_run_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("suite baseline result is incomplete"))?;
            let candidate_id = member
                .candidate_run_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("suite candidate result is incomplete"))?;
            let baseline = crate::result_record::LocalResultRecord::load(state_root, baseline_id)?
                .ok_or_else(|| anyhow::anyhow!("suite baseline result is unavailable"))?;
            let candidate =
                crate::result_record::LocalResultRecord::load(state_root, candidate_id)?
                    .ok_or_else(|| anyhow::anyhow!("suite candidate result is unavailable"))?;
            Ok((baseline, candidate))
        })
        .collect()
}

fn suite_run_id() -> Result<String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let sequence = SUITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(format!("suite-{millis}-{}-{sequence}", std::process::id()))
}

fn validate_suite_run_id(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.starts_with("suite-") && value.len() <= 128,
        "invalid suite run ID"
    );
    anyhow::ensure!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "invalid suite run ID"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_spec(source: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), source).unwrap();
        file
    }

    #[test]
    fn parses_closed_two_candidate_suite() {
        let file = write_spec(
            r#"bench_suite "core" {
          schema = "a3s-bench/suite/v1"
          tasks = ["quick_file_edit", "./tasks/local"]
          candidate "baseline" { agent = "a3s-code-core" model = "openai/base" }
          candidate "candidate" { agent = "a3s-code-core" model = "openai/new" }
        }"#,
        );
        let spec = load_spec(file.path()).unwrap();
        assert_eq!(spec.id, "core");
        assert_eq!(spec.tasks.len(), 2);
        assert_eq!(spec.baseline.model.as_deref(), Some("openai/base"));
        assert!(spec.digest.starts_with("sha256:"));
    }

    #[test]
    fn rejects_unknown_fields_missing_roles_and_duplicate_options() {
        for source in [
            r#"bench_suite "core" { schema = "a3s-bench/suite/v1" tasks = ["x"] typo = true }"#,
            r#"bench_suite "core" { schema = "a3s-bench/suite/v1" tasks = ["x"] candidate "baseline" { agent = "a" } candidate "baseline" { agent = "b" } }"#,
        ] {
            assert!(load_spec(write_spec(source).path()).is_err());
        }
        assert!(parse_run_args(&["suite.acl".into(), "--json".into(), "--json".into()]).is_err());
    }

    #[test]
    fn resume_state_is_bound_to_the_exact_spec() {
        let spec = SuiteSpec {
            id: "core".into(),
            tasks: vec!["task".into()],
            baseline: CandidateSpec {
                agent: "a".into(),
                model: None,
            },
            candidate: CandidateSpec {
                agent: "b".into(),
                model: None,
            },
            digest: "sha256:spec".into(),
        };
        let state = SuiteRunState {
            schema: "a3s.bench.suite-run.v1".into(),
            suite_run_id: "suite-1".into(),
            suite_id: "core".into(),
            spec_digest: "sha256:spec".into(),
            members: vec![SuiteMemberState {
                task_reference: "task".into(),
                baseline_run_id: None,
                candidate_run_id: None,
            }],
        };
        validate_state(&state, "suite-1", &spec).unwrap();
        let mut changed = spec;
        changed.digest = "sha256:changed".into();
        assert!(validate_state(&state, "suite-1", &changed).is_err());
    }
}
