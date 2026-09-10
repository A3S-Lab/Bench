use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLock {
    pub schema: String,
    pub lock_digest: String,
    pub task_revision: String,
    pub artifact_digest: String,
    pub judge_revision: String,
    pub judge_artifact_digest: String,
    pub judge_model: Option<String>,
    pub resolved_images: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<crate::task::TaskResources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_imports: Option<Vec<crate::task::WorkWorkspaceImport>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_allow_hosts: Option<Vec<String>>,
}

pub struct LoadedTaskLock {
    pub lock: TaskLock,
    pub task_artifact: PathBuf,
    pub judge_artifact: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateLock {
    pub schema: String,
    pub lock_digest: String,
    pub candidate_revision: String,
    pub artifact_digest: String,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<CandidateProductLock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateProductLock {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_triple: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_set_digest: Option<String>,
}

pub fn create_task_with_provider(
    source: &Path,
    judge_model: Option<String>,
    state_root: &Path,
    output: &Path,
    runtime_provider: &str,
) -> Result<TaskLock> {
    let root = if source.is_dir() {
        source
    } else {
        source.parent().unwrap_or_else(|| Path::new("."))
    };
    let digest = crate::task_snapshot::capture(root, state_root)?;
    let captured = crate::task_snapshot::artifact_path(state_root, &digest)?;
    let task = crate::task::load_local(&captured)?;
    anyhow::ensure!(
        runtime_provider != crate::os_runtime::PROVIDER || task.schema == "a3s-bench/task/v1",
        "os-runtime cannot enforce explicit a3s-bench/task/v2 resources; use the Docker Runtime"
    );
    let requires_judge_model = task
        .legacy_judge
        .as_ref()
        .is_some_and(|source| source.requires_model_gateway);
    anyhow::ensure!(
        !requires_judge_model || judge_model.is_some(),
        "Task {:?} requires bench.judge_model in .a3s/config.acl",
        task.id
    );
    let judge_model = if requires_judge_model {
        judge_model
    } else {
        None
    };
    if let Some(model) = judge_model.as_deref() {
        crate::config::validate_model_reference(model)?;
    }
    let judge = resolve_judge(&task, state_root)?;
    let judge_artifact_digest = crate::task_snapshot::capture(&judge.root, state_root)?;
    let judge_artifact = crate::task_snapshot::artifact_path(state_root, &judge_artifact_digest)?;
    let locked_judge = crate::asset::load_local(&judge_artifact)?;
    let mut resolved_images = BTreeMap::new();
    if runtime_provider == crate::os_runtime::PROVIDER {
        resolved_images.extend(crate::os_runtime::resolved_runner_images()?);
    } else {
        for (reference, platform) in task_image_references(&task) {
            let resolved = crate::runtime::resolve_image(reference, platform)?;
            resolved_images.insert(image_key(reference, platform), resolved.immutable_ref);
        }
    }
    let (lock_schema, resources, workspace_imports, network_allow_hosts) =
        match task.schema.as_str() {
            "a3s-bench/task/v1" => ("a3s.bench.task-lock.v1", None, None, None),
            "a3s-bench/task/v2" => (
                "a3s.bench.task-lock.v2",
                Some(task.resources),
                Some(task.work_workspace_imports.clone()),
                (!task.work_network_allow_hosts.is_empty())
                    .then(|| task.work_network_allow_hosts.clone()),
            ),
            _ => unreachable!("Task schema was validated"),
        };
    let mut value = TaskLock {
        schema: lock_schema.into(),
        lock_digest: String::new(),
        task_revision: digest.clone(),
        artifact_digest: digest,
        judge_revision: locked_judge.identity,
        judge_artifact_digest,
        judge_model,
        resolved_images,
        resources,
        workspace_imports,
        network_allow_hosts,
    };
    value.lock_digest = crate::lock_identity::task(&value)?;
    write_exclusive(output, &serde_json::to_vec_pretty(&value)?)?;
    Ok(value)
}

fn resolve_judge(
    task: &crate::task::TaskInfo,
    state_root: &Path,
) -> Result<crate::asset::LocalAssetPackage> {
    if task.judge_asset.starts_with("oci://") {
        crate::asset::resolve(&task.judge_asset, state_root)
    } else {
        crate::asset::load_local(&task.root.join(&task.judge_asset))
    }
}

fn task_image_references(task: &crate::task::TaskInfo) -> Vec<(&str, Option<&str>)> {
    let mut values = vec![(task.work_image.as_str(), task.work_platform.as_deref())];
    if let Some(seed) = &task.workspace_seed {
        values.push((seed.image.as_str(), seed.platform.as_deref()));
    }
    if let Some(judge) = &task.legacy_judge {
        values.push((judge.image.as_str(), judge.platform.as_deref()));
    }
    values.sort_unstable();
    values.dedup();
    values
}

pub fn image_key(reference: &str, platform: Option<&str>) -> String {
    format!("{}|{}", platform.unwrap_or("native"), reference)
}

#[allow(dead_code)]
pub fn create_candidate(
    reference: &str,
    model: Option<String>,
    state_root: &Path,
    output: &Path,
) -> Result<CandidateLock> {
    create_candidate_with_options(reference, model, None, state_root, output)
}

pub fn create_candidate_with_options(
    reference: &str,
    model: Option<String>,
    reasoning_effort: Option<String>,
    state_root: &Path,
    output: &Path,
) -> Result<CandidateLock> {
    create_candidate_with_codex_default(
        reference,
        model,
        reasoning_effort,
        None,
        state_root,
        output,
    )
}

pub fn create_candidate_with_codex_default(
    reference: &str,
    model: Option<String>,
    reasoning_effort: Option<String>,
    codex_default_reasoning_effort: Option<String>,
    state_root: &Path,
    output: &Path,
) -> Result<CandidateLock> {
    let asset = crate::asset::resolve(reference, state_root)?;
    let digest = crate::task_snapshot::capture(&asset.root, state_root)?;
    let captured = crate::task_snapshot::artifact_path(state_root, &digest)?;
    let locked_asset = crate::asset::load_local(&captured)?;
    if let Some(effort) = codex_default_reasoning_effort.as_deref() {
        validate_reasoning_effort(effort)?;
    }
    if let Some(effort) = reasoning_effort.as_deref() {
        validate_reasoning_effort(effort)?;
        anyhow::ensure!(
            locked_asset.protocol == crate::asset::CandidateProtocol::CodexExec,
            "reasoning effort is supported only by the native Codex Candidate"
        );
    }
    let reasoning_effort = if locked_asset.protocol == crate::asset::CandidateProtocol::CodexExec {
        reasoning_effort.or(codex_default_reasoning_effort)
    } else {
        reasoning_effort
    };
    validate_model_reasoning_compatibility(model.as_deref(), reasoning_effort.as_deref())?;
    if locked_asset.protocol == crate::asset::CandidateProtocol::CodexExec {
        if let Some(model) = model.as_deref() {
            validate_codex_model(model)?;
        }
    }
    if model.is_some() && locked_asset.protocol == crate::asset::CandidateProtocol::AgentTool {
        locked_asset
            .model_instructions_path()
            .context("Candidate cannot be locked with a model")?;
        locked_asset
            .model_max_steps()
            .context("Candidate cannot be locked with a model")?;
    }
    let product = match locked_asset.protocol {
        crate::asset::CandidateProtocol::AgentTool => None,
        crate::asset::CandidateProtocol::A3sCodeExec => Some(CandidateProductLock {
            name: "a3s-cli".into(),
            version: crate::a3s_code_candidate::version()?,
            target_triple: None,
            artifact_set_digest: None,
        }),
        crate::asset::CandidateProtocol::CodexExec => {
            let package = crate::codex_package::prepare(state_root, None)?;
            let artifact_set_digest = package.artifact_set_digest().to_owned();
            Some(CandidateProductLock {
                name: "codex-cli".into(),
                version: package.reported_version,
                target_triple: Some(package.manifest.target_triple),
                artifact_set_digest: Some(artifact_set_digest),
            })
        }
    };
    let mut value = CandidateLock {
        schema: match locked_asset.protocol {
            crate::asset::CandidateProtocol::AgentTool => "a3s.bench.candidate-lock.v1",
            crate::asset::CandidateProtocol::A3sCodeExec => "a3s.bench.candidate-lock.v2",
            crate::asset::CandidateProtocol::CodexExec => "a3s.bench.candidate-lock.v3",
        }
        .into(),
        lock_digest: String::new(),
        candidate_revision: locked_asset.identity,
        artifact_digest: digest,
        model,
        reasoning_effort,
        product,
    };
    value.lock_digest = crate::lock_identity::candidate(&value)?;
    write_exclusive(output, &serde_json::to_vec_pretty(&value)?)?;
    Ok(value)
}

pub fn load_task(path: &Path, state_root: &Path) -> Result<LoadedTaskLock> {
    let value: TaskLock = serde_json::from_slice(&read_lock_file(path)?)?;
    anyhow::ensure!(
        matches!(
            value.schema.as_str(),
            "a3s.bench.task-lock.v1" | "a3s.bench.task-lock.v2"
        ),
        "invalid TaskLock schema"
    );
    crate::lock_identity::validate_digest(&value.lock_digest)?;
    anyhow::ensure!(
        crate::lock_identity::task(&value)? == value.lock_digest,
        "TaskLock semantic digest mismatch"
    );
    anyhow::ensure!(
        value.task_revision == value.artifact_digest,
        "TaskLock revision does not match artifact digest"
    );
    anyhow::ensure!(
        !value.judge_revision.trim().is_empty(),
        "TaskLock Judge revision is empty"
    );
    let artifact = crate::task_snapshot::artifact_path(state_root, &value.artifact_digest)?;
    crate::task_snapshot::verify(&artifact, &value.artifact_digest)
        .context("locked Task artifact is unavailable or corrupt")?;
    let task = crate::task::load_local(&artifact).context("locked Task artifact is invalid")?;
    match (value.schema.as_str(), task.schema.as_str()) {
        ("a3s.bench.task-lock.v1", "a3s-bench/task/v1") => anyhow::ensure!(
            value.resources.is_none()
                && value.workspace_imports.is_none()
                && value.network_allow_hosts.is_none(),
            "TaskLock v1 must not contain resources, workspace imports, or network allow hosts"
        ),
        ("a3s.bench.task-lock.v2", "a3s-bench/task/v2") => {
            anyhow::ensure!(
                value.resources == Some(task.resources),
                "TaskLock resources do not match the locked Task artifact"
            );
            anyhow::ensure!(
                value.workspace_imports.as_ref() == Some(&task.work_workspace_imports),
                "TaskLock workspace imports do not match the locked Task artifact"
            );
            anyhow::ensure!(
                value
                    .network_allow_hosts
                    .as_deref()
                    .is_none_or(|hosts| !hosts.is_empty()),
                "TaskLock network allow hosts must omit an empty list"
            );
            anyhow::ensure!(
                value.network_allow_hosts.as_deref().unwrap_or_default()
                    == task.work_network_allow_hosts,
                "TaskLock network allow hosts do not match the locked Task artifact"
            );
        }
        ("a3s.bench.task-lock.v1", "a3s-bench/task/v2") => {
            anyhow::bail!("TaskLock v1 cannot bind a task/v2 artifact")
        }
        ("a3s.bench.task-lock.v2", "a3s-bench/task/v1") => {
            anyhow::bail!("TaskLock v2 cannot bind a task/v1 artifact")
        }
        _ => unreachable!("Task and TaskLock schemas were validated"),
    }
    let requires_judge_model = task
        .legacy_judge
        .as_ref()
        .is_some_and(|source| source.requires_model_gateway);
    anyhow::ensure!(
        requires_judge_model == value.judge_model.is_some(),
        "TaskLock Judge model binding does not match Task requirements"
    );
    if let Some(model) = value.judge_model.as_deref() {
        crate::config::validate_model_reference(model)?;
    }
    let judge_artifact =
        crate::task_snapshot::artifact_path(state_root, &value.judge_artifact_digest)?;
    crate::task_snapshot::verify(&judge_artifact, &value.judge_artifact_digest)
        .context("locked Judge artifact is unavailable or corrupt")?;
    let judge = crate::asset::load_local(&judge_artifact)
        .context("locked Judge artifact is not an Asset package")?;
    anyhow::ensure!(
        judge.identity == value.judge_revision,
        "TaskLock Judge revision does not match artifact"
    );
    Ok(LoadedTaskLock {
        lock: value,
        task_artifact: artifact,
        judge_artifact,
    })
}

pub fn load_candidate(path: &Path, state_root: &Path) -> Result<(CandidateLock, PathBuf)> {
    let value: CandidateLock = serde_json::from_slice(&read_lock_file(path)?)?;
    anyhow::ensure!(
        matches!(
            value.schema.as_str(),
            "a3s.bench.candidate-lock.v1"
                | "a3s.bench.candidate-lock.v2"
                | "a3s.bench.candidate-lock.v3"
        ),
        "invalid CandidateLock schema"
    );
    crate::lock_identity::validate_digest(&value.lock_digest)?;
    anyhow::ensure!(
        crate::lock_identity::candidate(&value)? == value.lock_digest,
        "CandidateLock semantic digest mismatch"
    );
    if value.schema == "a3s.bench.candidate-lock.v2"
        && value
            .product
            .as_ref()
            .is_some_and(|product| product.name == "codex-cli")
    {
        anyhow::bail!(
            "legacy native Codex CandidateLock v2 is not a containerized lock; regenerate the CandidateLock"
        );
    }

    if value.schema == "a3s.bench.candidate-lock.v3" {
        if let Some(model) = value.model.as_deref() {
            validate_codex_model(model)?;
        }
        if let Some(reasoning_effort) = value.reasoning_effort.as_deref() {
            validate_reasoning_effort(reasoning_effort)?;
        }
        validate_model_reasoning_compatibility(
            value.model.as_deref(),
            value.reasoning_effort.as_deref(),
        )?;
    }
    anyhow::ensure!(
        !value.candidate_revision.trim().is_empty(),
        "CandidateLock revision is empty"
    );
    let artifact = crate::task_snapshot::artifact_path(state_root, &value.artifact_digest)?;
    crate::task_snapshot::verify(&artifact, &value.artifact_digest)
        .context("locked Candidate artifact is unavailable or corrupt")?;
    let candidate = crate::asset::load_local(&artifact)
        .context("locked Candidate artifact is not a Candidate adapter")?;
    anyhow::ensure!(
        candidate.identity == value.candidate_revision,
        "CandidateLock revision does not match artifact"
    );
    match (candidate.protocol, value.product.as_ref()) {
        (crate::asset::CandidateProtocol::AgentTool, None) => {
            anyhow::ensure!(
                value.schema == "a3s.bench.candidate-lock.v1",
                "Agent Candidate uses the historical CandidateLock v1 schema"
            );
            anyhow::ensure!(
                value.reasoning_effort.is_none(),
                "Agent Candidate lock contains a Codex-only reasoning effort"
            );
        }
        (crate::asset::CandidateProtocol::A3sCodeExec, Some(product)) => {
            anyhow::ensure!(
                value.schema == "a3s.bench.candidate-lock.v2" && product.name == "a3s-cli",
                "A3S Code Candidate has an invalid product lock"
            );
            anyhow::ensure!(
                value.reasoning_effort.is_none()
                    && product.target_triple.is_none()
                    && product.artifact_set_digest.is_none(),
                "A3S Code Candidate lock contains containerized Codex fields"
            );
            anyhow::ensure!(
                crate::a3s_code_candidate::version()? == product.version,
                "installed A3S CLI does not match locked version {:?}",
                product.version
            );
        }
        (crate::asset::CandidateProtocol::CodexExec, Some(product)) => {
            anyhow::ensure!(
                product.name == "codex-cli",
                "Codex Candidate has an invalid product lock"
            );
            if product.target_triple.is_none() || product.artifact_set_digest.is_none() {
                anyhow::bail!(
                    "legacy native Codex CandidateLock v2 is not a containerized lock; regenerate the CandidateLock"
                );
            }
            anyhow::ensure!(
                value.schema == "a3s.bench.candidate-lock.v3",
                "Codex Candidate has an invalid product lock schema"
            );
            crate::codex_package::load_cached(state_root, product, None)?;
        }
        _ => anyhow::bail!("Candidate protocol does not match its product lock"),
    }
    Ok((value, artifact))
}

pub fn validate_reasoning_effort(value: &str) -> Result<()> {
    anyhow::ensure!(
        matches!(
            value,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
        ),
        "reasoning effort must be one of none, minimal, low, medium, high, or xhigh"
    );
    Ok(())
}

pub fn validate_model_reasoning_compatibility(
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<()> {
    if model == Some("gpt-5.3-codex-spark") {
        anyhow::ensure!(
            matches!(reasoning_effort, Some("low" | "medium" | "high" | "xhigh")),
            "gpt-5.3-codex-spark requires reasoning effort low, medium, high, or xhigh"
        );
    }
    Ok(())
}

pub fn validate_codex_model(value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 256
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/')
            })
            && !value.starts_with('/')
            && !value.ends_with('/')
            && !value.contains("//"),
        "Codex model must contain only ASCII letters, digits, '.', '_', '-', and '/'"
    );
    Ok(())
}

