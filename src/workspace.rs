use crate::state_fs::{
    seal_role_input_tree, secure_atomic_write, secure_directory, set_owner_only_file,
};
use crate::task::{TaskInfo, WorkspaceSeed};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn state_root() -> Result<PathBuf> {
    let root = std::env::current_dir()?.join(".a3s/bench");
    secure_directory(&root)?;
    Ok(root)
}

pub fn create(task: &TaskInfo) -> Result<PathBuf> {
    let state_root = state_root()?;
    let source = task.root.join("public/workspace");
    let destination = run_directory(&state_root, "workspaces", &task.id)?;
    replace_directory(&destination)?;
    if source.is_dir() {
        copy_tree(&source, &destination)?;
    } else if let Some(seed) = &task.workspace_seed {
        materialize_seed(seed, &state_root, &destination)?;
    } else {
        anyhow::bail!("Task has neither public/workspace nor workspace OCI seed");
    }
    Ok(destination.canonicalize()?)
}

pub fn create_submission(task: &TaskInfo, workspace: &Path) -> Result<PathBuf> {
    let state_root = state_root()?;
    let destination = run_directory(&state_root, "submissions", &task.id)?;
    replace_directory(&destination)?;
    crate::submission::project(workspace, &destination, &task.submission)?;
    seal_role_input_tree(&destination)?;
    Ok(destination.canonicalize()?)
}

fn run_directory(state_root: &Path, kind: &str, task_id: &str) -> Result<PathBuf> {
    let root = state_root.join(kind);
    secure_directory(&root)?;
    Ok(root.join(format!("{task_id}-{}", std::process::id())))
}

