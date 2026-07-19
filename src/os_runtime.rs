use crate::{
    asset::LocalAssetPackage,
    runtime::{JudgeResult, RuntimeStatus},
    state_fs,
    task::TaskInfo,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use reqwest::blocking::{Client, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod auth;
mod scripts;

pub const PROVIDER: &str = "os-runtime";

const DEFAULT_NODE_IMAGE: &str = "registry.a3s.io/a3s/shared-runtime-node-20@sha256:c0eae0d86b6c3df85f7b1a6aedb55279fa4f9dd04025a45dda67b8d384f668dd";
const DEFAULT_PYTHON_IMAGE: &str = "registry.a3s.io/a3s/shared-runtime-python-3.12@sha256:9e7add669d18c9158f134051365eb28f4577139a7674a42211c4707e82335d96";
pub(crate) const CANDIDATE_IMAGE_KEY: &str = "os-runtime|candidate";
pub(crate) const JUDGE_IMAGE_KEY: &str = "os-runtime|judge";
const RESULT_MARKER: &str = "A3S_BENCH_RESULT_V1:";
const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
const MAX_ENCODED_RESULT_BYTES: usize = MAX_ENVELOPE_BYTES.div_ceil(3) * 4;
const MAX_STEP_TIMEOUT_MS: u64 = 600_000;
const RESULT_LOG_TAIL_LINES: u16 = 2_000;
static INVOCATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteFile {
    path: String,
    data: String,
    executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteTree {
    files: Vec<RemoteFile>,
}

struct OsClient {
    address: String,
    access_token: String,
    http: Client,
}

pub(crate) fn resolved_runner_images() -> Result<BTreeMap<String, String>> {
    let candidate =
        std::env::var("A3S_BENCH_OS_NODE_IMAGE").unwrap_or_else(|_| DEFAULT_NODE_IMAGE.to_owned());
    let judge = std::env::var("A3S_BENCH_OS_PYTHON_IMAGE")
        .unwrap_or_else(|_| DEFAULT_PYTHON_IMAGE.to_owned());
    validate_digest_pinned_image(&candidate)?;
    validate_digest_pinned_image(&judge)?;
    Ok(BTreeMap::from([
        (CANDIDATE_IMAGE_KEY.to_owned(), candidate),
        (JUDGE_IMAGE_KEY.to_owned(), judge),
    ]))
}

fn locked_runner_image<'a>(images: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    let image = images
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("TaskLock does not bind managed image {key:?}"))?;
    validate_digest_pinned_image(image)?;
    Ok(image)
}

fn validate_digest_pinned_image(value: &str) -> Result<()> {
    let (name, digest) = value
        .split_once("@sha256:")
        .ok_or_else(|| anyhow::anyhow!("managed os-runtime image must use @sha256:<digest>"))?;
    anyhow::ensure!(
        !name.is_empty()
            && !name.contains('@')
            && !name.bytes().any(|byte| byte.is_ascii_whitespace()),
        "managed os-runtime image name is invalid"
    );
    anyhow::ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "managed os-runtime image digest must contain 64 lowercase hexadecimal characters"
    );
    Ok(())
}

pub fn preflight() -> Result<RuntimeStatus> {
    let client = OsClient::connect()?;
    client.get("/api/v1/runtime/functions/invocations?page=1&limit=1")?;
    Ok(RuntimeStatus {
        provider: PROVIDER.to_owned(),
        ready: true,
        detail: format!("authenticated A3S OS at {}", client.address),
    })
}

pub fn execute_candidate(
    task: &TaskInfo,
    candidate: &LocalAssetPackage,
    workspace: &Path,
    resolved_images: &BTreeMap<String, String>,
) -> Result<()> {
    let entrypoint = candidate
        .entrypoint
        .split(':')
        .next()
        .unwrap_or(&candidate.entrypoint);
    validate_relative_path(entrypoint)?;
    let timeout_ms = step_timeout_ms(task.candidate_timeout_sec)?;
    let input = json!({
        "candidate": RemoteTree::capture(&candidate.root)?,
        "workspace": RemoteTree::capture(workspace)?,
        "entrypoint": entrypoint,
        "timeoutMs": timeout_ms,
    });
    ensure_payload_size(&input)?;
    let image = locked_runner_image(resolved_images, CANDIDATE_IMAGE_KEY)?;
    let logs = OsClient::connect()?.run_step(
        image,
        vec!["node".into(), "-e".into(), scripts::CANDIDATE.into()],
        input,
        timeout_ms,
    )?;
    let tree: RemoteTree = parse_marked_result(&logs)?;
    tree.replace(workspace)
        .context("could not materialize os-runtime Candidate workspace")
}

