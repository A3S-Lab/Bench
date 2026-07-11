use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLock {
    pub schema: String,
    pub task_revision: String,
    pub artifact_digest: String,
    pub resolved_images: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateLock {
    pub schema: String,
    pub candidate_revision: String,
    pub artifact_digest: String,
    pub model: Option<String>,
}

pub fn create_task(source: &Path, state_root: &Path, output: &Path) -> Result<TaskLock> {
    let task = crate::task::load_local(source)?;
    let root = if source.is_dir() {
        source
    } else {
        source.parent().unwrap_or_else(|| Path::new("."))
    };
    let digest = snapshot(root, state_root)?;
    let mut resolved_images = BTreeMap::new();
    for (reference, platform) in task_image_references(&task) {
        let resolved = crate::runtime::resolve_image(reference, platform)?;
        resolved_images.insert(image_key(reference, platform), resolved.immutable_ref);
    }
    let value = TaskLock {
        schema: "a3s.bench.task-lock.v1".into(),
        task_revision: digest.clone(),
        artifact_digest: digest,
        resolved_images,
    };
    write_exclusive(output, &serde_json::to_vec_pretty(&value)?)?;
    Ok(value)
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
    let digest = snapshot(&asset.root, state_root)?;
    let value = CandidateLock {
        schema: "a3s.bench.candidate-lock.v1".into(),
        candidate_revision: asset.identity,
        artifact_digest: digest,
        model,
    };
    write_exclusive(output, &serde_json::to_vec_pretty(&value)?)?;
    Ok(value)
}

pub fn load_task(path: &Path, state_root: &Path) -> Result<(TaskLock, PathBuf)> {
    let value: TaskLock = serde_json::from_slice(&read_lock_file(path)?)?;
    anyhow::ensure!(
        value.schema == "a3s.bench.task-lock.v1",
        "invalid TaskLock schema"
    );
    anyhow::ensure!(
        value.task_revision == value.artifact_digest,
        "TaskLock revision does not match artifact digest"
    );
    let artifact = artifact_path(state_root, &value.artifact_digest)?;
    verify_artifact(&artifact, &value.artifact_digest)
        .context("locked Task artifact is unavailable or corrupt")?;
    Ok((value, artifact))
}

pub fn load_candidate(path: &Path, state_root: &Path) -> Result<(CandidateLock, PathBuf)> {
    let value: CandidateLock = serde_json::from_slice(&read_lock_file(path)?)?;
    anyhow::ensure!(
        value.schema == "a3s.bench.candidate-lock.v1",
        "invalid CandidateLock schema"
    );
    anyhow::ensure!(
        !value.candidate_revision.trim().is_empty(),
        "CandidateLock revision is empty"
    );
    let artifact = artifact_path(state_root, &value.artifact_digest)?;
    verify_artifact(&artifact, &value.artifact_digest)
        .context("locked Candidate artifact is unavailable or corrupt")?;
    Ok((value, artifact))
}

fn read_lock_file(path: &Path) -> Result<Vec<u8>> {
    crate::state_fs::read_regular_file(path, "lock")
}

fn snapshot(source: &Path, state_root: &Path) -> Result<String> {
    let source = source.canonicalize()?;
    let files = collect(&source)?;
    let digest = tree_digest(&source, &files)?;
    let destination = artifact_path(state_root, &digest)?;
    if real_directory(&destination)? {
        verify_artifact(&destination, &digest)?;
        crate::state_fs::seal_tree_read_only(&destination)?;
        return Ok(digest);
    }
    let artifacts = state_root.join("artifacts");
    crate::state_fs::secure_directory(&artifacts).context("could not secure artifact root")?;
    let staging = crate::state_fs::create_unique_staging_directory(&artifacts, "snapshot")
        .context("could not allocate artifact staging directory")?;
    let publish = (|| -> Result<()> {
        for relative in &files {
            let target = staging.join(relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(source.join(relative), target)
                .context("could not copy file into artifact staging")?;
        }
        crate::state_fs::sync_tree(&staging).context("could not sync artifact staging")?;
        match std::fs::rename(&staging, &destination) {
            Ok(()) => Ok(()),
            Err(_) if real_directory(&destination)? => Ok(()),
            Err(error) => Err(error).context("could not publish artifact staging"),
        }
    })();
    if staging.exists() {
        std::fs::remove_dir_all(&staging).context("could not clean artifact staging")?;
    }
    publish?;
    verify_artifact(&destination, &digest).context("published artifact failed verification")?;
    crate::state_fs::seal_tree_read_only(&destination)
        .context("could not seal published artifact")?;
    #[cfg(unix)]
    std::fs::File::open(&artifacts)
        .context("could not open artifact root for sync")?
        .sync_all()
        .context("could not sync artifact root")?;
    Ok(digest)
}

fn verify_artifact(path: &Path, expected: &str) -> Result<()> {
    anyhow::ensure!(real_directory(path)?, "artifact directory is missing");
    let files = collect(path)?;
    let actual = tree_digest(path, &files)?;
    anyhow::ensure!(actual == expected, "artifact tree digest mismatch");
    Ok(())
}

fn tree_digest(root: &Path, files: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    for relative in files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(std::fs::read(root.join(relative))?);
        hasher.update([0]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn real_directory(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Bench artifact path is not a real directory: {}",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn collect(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            anyhow::ensure!(!kind.is_symlink(), "snapshot source contains a symlink");
            if kind.is_dir() {
                visit(root, &entry.path(), output)?;
            } else if kind.is_file() {
                output.push(entry.path().strip_prefix(root)?.to_path_buf());
            } else {
                anyhow::bail!("snapshot source contains a special file");
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn artifact_path(state_root: &Path, digest: &str) -> Result<PathBuf> {
    let value = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow::anyhow!("artifact digest must use sha256"))?;
    anyhow::ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid artifact digest"
    );
    Ok(state_root.join("artifacts").join(value))
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
