use crate::{asset::LocalAssetPackage, task::TaskInfo};
use a3s_runtime::{ProviderId, RuntimeSelection};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedImage {
    pub source: String,
    pub immutable_ref: String,
    pub image_id: String,
}

pub fn resolve_image(reference: &str, platform: Option<&str>) -> Result<ResolvedImage> {
    let mut inspect = Command::new("docker");
    inspect.args(["image", "inspect", reference]);
    let present = inspect.output().context("could not inspect Docker image")?;
    if !present.status.success() {
        let mut pull = Command::new("docker");
        pull.arg("pull");
        if let Some(platform) = platform {
            pull.args(["--platform", platform]);
        }
        let pull = pull
            .arg(reference)
            .output()
            .context("could not start Docker image pull")?;
        anyhow::ensure!(
            pull.status.success(),
            "could not pull Docker image {reference:?}: {}",
            String::from_utf8_lossy(&pull.stderr).trim()
        );
    }
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
) -> Result<()> {
    let entrypoint = candidate
        .entrypoint
        .split(':')
        .next()
        .unwrap_or(&candidate.entrypoint);
    let mut command = Command::new("docker");
    command.args([
        "run",
        "--rm",
        "--network",
        "none",
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "256",
        "--memory",
        "2g",
        "--cpus",
        "2",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=64m",
    ]);
    if let Some(platform) = task.work_platform.as_deref() {
        command.args(["--platform", platform]);
    }
    configure_mounted_tree_owner(&mut command, &candidate.root)?;
    let candidate_output = command
        .arg("--mount")
        .arg(format!(
            "type=bind,src={},dst=/workspace",
            workspace.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,src={},dst=/agent,readonly",
            candidate.root.display()
        ))
        .arg(&task.work_image)
        .args(["/bin/sh", &format!("/agent/{entrypoint}"), "/workspace"])
        .output()
        .context("could not start Docker Candidate")?;
    anyhow::ensure!(
        candidate_output.status.success(),
        "Candidate exited with {}: {}",
        candidate_output.status,
        String::from_utf8_lossy(&candidate_output.stderr).trim()
    );
    Ok(())
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
    command.args([
        "run",
        "--rm",
        "--network",
        "none",
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "128",
        "--memory",
        "1g",
        "--cpus",
        "1",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=64m",
    ]);
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

fn validate_judge_result(task: &TaskInfo, result: &JudgeResult) -> Result<()> {
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
        ProviderId::DOCKER => docker_preflight(),
        ProviderId::A3S_BOX => command_preflight("a3s-box", &["--version"], "a3s-box"),
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
    fn validates_locked_metric_contract() {
        let task = TaskInfo {
            id: "test".into(),
            name: "Test".into(),
            category: "correctness".into(),
            judge_asset: "private/judge".into(),
            work_image: "alpine".into(),
            work_platform: None,
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
}