pub fn execute_judge(
    task: &TaskInfo,
    judge: &LocalAssetPackage,
    submission: &Path,
    resolved_images: &BTreeMap<String, String>,
) -> Result<JudgeResult> {
    let hidden = task.root.join("private/bundle").canonicalize()?;
    let (entrypoint_file, entrypoint_function) = judge
        .entrypoint
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("Judge entrypoint must use file.py:function form"))?;
    validate_relative_path(entrypoint_file)?;
    anyhow::ensure!(
        !entrypoint_function.is_empty()
            && entrypoint_function
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "Judge entrypoint function is invalid"
    );
    let timeout_ms = step_timeout_ms(task.candidate_timeout_sec)?;
    let input = json!({
        "judge": RemoteTree::capture(&judge.root)?,
        "submission": RemoteTree::capture(submission)?,
        "hidden": RemoteTree::capture(&hidden)?,
        "entrypointFile": entrypoint_file,
        "entrypointFunction": entrypoint_function,
    });
    ensure_payload_size(&input)?;
    let image = locked_runner_image(resolved_images, JUDGE_IMAGE_KEY)?;
    let logs = OsClient::connect()?.run_step(
        image,
        vec!["python3".into(), "-c".into(), scripts::JUDGE.into()],
        input,
        timeout_ms,
    )?;
    let result: JudgeResult = parse_marked_result(&logs)?;
    anyhow::ensure!(
        result.schema == "bench.judge.result.v1",
        "Judge returned unsupported schema {}",
        result.schema
    );
    crate::runtime::validate_judge_result(task, &result)?;
    Ok(result)
}

impl RemoteTree {
    fn capture(root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        let mut files = Vec::new();
        capture_directory(&root, &root, &mut files)?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Self { files })
    }

    fn replace(&self, root: &Path) -> Result<()> {
        if root.exists() {
            for entry in std::fs::read_dir(root)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    std::fs::remove_dir_all(entry.path())?;
                } else {
                    std::fs::remove_file(entry.path())?;
                }
            }
        }
        state_fs::secure_directory(root)?;
        for file in &self.files {
            validate_relative_path(&file.path)?;
            let target = root.join(&file.path);
            anyhow::ensure!(target.starts_with(root), "remote file escapes workspace");
            if let Some(parent) = target.parent() {
                state_fs::secure_directory(parent)?;
            }
            let bytes = BASE64
                .decode(&file.data)
                .context("os-runtime returned invalid base64 file data")?;
            std::fs::write(&target, bytes)?;
            state_fs::set_owner_only_file(&target, file.executable)?;
        }
        Ok(())
    }
}

fn capture_directory(root: &Path, directory: &Path, files: &mut Vec<RemoteFile>) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(!kind.is_symlink(), "os-runtime payload contains a symlink");
        if kind.is_dir() {
            capture_directory(root, &entry.path(), files)?;
        } else if kind.is_file() {
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            let path = portable_relative_path(&relative)?;
            let data = BASE64.encode(std::fs::read(entry.path())?);
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                entry.metadata()?.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            files.push(RemoteFile {
                path,
                data,
                executable,
            });
        } else {
            anyhow::bail!("os-runtime payload contains a special file");
        }
    }
    Ok(())
}

fn portable_relative_path(path: &Path) -> Result<String> {
    let mut values = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => values.push(
                value
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("path is not UTF-8"))?,
            ),
            _ => anyhow::bail!("path is not a closed relative path"),
        }
    }
    anyhow::ensure!(!values.is_empty(), "relative path is empty");
    Ok(values.join("/"))
}

fn validate_relative_path(value: &str) -> Result<()> {
    portable_relative_path(Path::new(value)).map(|_| ())
}

fn ensure_payload_size(value: &Value) -> Result<()> {
    let actual = serde_json::to_vec(value)?.len();
    anyhow::ensure!(
        actual <= MAX_ENVELOPE_BYTES,
        "os-runtime payload is {actual} bytes; maximum is {MAX_ENVELOPE_BYTES}; use Docker or an artifact-backed Runtime for larger workspaces"
    );
    Ok(())
}

fn step_timeout_ms(seconds: u64) -> Result<u64> {
    let value = seconds
        .checked_mul(1_000)
        .ok_or_else(|| anyhow::anyhow!("Task timeout is too large"))?;
    anyhow::ensure!(
        value <= MAX_STEP_TIMEOUT_MS,
        "os-runtime step timeout cannot exceed 600 seconds"
    );
    Ok(value)
}

