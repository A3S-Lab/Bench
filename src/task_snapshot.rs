use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn capture(source: &Path, state_root: &Path) -> Result<String> {
    capture_with_hook(source, state_root, || Ok(()))
}

fn capture_with_hook<F>(source: &Path, state_root: &Path, after_read: F) -> Result<String>
where
    F: FnOnce() -> Result<()>,
{
    let source = source.canonicalize()?;
    let files = collect(&source)?;
    let digest = tree_digest(&source, &files)?;
    let destination = artifact_path(state_root, &digest)?;
    if real_directory(&destination)? {
        verify(&destination, &digest)?;
        crate::state_fs::seal_tree_read_only(&destination)?;
        return Ok(digest);
    }
    let artifacts = state_root.join("artifacts");
    crate::state_fs::secure_directory(&artifacts).context("could not secure artifact root")?;
    let staging = crate::state_fs::create_unique_staging_directory(&artifacts, "snapshot")
        .context("could not allocate artifact staging directory")?;
    let publish = (|| -> Result<()> {
        after_read()?;
        for relative in &files {
            let target = staging.join(relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(source.join(relative), target).map_err(|error| {
                anyhow::anyhow!("source_changed: Task source changed during capture: {error}")
            })?;
        }
        ensure_stable_generation(&source, &staging, &files, &digest)?;
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
    verify(&destination, &digest).context("published artifact failed verification")?;
    crate::state_fs::seal_tree_read_only(&destination)
        .context("could not seal published artifact")?;
    #[cfg(unix)]
    std::fs::File::open(&artifacts)
        .context("could not open artifact root for sync")?
        .sync_all()
        .context("could not sync artifact root")?;
    Ok(digest)
}

fn ensure_stable_generation(
    source: &Path,
    staging: &Path,
    initial_files: &[PathBuf],
    initial_digest: &str,
) -> Result<()> {
    let captured_files = collect(staging)?;
    let captured_digest = tree_digest(staging, &captured_files)?;
    let final_files = collect(source).map_err(source_changed)?;
    let final_digest = tree_digest(source, &final_files).map_err(source_changed)?;
    anyhow::ensure!(
        initial_files == final_files
            && initial_digest == captured_digest
            && initial_digest == final_digest,
        "source_changed: Task source changed during capture"
    );
    Ok(())
}

fn source_changed(error: anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!("source_changed: Task source changed during capture: {error:#}")
}

pub fn verify(path: &Path, expected: &str) -> Result<()> {
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
        hasher.update([u8::from(crate::state_fs::is_executable(
            &root.join(relative),
        )?)]);
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
    fn visit(
        root: &Path,
        directory: &Path,
        output: &mut Vec<PathBuf>,
        seen_case: &mut HashSet<String>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            anyhow::ensure!(!kind.is_symlink(), "snapshot source contains a symlink");
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            let normalized = relative
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("snapshot path is not UTF-8"))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            anyhow::ensure!(
                !normalized.is_empty()
                    && normalized
                        .split('/')
                        .all(|part| !part.is_empty() && part != "." && part != ".."),
                "snapshot source contains an unsafe path"
            );
            anyhow::ensure!(
                seen_case.insert(normalized.to_lowercase()),
                "snapshot source contains case-colliding paths"
            );
            if kind.is_dir() {
                visit(root, &entry.path(), output, seen_case)?;
            } else if kind.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    anyhow::ensure!(
                        entry.metadata()?.nlink() == 1,
                        "snapshot source contains a hard link"
                    );
                }
                output.push(relative);
            } else {
                anyhow::bail!("snapshot source contains a special file");
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    let mut seen_case = HashSet::new();
    visit(root, root, &mut files, &mut seen_case)?;
    files.sort();
    Ok(files)
}

pub fn artifact_path(state_root: &Path, digest: &str) -> Result<PathBuf> {
    let value = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow::anyhow!("artifact digest must use sha256"))?;
    anyhow::ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid artifact digest"
    );
    Ok(state_root.join("artifacts").join(value))
}

#[cfg(test)]
mod tests;
