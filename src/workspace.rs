use crate::state_fs::{
    seal_role_input_tree, secure_atomic_write, secure_directory, set_owner_only_file,
};
use crate::task::{TaskInfo, WorkspaceSeed};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

static RUN_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) const CONTAINER_TREE_EXPORT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const CONTAINER_TREE_EXPORT_MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const PROCESS_STDERR_LIMIT: usize = 64 * 1024;
const PIPELINE_POLL_INTERVAL: Duration = Duration::from_millis(10);

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

pub fn create_empty(task: &TaskInfo) -> Result<PathBuf> {
    let state_root = state_root()?;
    let destination = run_directory(&state_root, "workspaces", &task.id)?;
    replace_directory(&destination)?;
    secure_directory(&destination)?;
    Ok(destination.canonicalize()?)
}

#[cfg(test)]
pub fn export_container_tree(container: &str, source_path: &str, destination: &Path) -> Result<()> {
    populate_empty_directory_atomically(destination, |staging| {
        extract_seed_tree(container, source_path, staging)
    })
}

const CONTAINER_FILE_TAR_OVERHEAD: u64 = 1024 * 1024;

pub(crate) fn export_container_files(
    container: &str,
    source_path: &str,
    destination: &Path,
    relative_paths: &[PathBuf],
    policy: &crate::task::SubmissionPolicy,
    deadline: Instant,
) -> Result<()> {
    let matcher = crate::submission::terminal_matcher(policy)?;
    let mut candidates = Vec::new();
    for relative in relative_paths {
        let normalized = crate::submission::normalize_terminal_path(relative)?;
        if matcher.matches(&normalized) {
            candidates.push((normalized, relative.clone()));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    anyhow::ensure!(
        candidates.windows(2).all(|pair| pair[0].0 != pair[1].0),
        "container workspace inventory contains a duplicate path"
    );

    populate_empty_directory_atomically(destination, |staging| {
        export_selected_container_files(
            container,
            source_path,
            staging,
            &candidates,
            policy,
            deadline,
        )
    })
}

fn export_selected_container_files(
    container: &str,
    source_path: &str,
    staging: &Path,
    candidates: &[(String, PathBuf)],
    policy: &crate::task::SubmissionPolicy,
    deadline: Instant,
) -> Result<()> {
    let parent = staging
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workspace export staging has no parent"))?;
    let probe = crate::state_fs::create_unique_staging_directory(parent, "workspace-export-file")
        .context("could not create workspace export probe")?;
    let result = (|| {
        let mut remaining_bytes = CONTAINER_TREE_EXPORT_MAX_BYTES;
        let mut limits = crate::submission::TerminalLimits::new(policy);
        for (normalized, relative) in candidates {
            if limits.is_full() {
                break;
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    anyhow::anyhow!("workspace OCI export exceeded its fixed deadline")
                })?;
            anyhow::ensure!(
                !remaining.is_zero(),
                "workspace OCI export exceeded its fixed deadline"
            );
            replace_directory(&probe).context("could not reset workspace export probe")?;
            secure_directory(&probe).context("could not secure workspace export probe")?;
            let container_path = format!("{}/{}", source_path.trim_end_matches('/'), normalized);
            let policy_cap = policy
                .max_file_bytes
                .saturating_add(CONTAINER_FILE_TAR_OVERHEAD);
            let copy_cap = remaining_bytes.min(policy_cap);
            anyhow::ensure!(
                remaining_bytes != 0,
                "workspace archive exceeded its fixed cumulative transfer limit"
            );
            let transferred = match extract_container_file(
                container,
                &container_path,
                &probe,
                remaining,
                copy_cap,
            ) {
                Ok(transferred) => transferred,
                Err(error) if copy_cap == policy_cap => {
                    let Some(ArchivePumpError::Limit { consumed, .. }) =
                        error.downcast_ref::<ArchivePumpError>()
                    else {
                        return Err(error);
                    };
                    remaining_bytes = remaining_bytes.checked_sub(*consumed).ok_or_else(|| {
                        anyhow::anyhow!(
                            "workspace archive exceeded its fixed cumulative transfer limit"
                        )
                    })?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            remaining_bytes = remaining_bytes.checked_sub(transferred).ok_or_else(|| {
                anyhow::anyhow!("workspace archive transfer accounting underflowed")
            })?;
            let file =
                only_regular_file(&probe).context("could not validate workspace export probe")?;
            let size = file
                .metadata()
                .context("could not inspect exported terminal file")?
                .len();
            if !limits.select(size) {
                continue;
            }
            let target = staging.join(relative);
            if let Some(parent) = target.parent() {
                secure_directory(parent)
                    .context("could not secure exported terminal file parent")?;
            }
            std::fs::rename(&file, &target).with_context(|| {
                format!(
                    "could not retain exported submission file {}",
                    relative.display()
                )
            })?;
        }
        Ok(())
    })();
    let cleanup = crate::state_fs::remove_tree(&probe);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("could not clean workspace export probe"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn only_regular_file(root: &Path) -> Result<PathBuf> {
    fn visit(directory: &Path, found: &mut Option<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(&entry.path(), found)?;
            } else if kind.is_file() {
                anyhow::ensure!(
                    found.is_none(),
                    "Docker copied more than one terminal workspace file"
                );
                *found = Some(entry.path());
            } else {
                anyhow::bail!("Docker copied a non-regular terminal workspace entry");
            }
        }
        Ok(())
    }

    let mut found = None;
    visit(root, &mut found)?;
    found.ok_or_else(|| anyhow::anyhow!("Docker copied no terminal workspace file"))
}

fn extract_container_file(
    container: &str,
    source_path: &str,
    destination: &Path,
    timeout: Duration,
    max_bytes: u64,
) -> Result<u64> {
    let mut copy = docker_copy_file_command(container, source_path);
    let mut extract = tar_extract_command(destination);
    run_archive_pipeline(&mut copy, &mut extract, timeout, max_bytes)
}

fn populate_empty_directory_atomically<F>(destination: &Path, populate: F) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    anyhow::ensure!(
        destination.is_dir(),
        "container workspace export destination is unavailable"
    );
    anyhow::ensure!(
        std::fs::read_dir(destination)?.next().is_none(),
        "container workspace export destination must be empty"
    );
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("container workspace export destination has no parent"))?;
    let staging = crate::state_fs::create_unique_staging_directory(parent, "workspace-export")
        .context("could not create workspace export staging")?;
    let result = (|| {
        populate(&staging)?;
        set_tree_owner_only(&staging).context("could not harden exported container workspace")?;
        // On Unix, renaming a directory over an existing empty directory is
        // atomic. The original empty destination therefore remains visible
        // until the complete, permission-hardened tree is ready.
        std::fs::rename(&staging, destination)
            .context("could not publish exported container workspace")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = crate::state_fs::remove_tree(&staging);
    }
    result
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
    Ok(unique_run_directory(&root, task_id))
}

fn unique_run_directory(root: &Path, task_id: &str) -> PathBuf {
    let sequence = RUN_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    root.join(format!("{task_id}-{}-{sequence}", std::process::id()))
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
        // The cache tree already has owner-only permissions (set during
        // populate_seed_staging), and `cp -a` preserves them, so we can
        // skip the expensive set_tree_owner_only traversal here.
        clone_tree(&clean, destination)?;
        return Ok(());
    }
    if is_real_directory(&cache)? {
        std::fs::remove_dir_all(&cache)?;
    }
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
    // Cache tree already has owner-only permissions; cp -a preserves them.
    Ok(())
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
    // Previously we fsynced every file in the tree here (sync_seed_tree),
    // but that caused multi-minute stalls on large seeds (248k files /
    // 14GB) because each fsync forces an ext4 journal commit.  The cache
    // is validated by the .complete marker and can be regenerated if lost
    // to a crash, so per-file fsync is not worth the cost.  A single
    // directory sync after the rename is sufficient.
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

/// Clone a cached seed tree into a fresh writable destination, preserving
/// relative symlinks (which were already validated when the cache was
/// published) and ensuring the destination root is owner-only.
fn clone_tree(source: &Path, destination: &Path) -> Result<()> {
    // Use `cp -a` which preserves symlinks, permissions, and is significantly
    // faster than per-file std::fs::copy for large trees (14GB / 250k files).
    // `source/.` copies the *contents* of source into destination rather
    // than nesting source as a subdirectory of destination.
    secure_directory(destination)?;
    let output = Command::new("cp")
        .arg("-a")
        .arg(format!("{}/.", source.display()))
        .arg(format!("{}/", destination.display()))
        .output()?;
    // GNU cp reapplies the source root's mode to an existing destination when
    // copying `source/.`, so restore the private state-directory contract.
    secure_directory(destination)?;
    anyhow::ensure!(
        output.status.success(),
        "cp -a failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[derive(Debug)]
enum ArchivePumpError {
    Deadline,
    Limit { limit: u64, consumed: u64 },
    Io(io::Error),
}

impl fmt::Display for ArchivePumpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadline => write!(formatter, "workspace archive transfer timed out"),
            Self::Limit { limit, .. } => write!(
                formatter,
                "workspace archive exceeded the fixed {limit}-byte transfer limit"
            ),
            Self::Io(error) => write!(formatter, "workspace archive transfer failed: {error}"),
        }
    }
}

impl std::error::Error for ArchivePumpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Deadline | Self::Limit { .. } => None,
        }
    }
}

