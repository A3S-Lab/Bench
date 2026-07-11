use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn secure_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Bench state directory must not be a symlink: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn create_secure_directory_exclusive(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    secure_directory(path)
}

pub fn create_unique_staging_directory(parent: &Path, purpose: &str) -> Result<PathBuf> {
    secure_directory(parent)?;
    anyhow::ensure!(
        !purpose.is_empty()
            && purpose
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "invalid staging directory purpose"
    );
    for _ in 0..32 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".tmp-{purpose}-{}-{sequence}", std::process::id()));
        match create_secure_directory_exclusive(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    anyhow::bail!("could not allocate an exclusive staging directory")
}

pub fn secure_atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("secure write path has no UTF-8 file name"))?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("secure write path has no parent"))?;
    secure_directory(parent)?;
    let mut temporary = None;
    for _ in 0..32 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = path.with_file_name(format!(
            ".{file_name}.tmp-{}-{sequence}",
            std::process::id()
        ));
        match create_secure_file(&candidate, bytes) {
            Ok(()) => {
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let temporary = temporary
        .ok_or_else(|| anyhow::anyhow!("could not allocate an exclusive temporary state file"))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn read_regular_file(path: &Path, kind: &str) -> Result<Vec<u8>> {
    read_optional_regular_file(path, kind)?
        .ok_or_else(|| anyhow::anyhow!("{kind} is unavailable at {}", path.display()))
}

pub fn read_optional_regular_file(path: &Path, kind: &str) -> Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "{kind} is unavailable at {}: {error}",
                path.display()
            ))
        }
    };
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "{kind} must be a real regular file"
    );
    Ok(Some(std::fs::read(path)?))
}

pub fn sync_tree(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(!kind.is_symlink(), "durable tree contains a symlink");
        if kind.is_dir() {
            sync_tree(&entry.path())?;
        } else if kind.is_file() {
            std::fs::File::open(entry.path())?.sync_all()?;
        } else {
            anyhow::bail!("durable tree contains a special file");
        }
    }
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

pub fn validate_tree(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(!kind.is_symlink(), "state tree contains a symlink");
        if kind.is_dir() {
            validate_tree(&entry.path())?;
        } else if !kind.is_file() {
            anyhow::bail!("state tree contains a special file");
        }
    }
    Ok(())
}

pub fn seal_tree_read_only(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(!kind.is_symlink(), "sealed tree contains a symlink");
        if kind.is_dir() {
            seal_tree_read_only(&entry.path())?;
        } else if kind.is_file() {
            set_owner_only_file(&entry.path(), true)?;
        } else {
            anyhow::bail!("sealed tree contains a special file");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
pub fn remove_sealed_tree(path: &Path) -> Result<()> {
    set_directory_owner_writable(path)?;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(!kind.is_symlink(), "sealed tree contains a symlink");
        if kind.is_dir() {
            remove_sealed_tree(&entry.path())?;
        } else if kind.is_file() {
            #[cfg(not(unix))]
            set_owner_only_file(&entry.path(), false)?;
            std::fs::remove_file(entry.path())?;
        } else {
            anyhow::bail!("sealed tree contains a special file");
        }
    }
    std::fs::remove_dir(path)?;
    Ok(())
}

pub fn set_owner_only_file(path: &Path, read_only: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if read_only { 0o400 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(read_only);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn create_secure_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
fn set_directory_owner_writable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_atomic_writes_never_publish_partial_content() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("state.json");
        let payloads: Vec<Vec<u8>> = (0..8)
            .map(|index| {
                format!(
                    "{{\"writer\":{index},\"padding\":\"{}\"}}",
                    "x".repeat(4096)
                )
                .into_bytes()
            })
            .collect();
        let handles: Vec<_> = payloads
            .iter()
            .cloned()
            .map(|payload| {
                let target = target.clone();
                std::thread::spawn(move || secure_atomic_write(&target, &payload).unwrap())
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(payloads.contains(&std::fs::read(&target).unwrap()));
        assert!(std::fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_replaces_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        let target = root.path().join("state");
        std::fs::write(&outside, "secret").unwrap();
        symlink(&outside, &target).unwrap();
        secure_atomic_write(&target, b"safe").unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"secret");
        assert_eq!(std::fs::read(&target).unwrap(), b"safe");
        assert!(!std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn secure_directory_rejects_symlink() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        let linked = root.path().join("linked");
        std::fs::create_dir(&real).unwrap();
        symlink(&real, &linked).unwrap();
        assert!(secure_directory(&linked).is_err());
    }
}
