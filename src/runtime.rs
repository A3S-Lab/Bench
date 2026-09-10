use crate::{asset::LocalAssetPackage, task::TaskInfo};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::runtime_selection::{RuntimeSelection, A3S_BOX_PROVIDER, DOCKER_PROVIDER};

const IMAGE_PULL_ATTEMPTS: u32 = 3;
const IMAGE_PULL_BASE_DELAY: Duration = Duration::from_secs(5);
const WORKSPACE_IMPORT_STAGING_TIMEOUT: Duration = Duration::from_secs(600);
const WORKSPACE_IMPORT_STAGING_ROOT: &str = "/run/a3s-workspace-import";

struct ImagePullAttempt {
    succeeded: bool,
    stderr: String,
}

/// Pull a Docker image with exponential backoff so a transient registry or
/// network failure does not abort an otherwise valid benchmark run.
pub fn pull_image_with_retry(reference: &str, platform: Option<&str>) -> Result<()> {
    retry_image_pull(
        reference,
        IMAGE_PULL_ATTEMPTS,
        IMAGE_PULL_BASE_DELAY,
        || {
            let output = docker_pull_command(reference, platform)
                .output()
                .context("could not start Docker image pull")?;
            Ok(ImagePullAttempt {
                succeeded: output.status.success(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        },
        std::thread::sleep,
    )
}

fn docker_pull_command(reference: &str, platform: Option<&str>) -> Command {
    let mut command = Command::new("docker");
    command.arg("pull");
    if let Some(platform) = platform {
        command.args(["--platform", platform]);
    }
    command.arg(reference);
    command
}

fn retry_image_pull<P, S>(
    reference: &str,
    max_attempts: u32,
    base_delay: Duration,
    mut pull: P,
    mut sleep: S,
) -> Result<()>
where
    P: FnMut() -> Result<ImagePullAttempt>,
    S: FnMut(Duration),
{
    anyhow::ensure!(max_attempts > 0, "Docker pull attempts must be positive");
    let mut last_error = String::new();
    for attempt in 1..=max_attempts {
        let outcome = pull()?;
        if outcome.succeeded {
            return Ok(());
        }
        last_error = outcome.stderr;
        if attempt < max_attempts {
            let delay = base_delay * 2u32.pow(attempt - 1);
            eprintln!(
                "Docker pull attempt {attempt}/{max_attempts} for {reference:?} failed: {last_error}"
            );
            eprintln!("Retrying in {delay:?}...");
            sleep(delay);
        }
    }
    anyhow::bail!(
        "could not pull Docker image {reference:?} after {max_attempts} attempts: {last_error}"
    )
}

static CANDIDATE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static WORKSPACE_IMPORT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct PreparedWorkspaceImport {
    name: String,
    target_path: String,
    volume: String,
}

/// Per-run resources copied from the locked Work image and mounted into the
/// Candidate container. The volumes are never shared between runs.
#[derive(Debug, Default)]
pub struct PreparedWorkspaceImports {
    imports: Vec<PreparedWorkspaceImport>,
}

impl PreparedWorkspaceImports {
    pub(crate) fn add_mounts(&self, command: &mut Command, workspace: &str) {
        for spec in self.mount_specs(workspace) {
            command.arg("--mount").arg(spec);
        }
    }

    pub(crate) fn mount_specs(&self, workspace: &str) -> Vec<String> {
        self.imports
            .iter()
            .map(|import| {
                format!(
                    "type=volume,src={},dst={},volume-nocopy",
                    import.volume,
                    resolve_workspace_import_target(workspace, &import.target_path)
                )
            })
            .collect()
    }

    pub(crate) fn add_environment(&self, command: &mut Command, workspace: &str) {
        for (name, value) in self.environment(workspace) {
            command.arg("--env").arg(format!("{name}={value}"));
        }
    }

    pub(crate) fn environment(&self, workspace: &str) -> Vec<(String, String)> {
        self.imports
            .iter()
            .filter(|import| import.name == "maven_repository")
            .map(|import| {
                (
                    "MAVEN_OPTS".to_owned(),
                    format!(
                        "-Dmaven.repo.local={}",
                        resolve_workspace_import_target(workspace, &import.target_path)
                    ),
                )
            })
            .collect()
    }
}

fn resolve_workspace_import_target(workspace: &str, target_path: &str) -> String {
    if target_path.starts_with('/') {
        target_path.to_owned()
    } else {
        format!("{}/{}", workspace.trim_end_matches('/'), target_path)
    }
}

impl Drop for PreparedWorkspaceImports {
    fn drop(&mut self) {
        for import in self.imports.iter().rev() {
            let _ = Command::new("docker")
                .args(["volume", "rm", "-f", &import.volume])
                .output();
        }
    }
}

pub fn prepare_workspace_imports(task: &TaskInfo) -> Result<PreparedWorkspaceImports> {
    let mut prepared = PreparedWorkspaceImports::default();
    for import in &task.work_workspace_imports {
        let sequence = WORKSPACE_IMPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_nanos();
        let volume = format!(
            "a3s-bench-workspace-import-{}-{created_at}-{sequence}",
            std::process::id(),
        );
        let created = Command::new("docker")
            .args(["volume", "create", &volume])
            .output()
            .context("could not create Candidate workspace import volume")?;
        anyhow::ensure!(
            created.status.success(),
            "could not create Candidate workspace import volume: {}",
            String::from_utf8_lossy(&created.stderr).trim()
        );
        prepared.imports.push(PreparedWorkspaceImport {
            name: import.name.clone(),
            target_path: import.target_path.clone(),
            volume: volume.clone(),
        });

        let container = format!("{volume}-stage");
        let script = format!(
            "set -eu; source_path=\u{24}1; [ -d \"\u{24}source_path\" ]; [ -z \"\u{24}(find \"\u{24}source_path\" -mindepth 1 ! -type d ! -type f -print -quit)\" ]; cp -a \"\u{24}source_path\"/. {WORKSPACE_IMPORT_STAGING_ROOT}/; chmod -R a+rwX {WORKSPACE_IMPORT_STAGING_ROOT}"
        );
        let mut command = Command::new("docker");
        command.args([
            "run",
            "--rm",
            "--pull",
            "never",
            "--name",
            &container,
            "--network",
            "none",
            "--pids-limit",
            "128",
            "--memory",
            "1g",
            "--cpus",
            "1",
            "--user",
            "0:0",
        ]);
        if let Some(platform) = task.work_platform.as_deref() {
            command.args(["--platform", platform]);
        }
        command
            .arg("--mount")
            .arg(format!(
                "type=volume,src={volume},dst={WORKSPACE_IMPORT_STAGING_ROOT},volume-nocopy"
            ))
            .args(["--entrypoint", "/bin/sh"])
            .arg(&task.work_image)
            .args(["-c", &script, "--", &import.source_path]);
        let (output, timed_out) =
            output_with_timeout(&mut command, WORKSPACE_IMPORT_STAGING_TIMEOUT)
                .context("could not stage Candidate workspace import")?;
        if timed_out {
            let _ = Command::new("docker")
                .args(["rm", "-f", &container])
                .output();
            anyhow::bail!(
                "Candidate workspace import {:?} staging timed out",
                import.name
            );
        }
        anyhow::ensure!(
            output.status.success(),
            "could not import {:?} from locked Work image: {}",
            import.name,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(prepared)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateProcessOutcome {
    Completed,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedImage {
    pub source: String,
    pub immutable_ref: String,
    pub image_id: String,
}

pub fn resolve_image(reference: &str, platform: Option<&str>) -> Result<ResolvedImage> {
    if !local_image_matches(reference, platform)? {
        pull_image_with_retry(reference, platform)?;
    }
    anyhow::ensure!(
        local_image_matches(reference, platform)?,
        "Docker image {reference:?} does not match requested platform {}",
        platform.unwrap_or("native")
    );
    let image_id = command_preflight_output(
        "docker",
        &["image", "inspect", "--format", "{{.Id}}", reference],
    )?;
    anyhow::ensure!(
        image_id.starts_with("sha256:"),
        "Docker returned invalid image ID"
    );
    let repo_digest = command_preflight_output(
        "docker",
        &[
            "image",
            "inspect",
            "--format",
            "{{if .RepoDigests}}{{index .RepoDigests 0}}{{end}}",
            reference,
        ],
    )?;
    Ok(ResolvedImage {
        source: reference.to_owned(),
        immutable_ref: if repo_digest.is_empty() {
            image_id.clone()
        } else {
            repo_digest
        },
        image_id,
    })
}

fn local_image_matches(reference: &str, requested: Option<&str>) -> Result<bool> {
    let output = Command::new("docker")
        .args([
            "image",
            "inspect",
            "--format",
            "{{.Os}}/{{.Architecture}}",
            reference,
        ])
        .output()
        .context("could not inspect Docker image")?;
    if !output.status.success() {
        return Ok(false);
    }
    let Some(requested) = requested else {
        return Ok(true);
    };
    let requested = canonical_platform(requested)?;
    let actual = String::from_utf8(output.stdout)?.trim().to_owned();
    Ok(actual == requested)
}

fn canonical_platform(value: &str) -> Result<String> {
    let mut parts = value.split('/');
    let os = parts.next().unwrap_or_default();
    let architecture = parts.next().unwrap_or_default();
    anyhow::ensure!(
        !os.is_empty() && !architecture.is_empty() && parts.next().is_none(),
        "platform must use os/architecture"
    );
    let architecture = match architecture {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        value => value,
    };
    Ok(format!("{os}/{architecture}"))
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub provider: String,
    pub ready: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeResult {
    pub schema: String,
    pub solution_verdict: String,
    pub metrics: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub diagnostics: serde_json::Value,
}

pub fn execute_docker_candidate(
    task: &TaskInfo,
    candidate: &LocalAssetPackage,
    workspace: &Path,
    workspace_imports: &PreparedWorkspaceImports,
) -> Result<CandidateProcessOutcome> {
    let entrypoint = candidate
        .entrypoint
        .split(':')
        .next()
        .unwrap_or(&candidate.entrypoint);
    let mut command = Command::new("docker");
    let container = format!(
        "a3s-bench-candidate-{}-{}",
        std::process::id(),
        CANDIDATE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    command.args(["run", "--rm", "--name", &container]);
    command.args(crate::runtime_profile::work_docker_args(
        task.resources.work,
    ));
    let public_internet = task.work_network_need == "public_internet";
    command.args(["--network", if public_internet { "bridge" } else { "none" }]);
    if public_internet {
        command.args(crate::runtime_profile::host_dns_args());
    }
    if let Some(platform) = task.work_platform.as_deref() {
        command.args(["--platform", platform]);
    }
    configure_mounted_tree_owner(&mut command, &candidate.root)?;
    workspace_imports.add_environment(&mut command, "/workspace");
    command.arg("--mount").arg(format!(
        "type=bind,src={},dst=/workspace",
        workspace.display()
    ));
    workspace_imports.add_mounts(&mut command, "/workspace");
    command
        .arg("--mount")
        .arg(format!(
            "type=bind,src={},dst=/agent,readonly",
            candidate.root.display()
        ))
        .arg(&task.work_image)
        .args(["/bin/sh", &format!("/agent/{entrypoint}"), "/workspace"]);
    let (candidate_output, timed_out) = output_with_timeout(
        &mut command,
        Duration::from_secs(task.candidate_timeout_sec),
    )
    .context("could not start Docker Candidate")?;
    if timed_out {
        let _ = Command::new("docker")
            .args(["rm", "-f", &container])
            .output();
        return Ok(CandidateProcessOutcome::TimedOut);
    }
    anyhow::ensure!(
        candidate_output.status.success(),
        "Candidate exited with {}: {}",
        candidate_output.status,
        String::from_utf8_lossy(&candidate_output.stderr).trim()
    );
    Ok(CandidateProcessOutcome::Completed)
}

fn output_with_timeout(command: &mut Command, timeout: Duration) -> Result<(Output, bool)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut stdout_file = tempfile::tempfile()?;
    let mut stderr_file = tempfile::tempfile()?;
    let mut child = command
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::from(stderr_file.try_clone()?))
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            break true;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let status = child.wait()?;
    stdout_file.seek(SeekFrom::Start(0))?;
    stderr_file.seek(SeekFrom::Start(0))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    stdout_file.read_to_end(&mut stdout)?;
    stderr_file.read_to_end(&mut stderr)?;
    Ok((
        Output {
            status,
            stdout,
            stderr,
        },
        timed_out,
    ))
}

pub fn execute_docker_judge(
    task: &TaskInfo,
    judge: &LocalAssetPackage,
    submission: &Path,
) -> Result<JudgeResult> {
    let hidden_root = task.root.join("private/bundle").canonicalize()?;
    let (entrypoint_file, entrypoint_function) = judge
        .entrypoint
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("Judge entrypoint must use file.py:function form"))?;
    anyhow::ensure!(
        entrypoint_file.ends_with(".py")
            && !entrypoint_file.starts_with('/')
            && !entrypoint_file.contains(".."),
        "Judge entrypoint file is invalid"
    );
    anyhow::ensure!(
        !entrypoint_function.is_empty()
            && entrypoint_function
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "Judge entrypoint function is invalid"
    );
    let script = format!(
        "import importlib.util,json\n\
spec=importlib.util.spec_from_file_location('judge',{})\n\
mod=importlib.util.module_from_spec(spec);spec.loader.exec_module(mod)\n\
print(json.dumps(getattr(mod,{})({{'submission_root':'/submission','hidden_bundle_root':'/hidden'}}),separators=(',',':')))",
        serde_json::to_string(&format!("/judge/{entrypoint_file}"))?,
        serde_json::to_string(entrypoint_function)?
    );
    let mut command = Command::new("docker");
    command.args(["run", "--rm", "--network", "none"]);
    command.args(crate::runtime_profile::judge_docker_args(
        task.resources.judge,
    ));
    command.args(crate::runtime_profile::READ_ONLY_JUDGE_TMPFS);
    configure_mounted_tree_owner(&mut command, &judge.root)?;
    let output = command
        .arg("--mount")
        .arg(format!(
            "type=bind,src={},dst=/submission,readonly",
            submission.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,src={},dst=/hidden,readonly",
            hidden_root.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,src={},dst=/judge,readonly",
            judge.root.display()
        ))
        .args(["python:3.12-alpine", "python", "-c", &script])
        .output()
        .context("could not start Docker Judge")?;
    anyhow::ensure!(
        output.status.success(),
        "Judge failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: JudgeResult =
        serde_json::from_slice(&output.stdout).context("Judge returned invalid JSON")?;
    anyhow::ensure!(
        result.schema == "bench.judge.result.v1",
        "Judge returned unsupported schema {}",
        result.schema
    );
    validate_judge_result(task, &result)?;
    Ok(result)
}

fn configure_mounted_tree_owner(command: &mut Command, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path)?;
        command.args(["--user", &format!("{}:{}", metadata.uid(), metadata.gid())]);
    }
    #[cfg(not(unix))]
    {
        let _ = (command, path);
    }
    Ok(())
}

pub(crate) fn validate_judge_result(task: &TaskInfo, result: &JudgeResult) -> Result<()> {
    anyhow::ensure!(
        result.solution_verdict == "valid",
        "Judge solution_verdict must be \"valid\""
    );
    anyhow::ensure!(
        result.metrics.len() == task.metrics.len(),
        "Judge metric set does not match Task"
    );
    for metric in &task.metrics {
        let value = result
            .metrics
            .get(&metric.name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("Judge metric {:?} must be a decimal string", metric.name)
            })?;
        anyhow::ensure!(
            canonical_decimal(value),
            "Judge metric {:?} is not canonical",
            metric.name
        );
        let number: f64 = value.parse().context("invalid Judge metric number")?;
        anyhow::ensure!(
            number.is_finite() && number >= metric.min && number <= metric.max,
            "Judge metric {:?} is outside [{}, {}]",
            metric.name,
            metric.min,
            metric.max
        );
    }
    Ok(())
}

pub(crate) fn canonical_decimal(value: &str) -> bool {
    if value == "0" {
        return true;
    }
    let value = value.strip_prefix('-').unwrap_or(value);
    let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
    !integer.is_empty()
        && integer.bytes().all(|byte| byte.is_ascii_digit())
        && (integer == "0" || !integer.starts_with('0'))
        && (fraction.is_empty()
            || (fraction.bytes().all(|byte| byte.is_ascii_digit()) && !fraction.ends_with('0')))
}

pub fn preflight(selection: &RuntimeSelection) -> Result<RuntimeStatus> {
    match selection.provider.as_str() {
        DOCKER_PROVIDER => docker_preflight(),
        A3S_BOX_PROVIDER => command_preflight("a3s-box", &["--version"], "a3s-box"),
        crate::os_runtime::PROVIDER => crate::os_runtime::preflight(),
        provider => Err(anyhow::anyhow!(
            "configured Runtime provider {provider:?} is not installed; provider selection never falls back to Docker"
        )),
    }
}

fn docker_preflight() -> Result<RuntimeStatus> {
    command_preflight(
        "docker",
        &["version", "--format", "{{.Server.Version}}"],
        "docker",
    )
}

fn command_preflight(command: &str, args: &[&str], provider: &str) -> Result<RuntimeStatus> {
    let output = Command::new(command).args(args).output().with_context(|| {
        format!("Runtime provider {provider:?} is unavailable: could not run {command}")
    })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(anyhow::anyhow!(
            "Runtime provider {provider:?} failed preflight{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let detail = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(RuntimeStatus {
        provider: provider.to_owned(),
        ready: true,
        detail,
    })
}

fn command_preflight_output(command: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(command).args(args).output()?;
    anyhow::ensure!(
        output.status.success(),
        "{} failed: {}",
        command,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::MetricInfo;

    #[test]
    fn canonical_metric_numbers() {
        for value in ["0", "1", "-1", "0.5", "12.34"] {
            assert!(canonical_decimal(value), "{value}");
        }
        for value in ["", "00", "01", "1.0", ".5", "1e2", "+1"] {
            assert!(!canonical_decimal(value), "{value}");
        }
    }

    #[test]
    fn canonicalizes_closed_container_platforms() {
        assert_eq!(canonical_platform("linux/x86_64").unwrap(), "linux/amd64");
        assert_eq!(canonical_platform("linux/aarch64").unwrap(), "linux/arm64");
        assert!(canonical_platform("linux").is_err());
        assert!(canonical_platform("linux/amd64/v2").is_err());
    }

    #[test]
    fn docker_pull_command_keeps_reference_and_platform_as_arguments() {
        let command = docker_pull_command("registry.example/image:tag", Some("linux/amd64"));
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "pull",
                "--platform",
                "linux/amd64",
                "registry.example/image:tag"
            ]
        );
    }

    #[test]
    fn docker_pull_retries_with_exponential_backoff() {
        let mut attempts = 0;
        let mut delays = Vec::new();
        retry_image_pull(
            "example.invalid/image:tag",
            3,
            Duration::from_millis(5),
            || {
                attempts += 1;
                Ok(ImagePullAttempt {
                    succeeded: attempts == 3,
                    stderr: "temporary registry failure".into(),
                })
            },
            |delay| delays.push(delay),
        )
        .unwrap();

        assert_eq!(attempts, 3);
        assert_eq!(
            delays,
            [Duration::from_millis(5), Duration::from_millis(10)]
        );
    }

    #[test]
    fn docker_pull_reports_the_last_failure() {
        let error = retry_image_pull(
            "example.invalid/image:tag",
            2,
            Duration::ZERO,
            || {
                Ok(ImagePullAttempt {
                    succeeded: false,
                    stderr: "registry unavailable".into(),
                })
            },
            |_| {},
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("after 2 attempts"));
        assert!(message.contains("registry unavailable"));
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_collects_output_and_terminates() {
        let mut quick = Command::new("sh");
        quick.args(["-c", "printf ready"]);
        let (output, timed_out) = output_with_timeout(&mut quick, Duration::from_secs(1)).unwrap();
        assert!(!timed_out);
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ready");

        let mut slow = Command::new("sleep");
        slow.arg("5");
        let (_, timed_out) = output_with_timeout(&mut slow, Duration::from_millis(50)).unwrap();
        assert!(timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn command_output_does_not_wait_for_descendants_holding_stdio() {
        let mut command = Command::new("sh");
        command.args(["-c", "(sleep 1) & printf ready"]);
        let started = Instant::now();

        let (output, timed_out) =
            output_with_timeout(&mut command, Duration::from_secs(2)).unwrap();

        assert!(!timed_out);
        assert!(started.elapsed() < Duration::from_millis(750));
        assert_eq!(output.stdout, b"ready");
    }

    #[test]
    #[ignore = "requires Docker and the native Alpine image"]
    fn docker_candidate_timeout_preserves_partial_workspace() {
        resolve_image("docker.io/library/alpine:3.20", None).unwrap();
        let candidate_root = tempfile::tempdir().unwrap();
        std::fs::write(
            candidate_root.path().join("run.sh"),
            "#!/bin/sh\nprintf partial > \"$1/partial.txt\"\nsleep 30\n",
        )
        .unwrap();
        let candidate = LocalAssetPackage {
            root: candidate_root.path().canonicalize().unwrap(),
            entrypoint: "run.sh".into(),
            definition_path: None,
            identity: "test-candidate".into(),
            protocol: crate::asset::CandidateProtocol::AgentTool,
        };
        let workspace = tempfile::tempdir().unwrap();
        let task = TaskInfo {
            schema: "a3s-bench/task/v1".into(),
            id: "timeout-test".into(),
            name: "Timeout test".into(),
            category: "correctness".into(),
            judge_asset: "unused".into(),
            work_image: "docker.io/library/alpine:3.20".into(),
            work_platform: None,
            work_network_need: "none".into(),
            work_network_allow_hosts: vec![],
            candidate_timeout_sec: 3,
            metrics: vec![],
            workspace_seed: None,
            submission: crate::task::SubmissionPolicy {
                include: vec!["**".into()],
                exclude: vec![],
                max_files: 100,
                max_total_bytes: 1024,
                max_file_bytes: 1024,
            },
            resources: Default::default(),
            work_workspace_imports: vec![],
            legacy_judge: None,
            root: std::path::PathBuf::new(),
        };

        assert_eq!(
            execute_docker_candidate(
                &task,
                &candidate,
                workspace.path(),
                &PreparedWorkspaceImports::default(),
            )
            .unwrap(),
            CandidateProcessOutcome::TimedOut
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("partial.txt")).unwrap(),
            "partial"
        );
    }

    #[test]
    fn validates_locked_metric_contract() {
        let task = TaskInfo {
            schema: "a3s-bench/task/v1".into(),
            id: "test".into(),
            name: "Test".into(),
            category: "correctness".into(),
            judge_asset: "private/judge".into(),
            work_image: "alpine".into(),
            work_platform: None,
            work_network_need: "none".into(),
            work_network_allow_hosts: vec![],
            candidate_timeout_sec: 300,
            metrics: vec![MetricInfo {
                name: "correctness".into(),
                min: 0.0,
                max: 1.0,
                role: "primary".into(),
            }],
            workspace_seed: None,
            submission: crate::task::SubmissionPolicy {
                include: vec!["**".into()],
                exclude: vec![],
                max_files: 100,
                max_total_bytes: 1024,
                max_file_bytes: 1024,
            },
            resources: Default::default(),
            work_workspace_imports: vec![],
            legacy_judge: None,
            root: std::path::PathBuf::new(),
        };
        let valid = JudgeResult {
            schema: "bench.judge.result.v1".into(),
            solution_verdict: "valid".into(),
            metrics: serde_json::from_value(serde_json::json!({"correctness":"1"})).unwrap(),
            diagnostics: serde_json::json!({}),
        };
        assert!(validate_judge_result(&task, &valid).is_ok());

        let mut invalid = valid;
        invalid
            .metrics
            .insert("correctness".into(), serde_json::json!("2"));
        assert!(validate_judge_result(&task, &invalid).is_err());
    }

    #[test]
    fn workspace_import_mount_and_maven_environment_resolve_absolute_and_relative_targets() {
        let imports = PreparedWorkspaceImports {
            imports: vec![
                PreparedWorkspaceImport {
                    name: "maven_repository".into(),
                    target_path: "/home/agent/.m2/repository".into(),
                    volume: "maven-volume".into(),
                },
                PreparedWorkspaceImport {
                    name: "other_cache".into(),
                    target_path: ".cache/tool".into(),
                    volume: "relative-volume".into(),
                },
            ],
        };
        assert_eq!(
            imports.mount_specs("/home/workspace/exchange-core"),
            [
                "type=volume,src=maven-volume,dst=/home/agent/.m2/repository,volume-nocopy",
                "type=volume,src=relative-volume,dst=/home/workspace/exchange-core/.cache/tool,volume-nocopy",
            ]
        );
        assert_eq!(
            imports.environment("/home/workspace/exchange-core"),
            [(
                "MAVEN_OPTS".into(),
                "-Dmaven.repo.local=/home/agent/.m2/repository".into()
            )]
        );
        std::mem::forget(imports);
    }
}