impl From<io::Error> for ArchivePumpError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
struct BoundedStderr {
    bytes: Vec<u8>,
    truncated: bool,
}

enum PipelineAbort {
    Timeout,
    CopyFailed,
    ExtractFailed,
    Pump(ArchivePumpError),
    Monitor(anyhow::Error),
}

fn extract_seed_tree(container: &str, source_path: &str, destination: &Path) -> Result<()> {
    let mut copy = docker_copy_command(container, source_path);
    let mut extract = tar_extract_command(destination);
    run_archive_pipeline(
        &mut copy,
        &mut extract,
        CONTAINER_TREE_EXPORT_TIMEOUT,
        CONTAINER_TREE_EXPORT_MAX_BYTES,
    )
    .map(|_| ())
}

fn run_archive_pipeline(
    copy_command: &mut Command,
    extract_command: &mut Command,
    timeout: Duration,
    max_bytes: u64,
) -> Result<u64> {
    // The archive is pumped in userspace rather than connecting the two
    // children directly. This makes the raw transfer byte limit enforceable.
    let mut copy = copy_command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start workspace OCI copy")?;
    let archive = copy
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Docker workspace copy did not expose an archive"))?;
    let mut extract = match extract_command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            terminate_child(&mut copy);
            return Err(error).context("could not start workspace OCI extraction");
        }
    };

    let extract_input = extract
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("tar did not expose workspace archive input"))?;
    let copy_stderr = copy
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Docker workspace copy did not expose stderr"))?;
    let extract_stderr = extract
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("tar did not expose workspace extraction stderr"))?;

    // Drain both stderr streams concurrently so a verbose child cannot fill
    // its pipe and deadlock. Only a bounded prefix is retained in memory.
    let copy_stderr_thread =
        std::thread::spawn(move || read_bounded_stderr(copy_stderr, PROCESS_STDERR_LIMIT));
    let extract_stderr_thread =
        std::thread::spawn(move || read_bounded_stderr(extract_stderr, PROCESS_STDERR_LIMIT));

    let started = Instant::now();
    let deadline = started.checked_add(timeout).unwrap_or(started);
    let (pump_sender, pump_receiver) = mpsc::sync_channel(1);
    let pump_thread = std::thread::spawn(move || {
        let _ = pump_sender.send(pump_archive(archive, extract_input, deadline, max_bytes));
    });

    let mut copy_status = None;
    let mut extract_status = None;
    let mut pump_result = None;
    let abort = loop {
        if Instant::now() >= deadline {
            break Some(PipelineAbort::Timeout);
        }
        if let Err(error) = poll_child(&mut copy, &mut copy_status) {
            break Some(PipelineAbort::Monitor(
                anyhow::Error::from(error).context("could not poll Docker copy"),
            ));
        }
        if copy_status.is_some_and(|status| !status.success()) {
            break Some(PipelineAbort::CopyFailed);
        }
        if let Err(error) = poll_child(&mut extract, &mut extract_status) {
            break Some(PipelineAbort::Monitor(
                anyhow::Error::from(error).context("could not poll workspace extraction"),
            ));
        }
        if extract_status.is_some_and(|status| !status.success()) {
            break Some(PipelineAbort::ExtractFailed);
        }
        if pump_result.is_none() {
            match pump_receiver.try_recv() {
                Ok(result) => match result {
                    Ok(bytes) => pump_result = Some(Ok(bytes)),
                    Err(error) => break Some(PipelineAbort::Pump(error)),
                },
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    break Some(PipelineAbort::Monitor(anyhow::anyhow!(
                        "workspace archive pump exited without a result"
                    )));
                }
            }
        }
        if copy_status.is_some() && extract_status.is_some() && pump_result.is_some() {
            break None;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(PIPELINE_POLL_INTERVAL));
    };

    if abort.is_some() {
        terminate_child(&mut copy);
        terminate_child(&mut extract);
    }
    let copy_wait = copy.wait();
    let extract_wait = extract.wait();
    if pump_result.is_none() {
        pump_result = Some(pump_receiver.recv().unwrap_or_else(|_| {
            Err(ArchivePumpError::Io(io::Error::other(
                "workspace archive pump exited without a result",
            )))
        }));
    }
    let pump_join = pump_thread.join();
    let copy_stderr = join_stderr_reader(copy_stderr_thread, "Docker copy");
    let extract_stderr = join_stderr_reader(extract_stderr_thread, "tar extraction");

    let copy_status = copy_wait.context("could not wait for Docker workspace copy")?;
    let extract_status = extract_wait.context("could not wait for workspace extraction")?;
    anyhow::ensure!(pump_join.is_ok(), "workspace archive pump panicked");

    match abort {
        Some(PipelineAbort::Timeout) => anyhow::bail!(
            "workspace OCI export exceeded its {:?} deadline{}{}",
            timeout,
            stderr_suffix("Docker copy", &copy_stderr),
            stderr_suffix("tar extraction", &extract_stderr)
        ),
        Some(PipelineAbort::CopyFailed) => anyhow::bail!(
            "workspace OCI source_path is unavailable{}",
            stderr_suffix("Docker copy", &copy_stderr)
        ),
        Some(PipelineAbort::ExtractFailed) => anyhow::bail!(
            "could not extract workspace OCI seed{}",
            stderr_suffix("tar extraction", &extract_stderr)
        ),
        Some(PipelineAbort::Pump(error)) => return Err(error.into()),
        Some(PipelineAbort::Monitor(error)) => return Err(error),
        None => {}
    }
    anyhow::ensure!(
        copy_status.success(),
        "workspace OCI source_path is unavailable{}",
        stderr_suffix("Docker copy", &copy_stderr)
    );
    anyhow::ensure!(
        extract_status.success(),
        "could not extract workspace OCI seed{}",
        stderr_suffix("tar extraction", &extract_stderr)
    );
    Ok(pump_result.expect("pump result is populated before pipeline completion")?)
}