fn replace_directory(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn materialize_seed(seed: &WorkspaceSeed, state_root: &Path, destination: &Path) -> Result<()> {
    // Ensure the image is present locally before computing its content id.
    let inspect = Command::new("docker")
        .args(["image", "inspect", &seed.image])
        .output()?;
    if !inspect.status.success() {
        crate::runtime::pull_image_with_retry(&seed.image, seed.platform.as_deref())
            .context("could not pull workspace OCI image")?;
    }
    let image_id = docker_image_id(&seed.image)?;

    let cache = seed_cache_path(state_root, &seed_cache_key(&image_id, seed))?;
    if let Some(clean) = valid_seed_cache(&cache, &image_id, seed)? {
        clone_tree(&clean, destination)?;
        set_tree_owner_only(destination)?;
        return Ok(());
    }
    if is_real_directory(&cache)? {
        std::fs::remove_dir_all(&cache)?;
    }
    // Remove staging directories left behind by previous runs that were
    // killed (e.g. by a timeout) before they could publish or clean up.
    sweep_stale_staging(&state_root.join("workspace-seeds"))?;
    // Extract the seed into a staging directory and publish it as the cache so
    // that subsequent runs can skip the Docker extraction entirely.
    let staging = crate::state_fs::create_unique_staging_directory(
        &state_root.join("workspace-seeds"),
        "seed-image",
    )?;
    // From this point on, any failure must remove the staging directory so it
    // does not leak.  The extraction logic lives in a helper so that the `?`
    // operator can be used freely without forgetting cleanup.
    if let Err(error) = populate_seed_staging(seed, &staging, &image_id) {
        let _ = crate::state_fs::remove_tree(&staging);
        return Err(error);
    }
    match std::fs::rename(&staging, &cache) {
        Ok(()) => {}
        Err(_) if valid_seed_cache(&cache, &image_id, seed)?.is_some() => {
            let _ = crate::state_fs::remove_tree(&staging);
        }
        Err(error) => {
            let _ = crate::state_fs::remove_tree(&staging);
            return Err(anyhow::anyhow!(
                "could not publish workspace seed cache: {error}"
            ));
        }
    }
    clone_tree(&cache.join("tree"), destination)?;
    set_tree_owner_only(destination)
}

/// Extract the workspace seed contents into the staging directory, write the
/// completeness marker, and fsync the tree.  On error the caller is
/// responsible for removing the staging directory.
fn populate_seed_staging(seed: &WorkspaceSeed, staging: &Path, image_id: &str) -> Result<()> {
    let tree = staging.join("tree");
    secure_directory(&tree)?;
    let container = create_seed_container(seed)?;
    let copy = extract_seed_tree(&container, &seed.source_path, &tree);
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    copy?;
    set_tree_owner_only(&tree)?;
    secure_atomic_write(
        &staging.join(".complete"),
        seed_cache_marker(image_id, seed).as_bytes(),
    )?;
    sync_seed_tree(staging)?;
    Ok(())
}

/// Remove staging directories left behind by other processes (e.g. runs that
/// were killed by a timeout before they could clean up).  Directories created
/// by the current process are left untouched.
fn sweep_stale_staging(parent: &Path) -> Result<()> {
    let read_dir = match std::fs::read_dir(parent) {
        Ok(rd) => rd,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let prefix = ".tmp-seed-image-";
    let current_pid = std::process::id().to_string();
    for entry in read_dir {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let name = name.to_owned();
        if !name.starts_with(prefix) {
            continue;
        }
        // Staging dirs are named ".tmp-seed-image-{pid}-{seq}".
        let suffix = &name[prefix.len()..];
        let staging_pid = suffix.split('-').next().unwrap_or("");
        if staging_pid != current_pid {
            let _ = crate::state_fs::remove_tree(&entry.path());
        }
    }
    Ok(())
}

fn docker_image_id(reference: &str) -> Result<String> {
    let output = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", reference])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "could not inspect workspace OCI image {reference:?}"
    );
    let id = String::from_utf8(output.stdout)?.trim().to_owned();
    anyhow::ensure!(
        id.starts_with("sha256:"),
        "Docker returned an invalid workspace image ID"
    );
    Ok(id)
}

fn create_seed_container(seed: &WorkspaceSeed) -> Result<String> {
    let mut create = Command::new("docker");
    create.arg("create");
    if let Some(platform) = seed.platform.as_deref() {
        create.args(["--platform", platform]);
    }
    let output = create.args([&seed.image, "/bin/true"]).output()?;
    anyhow::ensure!(
        output.status.success(),
        "could not create workspace seed container: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn seed_cache_key(image_id: &str, seed: &WorkspaceSeed) -> String {
    let mut hasher = Sha256::new();
    hasher.update(image_id.as_bytes());
    hasher.update([0]);
    hasher.update(seed.source_path.as_bytes());
    hasher.update([0]);
    hasher.update(seed.platform.as_deref().unwrap_or("").as_bytes());
    format!("{:x}", hasher.finalize())
}

fn seed_cache_marker(image_id: &str, seed: &WorkspaceSeed) -> String {
    format!(
        "{}\n{}\n{}\n",
        image_id,
        seed.source_path,
        seed.platform.as_deref().unwrap_or("")
    )
}

fn seed_cache_path(state_root: &Path, key: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid workspace seed cache key"
    );
    Ok(state_root.join("workspace-seeds").join(key))
}

fn valid_seed_cache(path: &Path, image_id: &str, seed: &WorkspaceSeed) -> Result<Option<PathBuf>> {
    let tree = path.join("tree");
    if !is_real_directory(&tree)? {
        return Ok(None);
    }
    let marker = path.join(".complete");
    let Some(bytes) =
        crate::state_fs::read_optional_regular_file(&marker, "workspace seed cache marker")?
    else {
        return Ok(None);
    };
    if bytes != seed_cache_marker(image_id, seed).into_bytes() {
        return Ok(None);
    }
    Ok(Some(tree))
}

fn is_real_directory(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "workspace seed cache path is not a real directory: {}",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn sync_seed_tree(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            sync_seed_tree(&entry.path())?;
        } else if kind.is_file() {
            std::fs::File::open(entry.path())?.sync_all()?;
        } else {
            anyhow::bail!("workspace seed tree contains a special file");
        }
    }
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// Clone a cached seed tree into a fresh writable destination, preserving
/// relative symlinks (which were already validated when the cache was
/// published). The caller is expected to run [`set_tree_owner_only`] on the
/// destination afterwards.
fn clone_tree(source: &Path, destination: &Path) -> Result<()> {
    secure_directory(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_symlink() {
            let link = std::fs::read_link(entry.path())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link, &target)?;
            #[cfg(not(unix))]
            {
                let _ = link;
                anyhow::bail!("workspace seed cache contains a symlink on a non-unix host");
            }
        } else if kind.is_dir() {
            clone_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        } else {
            anyhow::bail!("workspace seed cache contains a special file");
        }
    }
    Ok(())
}

