use crate::state_fs::{seal_role_input_tree, secure_directory, set_owner_only_file};
use crate::task::{TaskInfo, WorkspaceSeed};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn state_root() -> Result<PathBuf> {
    let root = std::env::current_dir()?.join(".a3s/bench");
    secure_directory(&root)?;
    Ok(root)
}

pub fn create(task: &TaskInfo) -> Result<PathBuf> {
    let source = task.root.join("public/workspace");
    let destination = run_directory("workspaces", &task.id)?;
    replace_directory(&destination)?;
    if source.is_dir() {
        copy_tree(&source, &destination)?;
    } else if let Some(seed) = &task.workspace_seed {
        materialize_seed(seed, &destination)?;
    } else {
        anyhow::bail!("Task has neither public/workspace nor workspace OCI seed");
    }
    Ok(destination.canonicalize()?)
}

pub fn create_submission(task: &TaskInfo, workspace: &Path) -> Result<PathBuf> {
    let destination = run_directory("submissions", &task.id)?;
    replace_directory(&destination)?;
    crate::submission::project(workspace, &destination, &task.submission)?;
    seal_role_input_tree(&destination)?;
    Ok(destination.canonicalize()?)
}

fn run_directory(kind: &str, task_id: &str) -> Result<PathBuf> {
    let root = std::env::current_dir()?.join(".a3s/bench").join(kind);
    secure_directory(&root)?;
    Ok(root.join(format!("{task_id}-{}", std::process::id())))
}

fn replace_directory(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn materialize_seed(seed: &WorkspaceSeed, destination: &Path) -> Result<()> {
    let inspect = Command::new("docker")
        .args(["image", "inspect", &seed.image])
        .output()?;
    if !inspect.status.success() {
        crate::runtime::pull_image_with_retry(&seed.image, seed.platform.as_deref())
            .context("could not pull workspace OCI image")?;
    }
    let mut create = Command::new("docker");
    create.arg("create");
    if let Some(platform) = seed.platform.as_deref() {
        create.args(["--platform", platform]);
    }
    let output = create.args([&seed.image, "/bin/true"]).output()?;
    anyhow::ensure!(
        output.status.success(),
        "could not create workspace seed container"
    );
    let container = String::from_utf8(output.stdout)?.trim().to_owned();
    secure_directory(destination)?;
    let copy = extract_seed_tree(&container, &seed.source_path, destination);
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    copy?;
    set_tree_owner_only(destination)
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
}