fn poll_child(child: &mut Child, status: &mut Option<ExitStatus>) -> io::Result<()> {
    if status.is_none() {
        *status = child.try_wait()?;
    }
    Ok(())
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn pump_archive<R, W>(
    mut source: R,
    mut destination: W,
    deadline: Instant,
    max_bytes: u64,
) -> std::result::Result<u64, ArchivePumpError>
where
    R: Read,
    W: Write,
{
    let mut transferred = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err(ArchivePumpError::Deadline);
        }
        let count = source.read(&mut buffer)?;
        if count == 0 {
            destination.flush()?;
            return Ok(transferred);
        }
        let next = transferred
            .checked_add(count as u64)
            .ok_or(ArchivePumpError::Limit {
                limit: max_bytes,
                consumed: u64::MAX,
            })?;
        if next > max_bytes {
            return Err(ArchivePumpError::Limit {
                limit: max_bytes,
                consumed: next,
            });
        }
        destination.write_all(&buffer[..count])?;
        transferred = next;
    }
}

fn read_bounded_stderr<R: Read>(mut stderr: R, limit: usize) -> io::Result<BoundedStderr> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = stderr.read(&mut buffer)?;
        if count == 0 {
            return Ok(BoundedStderr { bytes, truncated });
        }
        let retained = limit.saturating_sub(bytes.len()).min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < count;
    }
}

