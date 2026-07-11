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
    pub resolved_images: BTreeMap<String, String>,
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
}

pub fn create_task(source: &Path, state_root: &Path, output: &Path) -> Result<TaskLock> {
    let root = if source.is_dir() {
        source
    } else {
        source.parent().unwrap_or_else(|| Path::new("."))
    };
    let digest = crate::task_snapshot::capture(root, state_root)?;
    let captured = crate::task_snapshot::artifact_path(state_root, &digest)?;
    let task = crate::task::load_local(&captured)?;
    let judge = resolve_judge(&task, state_root)?;
    let judge_artifact_digest = crate::task_snapshot::capture(&judge.root, state_root)?;
    let judge_artifact = crate::task_snapshot::artifact_path(state_root, &judge_artifact_digest)?;
    let locked_judge = crate::asset::load_local(&judge_artifact)?;
    let mut resolved_images = BTreeMap::new();
    for (reference, platform) in task_image_references(&task) {
        let resolved = crate::runtime::resolve_image(reference, platform)?;
        resolved_images.insert(image_key(reference, platform), resolved.immutable_ref);
    }
    let mut value = TaskLock {
        schema: "a3s.bench.task-lock.v1".into(),
        lock_digest: String::new(),
        task_revision: digest.clone(),
        artifact_digest: digest,
        judge_revision: locked_judge.identity,
        judge_artifact_digest,
        resolved_images,
    };
    value.lock_digest = crate::lock_identity::task(&value)?;
    write_exclusive(output, &serde_json::to_vec_pretty(&value)?)?;
    Ok(value)
}

fn resolve_judge(
    task: &crate::task::TaskInfo,
    state_root: &Path,
) -> Result<crate::asset::LocalAgentAsset> {
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

pub fn create_candidate(
    reference: &str,
    model: Option<String>,
    state_root: &Path,
    output: &Path,
) -> Result<CandidateLock> {
    let asset = crate::asset::resolve(reference, state_root)?;
    let digest = crate::task_snapshot::capture(&asset.root, state_root)?;
    let captured = crate::task_snapshot::artifact_path(state_root, &digest)?;
    let locked_asset = crate::asset::load_local(&captured)?;
    let mut value = CandidateLock {
        schema: "a3s.bench.candidate-lock.v1".into(),
        lock_digest: String::new(),
        candidate_revision: locked_asset.identity,
        artifact_digest: digest,
        model,
    };
    value.lock_digest = crate::lock_identity::candidate(&value)?;
    write_exclusive(output, &serde_json::to_vec_pretty(&value)?)?;
    Ok(value)
}

pub fn load_task(path: &Path, state_root: &Path) -> Result<LoadedTaskLock> {
    let value: TaskLock = serde_json::from_slice(&read_lock_file(path)?)?;
    anyhow::ensure!(
        value.schema == "a3s.bench.task-lock.v1",
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
    let judge_artifact =
        crate::task_snapshot::artifact_path(state_root, &value.judge_artifact_digest)?;
    crate::task_snapshot::verify(&judge_artifact, &value.judge_artifact_digest)
        .context("locked Judge artifact is unavailable or corrupt")?;
    let judge = crate::asset::load_local(&judge_artifact)
        .context("locked Judge artifact is not an Agent Asset")?;
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
        value.schema == "a3s.bench.candidate-lock.v1",
        "invalid CandidateLock schema"
    );
    crate::lock_identity::validate_digest(&value.lock_digest)?;
    anyhow::ensure!(
        crate::lock_identity::candidate(&value)? == value.lock_digest,
        "CandidateLock semantic digest mismatch"
    );
    anyhow::ensure!(
        !value.candidate_revision.trim().is_empty(),
        "CandidateLock revision is empty"
    );
    let artifact = crate::task_snapshot::artifact_path(state_root, &value.artifact_digest)?;
    crate::task_snapshot::verify(&artifact, &value.artifact_digest)
        .context("locked Candidate artifact is unavailable or corrupt")?;
    let candidate = crate::asset::load_local(&artifact)
        .context("locked Candidate artifact is not an Agent Asset")?;
    anyhow::ensure!(
        candidate.identity == value.candidate_revision,
        "CandidateLock revision does not match artifact"
    );
    Ok((value, artifact))
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