impl OsClient {
    fn connect() -> Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(120))
            .build()?;
        let (address, access_token) = auth::credentials(&http)?;
        Ok(Self {
            address,
            access_token,
            http,
        })
    }

    fn get(&self, path: &str) -> Result<Value> {
        response_json(
            self.http
                .get(format!("{}{}", self.address, path))
                .bearer_auth(&self.access_token)
                .send()?,
        )
    }

    fn run_step(
        &self,
        image: &str,
        command: Vec<String>,
        input: Value,
        timeout_ms: u64,
    ) -> Result<String> {
        let idempotency_key = format!(
            "a3s-bench-{}-{}-{}",
            unix_time_nanos()?,
            std::process::id(),
            INVOCATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let body = json!({
            "image": image,
            "input": input,
            "command": command,
            "io": {"outputJsonPath": "/tmp/a3s-bench-unused.json"},
            "timeoutMs": timeout_ms,
            "idempotencyKey": idempotency_key,
        });
        let accepted = response_json(
            self.http
                .post(format!("{}/api/v1/runtime/functions/steps", self.address))
                .bearer_auth(&self.access_token)
                .json(&body)
                .send()?,
        )?;
        let invocation_id = data_field(&accepted, "invocationId")?
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("A3S OS returned an invalid invocationId"))?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms + 60_000);
        loop {
            let status = self.get(&format!(
                "/api/v1/runtime/functions/invocations/{invocation_id}"
            ))?;
            let state = data_field(&status, "state")?.as_str().unwrap_or("unknown");
            if matches!(state, "succeeded" | "failed" | "canceled") {
                let logs = self.get(&format!(
                    "/api/v1/runtime/functions/invocations/{invocation_id}/logs?tailLines={RESULT_LOG_TAIL_LINES}"
                ))?;
                let text = data_field(&logs, "logs")?
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                anyhow::ensure!(
                    state == "succeeded",
                    "A3S OS invocation {invocation_id} ended as {state}: {}",
                    text.trim()
                );
                return Ok(text);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "A3S OS invocation {invocation_id} did not reach a terminal state"
            );
            std::thread::sleep(Duration::from_millis(750));
        }
    }
}

fn unix_time_nanos() -> Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_nanos())
}

fn response_json(response: Response) -> Result<Value> {
    let status = response.status();
    let bytes = response.bytes()?;
    let value: Value = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "A3S OS returned non-JSON HTTP {status}: {}",
            String::from_utf8_lossy(&bytes).trim()
        )
    })?;
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("request failed");
    let details = value
        .get("details")
        .filter(|details| !details.is_null())
        .map(|details| format!("; details={details}"))
        .unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "A3S OS HTTP {status}: {message}{details}"
    );
    Ok(value)
}

fn data_field<'a>(value: &'a Value, name: &str) -> Result<&'a Value> {
    value
        .get("data")
        .and_then(|data| data.get(name))
        .or_else(|| value.get(name))
        .ok_or_else(|| anyhow::anyhow!("A3S OS response is missing {name:?}"))
}

fn parse_marked_result<T: DeserializeOwned>(logs: &str) -> Result<T> {
    let encoded = logs
        .lines()
        .rev()
        .find_map(|line| {
            line.find(RESULT_MARKER)
                .map(|index| &line[index + RESULT_MARKER.len()..])
        })
        .ok_or_else(|| anyhow::anyhow!("A3S OS logs do not contain a Bench result marker"))?;
    let encoded = encoded.trim();
    anyhow::ensure!(
        encoded.len() <= MAX_ENCODED_RESULT_BYTES,
        "A3S OS Bench result exceeds the 64 KiB envelope"
    );
    let decoded = BASE64
        .decode(encoded)
        .context("A3S OS Bench result marker is not base64")?;
    anyhow::ensure!(
        decoded.len() <= MAX_ENVELOPE_BYTES,
        "A3S OS Bench result exceeds the 64 KiB envelope"
    );
    serde_json::from_slice(&decoded).context("A3S OS Bench result marker contains invalid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_result_marker_from_prefixed_logs() {
        let encoded = BASE64.encode(br#"{"files":[]}"#);
        let logs = format!("noise\npod: {RESULT_MARKER}{encoded}\n");
        let value: RemoteTree = parse_marked_result(&logs).unwrap();
        assert!(value.files.is_empty());
    }

    #[test]
    fn rejects_results_larger_than_the_envelope() {
        let encoded = BASE64.encode(vec![b'x'; MAX_ENVELOPE_BYTES + 1]);
        let logs = format!("{RESULT_MARKER}{encoded}");
        let error = parse_marked_result::<Value>(&logs).unwrap_err();
        assert!(format!("{error:#}").contains("exceeds the 64 KiB envelope"));
    }

    #[test]
    fn managed_images_must_be_digest_pinned() {
        assert!(validate_digest_pinned_image(DEFAULT_NODE_IMAGE).is_ok());
        assert!(validate_digest_pinned_image(DEFAULT_PYTHON_IMAGE).is_ok());
        for image in [
            "registry.example.test/runner:latest",
            "registry.example.test/runner@sha256:abc",
            "registry.example.test/runner@sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "registry.example.test/runner@sha256:gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
        ] {
            assert!(validate_digest_pinned_image(image).is_err(), "{image}");
        }
    }

    #[test]
    fn remote_tree_round_trip_preserves_files() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("nested")).unwrap();
        std::fs::write(source.path().join("nested/answer.txt"), b"42\n").unwrap();
        let tree = RemoteTree::capture(source.path()).unwrap();
        let destination = tempfile::tempdir().unwrap();
        tree.replace(destination.path()).unwrap();
        assert_eq!(
            std::fs::read(destination.path().join("nested/answer.txt")).unwrap(),
            b"42\n"
        );
    }

    #[test]
    fn rejects_paths_that_escape_the_envelope() {
        assert!(validate_relative_path("../secret").is_err());
        assert!(validate_relative_path("/absolute").is_err());
        assert!(validate_relative_path("safe/file").is_ok());
    }
}