fn extract_seed_tree(container: &str, source_path: &str, destination: &Path) -> Result<()> {
    // Streaming through tar with --no-same-owner prevents container uid/gid
    // metadata from making the extracted workspace unreadable to Bench.
    let mut copy = docker_copy_command(container, source_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start workspace OCI copy")?;
    let archive = copy
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Docker workspace copy did not expose an archive"))?;
    let extract = match tar_extract_command(destination)
        .stdin(Stdio::from(archive))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = copy.kill();
            let _ = copy.wait();
            return Err(error).context("could not start workspace OCI extraction");
        }
    };

    let (copy_output, extract_output) = std::thread::scope(|scope| {
        let copy_wait = scope.spawn(move || copy.wait_with_output());
        let extract_output = extract.wait_with_output()?;
        let copy_output = copy_wait
            .join()
            .map_err(|_| anyhow::anyhow!("workspace OCI copy waiter panicked"))??;
        Ok::<_, anyhow::Error>((copy_output, extract_output))
    })?;
    anyhow::ensure!(
        copy_output.status.success(),
        "workspace OCI source_path is unavailable: {}",
        String::from_utf8_lossy(&copy_output.stderr).trim()
    );
    anyhow::ensure!(
        extract_output.status.success(),
        "could not extract workspace OCI seed: {}",
        String::from_utf8_lossy(&extract_output.stderr).trim()
    );
    Ok(())
}

fn docker_copy_command(container: &str, source_path: &str) -> Command {
    let source_path = source_path.trim_end_matches('/');
    let source = format!("{container}:{source_path}/.");
    let mut command = Command::new("docker");
    command.args(["cp", &source, "-"]);
    command
}

fn tar_extract_command(destination: &Path) -> Command {
    let mut command = Command::new("tar");
    command.args(["-x", "--no-same-owner", "-C"]);
    command.arg(destination);
    command
}

fn set_tree_owner_only(path: &Path) -> Result<()> {
    let root = path.canonicalize()?;
    set_tree_owner_only_inner(&root, &root)
}

fn set_tree_owner_only_inner(root: &Path, path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            validate_seed_symlink(root, &entry.path())?;
        } else if kind.is_dir() {
            set_tree_owner_only_inner(root, &entry.path())?;
        } else if kind.is_file() {
            set_owner_only_file(&entry.path(), false)?;
        } else {
            anyhow::bail!("workspace OCI seed contains a special file");
        }
    }
    secure_directory(path)
}

