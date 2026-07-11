use crate::state_fs::{seal_role_input_tree, secure_directory, set_owner_only_file};
use crate::task::{TaskInfo, WorkspaceSeed};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        let mut pull = Command::new("docker");
        pull.arg("pull");
        if let Some(platform) = seed.platform.as_deref() {
            pull.args(["--platform", platform]);
        }
        let pull = pull.arg(&seed.image).output()?;
        anyhow::ensure!(
            pull.status.success(),
            "could not pull workspace OCI image: {}",
            String::from_utf8_lossy(&pull.stderr).trim()
        );
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
    let copy = Command::new("docker")
        .arg("cp")
        .arg(format!("{}:{}/.", container, seed.source_path))
        .arg(destination)
        .output();
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    let copy = copy?;
    anyhow::ensure!(
        copy.status.success(),
        "workspace OCI source_path is unavailable: {}",
        String::from_utf8_lossy(&copy.stderr).trim()
    );
    set_tree_owner_only(destination)
}

fn set_tree_owner_only(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(!kind.is_symlink(), "workspace OCI seed contains a symlink");
        if kind.is_dir() {
            set_tree_owner_only(&entry.path())?;
        } else if kind.is_file() {
            set_owner_only_file(&entry.path(), false)?;
        } else {
            anyhow::bail!("workspace OCI seed contains a special file");
        }
    }
    secure_directory(path)
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