fn join_stderr_reader(
    handle: std::thread::JoinHandle<io::Result<BoundedStderr>>,
    process: &str,
) -> BoundedStderr {
    match handle.join() {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => BoundedStderr {
            bytes: format!("could not read {process} stderr: {error}").into_bytes(),
            truncated: false,
        },
        Err(_) => BoundedStderr {
            bytes: format!("{process} stderr reader panicked").into_bytes(),
            truncated: false,
        },
    }
}

fn stderr_suffix(process: &str, output: &BoundedStderr) -> String {
    let text = String::from_utf8_lossy(&output.bytes);
    let truncation = if output.truncated { " [truncated]" } else { "" };
    if text.trim().is_empty() {
        String::new()
    } else {
        format!(": {process}: {}{truncation}", text.trim())
    }
}

fn docker_copy_command(container: &str, source_path: &str) -> Command {
    let source_path = source_path.trim_end_matches('/');
    let source = format!("{container}:{source_path}/.");
    let mut command = Command::new("docker");
    command.args(["cp", &source, "-"]);
    command
}

fn docker_copy_file_command(container: &str, source_path: &str) -> Command {
    let source = format!("{container}:{source_path}");
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

    #[cfg(unix)]
    #[test]
    fn isolated_export_probe_rejects_non_regular_entries() {
        use std::os::unix::fs::symlink;

        let probe = tempfile::tempdir().unwrap();
        std::fs::write(probe.path().join("target"), "data").unwrap();
        symlink("target", probe.path().join("link")).unwrap();
        assert!(only_regular_file(probe.path()).is_err());
    }

    #[test]
    #[ignore = "requires Docker and the native Alpine image"]
    fn filtered_container_export_copies_only_deterministically_selected_files() {
        struct ContainerGuard(String);
        impl Drop for ContainerGuard {
            fn drop(&mut self) {
                let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
            }
        }

        let name = format!(
            "a3s-bench-filtered-export-test-{}-{}",
            std::process::id(),
            RUN_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let script = concat!(
            "mkdir -p /workspace/keep /workspace/cache; ",
            "dd if=/dev/zero of=/workspace/keep/a.txt bs=600 count=1 2>/dev/null; ",
            "dd if=/dev/zero of=/workspace/keep/b.txt bs=600 count=1 2>/dev/null; ",
            "dd if=/dev/zero of=/workspace/keep/c.txt bs=400 count=1 2>/dev/null; ",
            "dd if=/dev/zero of=/workspace/cache/huge.bin bs=1024 count=2048 2>/dev/null"
        );
        let created = Command::new("docker")
            .args([
                "create",
                "--name",
                &name,
                "docker.io/library/alpine:3.20",
                "sh",
                "-c",
                script,
            ])
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
        let container = String::from_utf8(created.stdout).unwrap().trim().to_owned();
        let _guard = ContainerGuard(container.clone());
        let started = Command::new("docker")
            .args(["start", "--attach", &container])
            .output()
            .unwrap();
        assert!(started.status.success());

        let output = tempfile::tempdir().unwrap();
        let destination = output.path().join("workspace");
        secure_directory(&destination).unwrap();
        let policy = crate::task::SubmissionPolicy {
            include: vec!["keep/".into()],
            exclude: Vec::new(),
            max_files: 10,
            max_total_bytes: 1_024,
            max_file_bytes: 700,
        };
        let paths = vec![
            PathBuf::from("cache/huge.bin"),
            PathBuf::from("keep/c.txt"),
            PathBuf::from("keep/b.txt"),
            PathBuf::from("keep/a.txt"),
        ];
        export_container_files(
            &container,
            "/workspace",
            &destination,
            &paths,
            &policy,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap_or_else(|error| panic!("{error:#}"));

        assert_eq!(
            std::fs::metadata(destination.join("keep/a.txt"))
                .unwrap()
                .len(),
            600
        );
        assert!(!destination.join("keep/b.txt").exists());
        assert_eq!(
            std::fs::metadata(destination.join("keep/c.txt"))
                .unwrap()
                .len(),
            400
        );
        assert!(!destination.join("cache").exists());
    }
    #[test]
    fn archive_pump_copies_data_within_the_limit() {
        let source = io::Cursor::new(b"bounded archive".to_vec());
        let mut destination = Vec::new();
        let transferred = pump_archive(
            source,
            &mut destination,
            Instant::now() + Duration::from_secs(1),
            1024,
        )
        .unwrap();

        assert_eq!(transferred, 15);
        assert_eq!(destination, b"bounded archive");
    }

    #[test]
    fn archive_pump_rejects_streams_over_the_fixed_limit() {
        let source = io::Cursor::new(vec![b'x'; 17]);
        let mut destination = Vec::new();
        let error = pump_archive(
            source,
            &mut destination,
            Instant::now() + Duration::from_secs(1),
            16,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ArchivePumpError::Limit {
                limit: 16,
                consumed: 17
            }
        ));
        assert!(destination.len() <= 16);
    }

    #[test]
    fn archive_pump_rejects_an_expired_deadline() {
        let source = io::Cursor::new(b"archive".to_vec());
        let mut destination = Vec::new();
        let error = pump_archive(source, &mut destination, Instant::now(), 1024).unwrap_err();

        assert!(matches!(error, ArchivePumpError::Deadline));
        assert!(destination.is_empty());
    }

    #[test]
    fn stderr_collection_drains_input_but_retains_only_a_bounded_prefix() {
        let output = read_bounded_stderr(io::Cursor::new(vec![b'e'; 128]), 16).unwrap();

        assert_eq!(output.bytes, vec![b'e'; 16]);
        assert!(output.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn archive_pipeline_deadline_terminates_both_children() {
        let mut copy = Command::new("sleep");
        copy.arg("10");
        let mut extract = Command::new("sleep");
        extract.arg("10");
        let started = Instant::now();

        let error = run_archive_pipeline(&mut copy, &mut extract, Duration::from_millis(30), 1024)
            .unwrap_err();

        assert!(error.to_string().contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn failed_atomic_population_leaves_the_original_destination_empty() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("destination");
        std::fs::create_dir(&destination).unwrap();

        let error = populate_empty_directory_atomically(&destination, |staging| {
            std::fs::write(staging.join("partial"), "must not publish")?;
            anyhow::bail!("simulated export failure")
        })
        .unwrap_err();

        assert!(error.to_string().contains("simulated export failure"));
        assert!(destination.is_dir());
        assert!(std::fs::read_dir(&destination).unwrap().next().is_none());
        assert!(std::fs::read_dir(parent.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp-workspace-export-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn successful_atomic_population_publishes_only_the_complete_tree() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("destination");
        std::fs::create_dir(&destination).unwrap();

        populate_empty_directory_atomically(&destination, |staging| {
            std::fs::create_dir(staging.join("nested"))?;
            std::fs::write(staging.join("nested/result"), "complete")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(destination.join("nested/result")).unwrap(),
            "complete"
        );
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(destination.join("nested/result"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn container_export_refuses_to_merge_into_existing_snapshot() {
        let destination = tempfile::tempdir().unwrap();
        let existing = destination.path().join("existing.txt");
        std::fs::write(&existing, "preserve me").unwrap();
        let error = export_container_tree("unused", "/app", destination.path()).unwrap_err();
        assert!(error.to_string().contains("must be empty"));
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "preserve me");
    }

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

    #[test]
    fn run_directories_are_unique_for_sequential_suite_members() {
        let root = Path::new("workspaces");
        let first = unique_run_directory(root, "task");
        let second = unique_run_directory(root, "task");
        assert_ne!(first, second);
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
        use std::os::unix::fs::{symlink, PermissionsExt};

        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("packages")).unwrap();
        std::fs::write(source.path().join("packages/mathlib"), "package").unwrap();
        symlink("packages/mathlib", source.path().join("mathlib")).unwrap();
        std::fs::write(source.path().join("README"), "hello").unwrap();

        let destination = tempfile::tempdir().unwrap();
        std::fs::set_permissions(destination.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        clone_tree(source.path(), destination.path()).unwrap();

        assert_eq!(
            std::fs::metadata(destination.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
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