fn validate_seed_symlink(root: &Path, link: &Path) -> Result<()> {
    let target = std::fs::read_link(link).with_context(|| {
        format!(
            "could not read workspace OCI seed symlink {}",
            link.display()
        )
    })?;
    anyhow::ensure!(
        !target.is_absolute(),
        "workspace OCI seed contains an absolute symlink: {}",
        link.display()
    );
    let resolved = link.canonicalize().with_context(|| {
        format!(
            "workspace OCI seed contains an unresolvable symlink: {}",
            link.display()
        )
    })?;
    anyhow::ensure!(
        resolved.starts_with(root),
        "workspace OCI seed symlink escapes the workspace: {}",
        link.display()
    );
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    secure_directory(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        anyhow::ensure!(!kind.is_symlink(), "workspace symlinks are not supported");
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
            set_owner_only_file(&destination.join(entry.file_name()), false)?;
        } else {
            anyhow::bail!("workspace contains a special file");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_seed_copy_uses_argument_safe_process_pipeline() {
        let copy = docker_copy_command("container-id", "/workspace/it's safe");
        let copy_arguments = copy
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            copy_arguments,
            ["cp", "container-id:/workspace/it's safe/.", "-"]
        );

        let destination = Path::new("destination with ' quote");
        let extract = tar_extract_command(destination);
        let extract_arguments = extract
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            extract_arguments,
            ["-x", "--no-same-owner", "-C", "destination with ' quote"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_permissions_preserve_internal_relative_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("packages")).unwrap();
        std::fs::write(workspace.path().join("packages/mathlib"), "package").unwrap();
        symlink("packages/mathlib", workspace.path().join("mathlib")).unwrap();

        set_tree_owner_only(workspace.path()).unwrap();

        assert_eq!(
            std::fs::read_link(workspace.path().join("mathlib")).unwrap(),
            Path::new("packages/mathlib")
        );
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_permissions_reject_unsafe_or_unresolvable_symlinks() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(parent.path().join("outside"), "outside").unwrap();

        symlink("../outside", workspace.join("escape")).unwrap();
        assert!(set_tree_owner_only(&workspace).is_err());
        std::fs::remove_file(workspace.join("escape")).unwrap();

        symlink(parent.path().join("outside"), workspace.join("absolute")).unwrap();
        assert!(set_tree_owner_only(&workspace).is_err());
        std::fs::remove_file(workspace.join("absolute")).unwrap();

        symlink("cycle-b", workspace.join("cycle-a")).unwrap();
        symlink("cycle-a", workspace.join("cycle-b")).unwrap();
        assert!(set_tree_owner_only(&workspace).is_err());
    }

    #[test]
    fn seed_cache_key_is_stable_and_distinguishes_inputs() {
        let seed = WorkspaceSeed {
            image: "sha256:abc".into(),
            source_path: "/home/workspace".into(),
            platform: Some("linux/amd64".into()),
        };
        let key = seed_cache_key("sha256:abc", &seed);
        assert_eq!(key.len(), 64);
        assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()));
        // Same inputs -> same key.
        assert_eq!(key, seed_cache_key("sha256:abc", &seed));
        // Different image id -> different key.
        assert_ne!(key, seed_cache_key("sha256:xyz", &seed));
        // Different source path -> different key.
        let other = WorkspaceSeed {
            image: "sha256:abc".into(),
            source_path: "/other".into(),
            platform: Some("linux/amd64".into()),
        };
        assert_ne!(key, seed_cache_key("sha256:abc", &other));
    }

    #[test]
    fn seed_cache_marker_round_trips() {
        let seed = WorkspaceSeed {
            image: "sha256:deadbeef".into(),
            source_path: "/home/ws".into(),
            platform: None,
        };
        let marker = seed_cache_marker("sha256:deadbeef", &seed);
        assert_eq!(marker, "sha256:deadbeef\n/home/ws\n\n");
    }

    #[cfg(unix)]
    #[test]
    fn clone_tree_preserves_relative_symlinks() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("packages")).unwrap();
        std::fs::write(source.path().join("packages/mathlib"), "package").unwrap();
        symlink("packages/mathlib", source.path().join("mathlib")).unwrap();
        std::fs::write(source.path().join("README"), "hello").unwrap();

        let destination = tempfile::tempdir().unwrap();
        clone_tree(source.path(), destination.path()).unwrap();

        assert_eq!(
            std::fs::read_link(destination.path().join("mathlib")).unwrap(),
            Path::new("packages/mathlib")
        );
        assert_eq!(
            std::fs::read_to_string(destination.path().join("packages/mathlib")).unwrap(),
            "package"
        );
        assert_eq!(
            std::fs::read_to_string(destination.path().join("README")).unwrap(),
            "hello"
        );
    }
}