fn read_lock_file(path: &Path) -> Result<Vec<u8>> {
    crate::state_fs::read_regular_file(path, "lock")
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("could not create lock {}", path.display()))?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    fn write_candidate_lock(path: &Path, mut value: CandidateLock) {
        value.lock_digest = crate::lock_identity::candidate(&value).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    fn legacy_v2_candidate_lock() -> CandidateLock {
        CandidateLock {
            schema: "a3s.bench.candidate-lock.v2".into(),
            lock_digest: String::new(),
            candidate_revision: format!("sha256:{}", "a".repeat(64)),
            artifact_digest: format!("sha256:{}", "b".repeat(64)),
            model: Some("gpt-5.3-codex-spark".into()),
            reasoning_effort: None,
            product: Some(CandidateProductLock {
                name: "codex-cli".into(),
                version: "codex-cli 0.147.0".into(),
                target_triple: None,
                artifact_set_digest: None,
            }),
        }
    }

    fn v3_candidate_lock(model: &str, reasoning_effort: &str) -> CandidateLock {
        CandidateLock {
            schema: "a3s.bench.candidate-lock.v3".into(),
            lock_digest: String::new(),
            candidate_revision: format!("sha256:{}", "a".repeat(64)),
            artifact_digest: format!("sha256:{}", "b".repeat(64)),
            model: Some(model.into()),
            reasoning_effort: Some(reasoning_effort.into()),
            product: Some(CandidateProductLock {
                name: "codex-cli".into(),
                version: "codex-cli 0.147.0".into(),
                target_triple: Some("x86_64-unknown-linux-musl".into()),
                artifact_set_digest: Some(format!("sha256:{}", "c".repeat(64))),
            }),
        }
    }

    #[test]
    fn candidate_loader_rejects_absent_artifact_v2_with_regenerate_error() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("candidate-v2.lock.json");
        write_candidate_lock(&path, legacy_v2_candidate_lock());

        let error = load_candidate(&path, state.path()).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("regenerate the CandidateLock"),
            "{message}"
        );
        assert!(
            !message.contains("locked Candidate artifact is unavailable or corrupt"),
            "{message}"
        );
    }

    #[test]
    fn candidate_loader_revalidates_v3_fields_before_artifact_lookup() {
        let state = tempfile::tempdir().unwrap();
        for (name, value, expected) in [
            (
                "invalid-model",
                v3_candidate_lock("gpt 5.3 codex spark", "low"),
                "Codex model must contain only ASCII letters",
            ),
            (
                "invalid-reasoning-effort",
                v3_candidate_lock("gpt-5.3-codex-spark", "invalid"),
                "reasoning effort must be one of",
            ),
            (
                "spark-none",
                v3_candidate_lock("gpt-5.3-codex-spark", "none"),
                "requires reasoning effort low, medium, high, or xhigh",
            ),
        ] {
            let path = state.path().join(format!("candidate-v3-{name}.lock.json"));
            write_candidate_lock(&path, value);

            let error = load_candidate(&path, state.path()).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains(expected), "{message}");
            assert!(
                !message.contains("locked Candidate artifact is unavailable or corrupt"),
                "{message}"
            );
        }
    }
}
