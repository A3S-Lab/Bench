use crate::model_candidate::ModelExecution;
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_EVENT_LINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: u64 = 64 * 1024;
const MINIMUM_CLI_VERSION: semver::Version = semver::Version::new(0, 12, 5);

#[derive(Debug)]
pub enum A3sCodeOutcome {
    Completed(ModelExecution),
    TimedOut,
}

pub struct A3sCodeRequest<'a> {
    pub workspace: &'a Path,
    pub config_path: &'a Path,
    pub instructions: &'a str,
    pub task_prompt: &'a str,
    pub model: Option<&'a str>,
    pub workspace_source_path: Option<&'a str>,
    pub public_internet: bool,
    pub timeout_sec: u64,
}

pub fn version() -> Result<String> {
    version_with(command)
}

fn version_with(mut command: impl FnMut() -> Command) -> Result<String> {
    let output = command()
        .arg("--version")
        .output()
        .context("A3S Code Candidate requires the a3s CLI on PATH")?;
    anyhow::ensure!(
        output.status.success(),
        "could not query A3S CLI version: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let version = String::from_utf8(output.stdout)?.trim().to_owned();
    anyhow::ensure!(!version.is_empty(), "A3S CLI returned an empty version");
    let parsed_version = parse_version(&version)?;
    anyhow::ensure!(
        parsed_version >= MINIMUM_CLI_VERSION,
        "A3S Code Candidate requires A3S CLI {MINIMUM_CLI_VERSION} or newer, found {parsed_version}"
    );

    let help = command()
        .args(["code", "exec", "--help"])
        .output()
        .context("could not inspect the A3S Code execution contract")?;
    let help_text = String::from_utf8_lossy(&help.stdout);
    anyhow::ensure!(
        help.status.success()
            && help_text.contains("--tool-policy")
            && help_text.contains("local-workspace"),
        "installed A3S CLI does not provide the required local-workspace execution policy"
    );
    Ok(version)
}

fn parse_version(output: &str) -> Result<semver::Version> {
    let mut fields = output.split_whitespace();
    anyhow::ensure!(
        fields.next() == Some("a3s"),
        "A3S CLI returned an invalid version"
    );
    let version = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("A3S CLI returned an invalid version"))?;
    semver::Version::parse(version).context("A3S CLI returned an invalid version")
}

pub fn execute(request: A3sCodeRequest<'_>) -> Result<A3sCodeOutcome> {
    execute_with_command(request, command())
}

fn execute_with_command(
    request: A3sCodeRequest<'_>,
    mut process: Command,
) -> Result<A3sCodeOutcome> {
    anyhow::ensure!(
        !request.public_internet,
        "A3S Code local-workspace Candidate does not support Tasks that require public internet"
    );
    let prompt = candidate_prompt(&request);
    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(prompt.as_bytes())?;
    stdin.seek(SeekFrom::Start(0))?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;

    process
        .arg("-C")
        .arg(request.workspace)
        .arg("--config")
        .arg(request.config_path)
        .args([
            "--output",
            "jsonl",
            "--no-progress",
            "--non-interactive",
            "--color",
            "never",
            "code",
            "exec",
            "--mode",
            "auto",
            "--tool-policy",
            "local-workspace",
        ]);
    if let Some(model) = request.model {
        process.args(["--model", model]);
    }

    let mut child = process
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()
        .context("could not start A3S Code Candidate")?;
    let deadline = Instant::now() + Duration::from_secs(request.timeout_sec);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let public_error = read_tail(&mut stdout, MAX_DIAGNOSTIC_BYTES)?;
                let diagnostics = read_tail(&mut stderr, MAX_DIAGNOSTIC_BYTES)?;
                anyhow::bail!(
                    "A3S Code Candidate exited with {status}: stdout={} stderr={}",
                    public_error.trim(),
                    diagnostics.trim()
                );
            }
            return Ok(A3sCodeOutcome::Completed(parse_output(&mut stdout)?));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(A3sCodeOutcome::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn command() -> Command {
    if let Some(path) = std::env::var_os("A3S_BENCH_A3S_BIN") {
        return Command::new(path);
    }
    #[cfg(windows)]
    return Command::new("a3s.exe");
    #[cfg(not(windows))]
    Command::new("a3s")
}

fn candidate_prompt(request: &A3sCodeRequest<'_>) -> String {
    format!(
        "{}\n\n# Benchmark task\n\n{}\n\n# Workspace contract\n\n{}\n\nComplete the task and verify the result.",
        request.instructions,
        request.task_prompt,
        workspace_contract(request.workspace_source_path)
    )
}

fn workspace_contract(source_path: Option<&str>) -> String {
    let Some(source_path) = source_path else {
        return "The current working directory is the editable submission root. Use workspace-relative paths and write deliverables only inside this workspace."
            .to_string();
    };
    let source_name = Path::new(source_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("source directory");
    format!(
        "The current working directory is the extracted contents of `{source_path}` and is already the `{source_name}` directory. When the task names `{source_name}/path`, use the workspace-relative path `path`; do not create another `{source_name}` directory. Write every deliverable inside the current workspace."
    )
}

fn parse_output(stdout: &mut std::fs::File) -> Result<ModelExecution> {
    stdout.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut tool_calls_count = 0usize;
    let mut usage = None;
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        anyhow::ensure!(
            bytes <= MAX_EVENT_LINE_BYTES,
            "A3S Code emitted an oversized JSONL event"
        );
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).context("A3S Code emitted invalid JSONL")?;
        match value.get("type").and_then(Value::as_str) {
            Some("event")
                if value.pointer("/event/type").and_then(Value::as_str)
                    == Some("tool_execution_start") =>
            {
                tool_calls_count = tool_calls_count.saturating_add(1);
            }
            Some("result") => {
                anyhow::ensure!(
                    usage.is_none(),
                    "A3S Code emitted duplicate terminal results"
                );
                anyhow::ensure!(
                    value.get("schemaVersion").and_then(Value::as_u64) == Some(1)
                        && value.get("command").and_then(Value::as_str) == Some("code.exec")
                        && value.get("ok").and_then(Value::as_bool) == Some(true),
                    "A3S Code emitted an invalid terminal result envelope"
                );
                let data = value
                    .get("data")
                    .ok_or_else(|| anyhow::anyhow!("A3S Code result is missing data"))?;
                anyhow::ensure!(
                    data.get("toolPolicy").and_then(Value::as_str) == Some("local-workspace"),
                    "A3S Code did not retain the required local-workspace policy"
                );
                let value = data
                    .get("usage")
                    .ok_or_else(|| anyhow::anyhow!("A3S Code result is missing usage"))?;
                let prompt_tokens = usize_field(value, "prompt_tokens")?;
                let completion_tokens = usize_field(value, "completion_tokens")?;
                let total_tokens = usize_field(value, "total_tokens")?;
                anyhow::ensure!(
                    total_tokens == prompt_tokens.saturating_add(completion_tokens),
                    "A3S Code usage total_tokens is inconsistent"
                );
                usage = Some(ModelExecution {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    cache_read_tokens: optional_usize_field(value, "cache_read_tokens")?,
                    cache_write_tokens: optional_usize_field(value, "cache_write_tokens")?,
                    tool_calls_count,
                });
            }
            _ => {}
        }
    }
    usage.ok_or_else(|| anyhow::anyhow!("A3S Code did not emit a terminal result"))
}

fn usize_field(value: &Value, name: &str) -> Result<usize> {
    optional_usize_field(value, name)?
        .ok_or_else(|| anyhow::anyhow!("A3S Code usage is missing {name}"))
}

fn optional_usize_field(value: &Value, name: &str) -> Result<Option<usize>> {
    let Some(value) = value.get(name) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("A3S Code usage {name} is invalid"))
}

fn read_tail(file: &mut std::fs::File, limit: u64) -> Result<String> {
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(limit)))?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn executable(path: &Path, source: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, source).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn version_requires_the_local_workspace_contract() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("a3s");
        executable(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'a3s 0.12.5'; else echo '  --tool-policy <standard|local-workspace>'; fi\n",
        );
        assert_eq!(
            version_with(|| Command::new(&binary)).unwrap(),
            "a3s 0.12.5"
        );

        executable(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'a3s 0.12.5'; else echo 'code exec'; fi\n",
        );
        let error = version_with(|| Command::new(&binary)).unwrap_err();
        assert!(format!("{error:#}").contains("local-workspace"));

        executable(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'a3s 0.12.4'; else echo '  --tool-policy <standard|local-workspace>'; fi\n",
        );
        let error = version_with(|| Command::new(&binary)).unwrap_err();
        assert!(format!("{error:#}").contains("0.12.5 or newer"));
    }

    #[cfg(unix)]
    #[test]
    fn executes_the_native_product_with_explicit_closed_inputs() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("a3s");
        let args_path = root.path().join("args.txt");
        let prompt_path = root.path().join("prompt.txt");
        let workspace = root.path().join("workspace");
        let config = root.path().join("config.acl");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(&config, "default_model = \"deepseek/chat\"\n").unwrap();
        executable(
            &binary,
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$A3S_TEST_ARGS\"\ncat > \"$A3S_TEST_PROMPT\"\nprintf '%s\\n' '{\"schemaVersion\":1,\"command\":\"code.exec\",\"type\":\"event\",\"sequence\":1,\"event\":{\"type\":\"tool_execution_start\",\"id\":\"1\",\"name\":\"Read\",\"args\":{}}}'\nprintf '%s\\n' '{\"schemaVersion\":1,\"command\":\"code.exec\",\"type\":\"result\",\"sequence\":2,\"ok\":true,\"data\":{\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5,\"cache_read_tokens\":null,\"cache_write_tokens\":null},\"toolPolicy\":\"local-workspace\"}}'\n",
        );
        let mut process = Command::new(&binary);
        process
            .env("A3S_TEST_ARGS", &args_path)
            .env("A3S_TEST_PROMPT", &prompt_path);
        let outcome = execute_with_command(
            A3sCodeRequest {
                workspace: &workspace,
                config_path: &config,
                instructions: "Product instructions",
                task_prompt: "Fix the task",
                model: Some("deepseek/chat"),
                workspace_source_path: None,
                public_internet: false,
                timeout_sec: 5,
            },
            process,
        )
        .unwrap();
        let A3sCodeOutcome::Completed(usage) = outcome else {
            panic!("fake product should complete");
        };
        assert_eq!(usage.total_tokens, 5);
        assert_eq!(usage.tool_calls_count, 1);
        let args = std::fs::read_to_string(args_path).unwrap();
        assert!(args.contains(workspace.to_str().unwrap()));
        assert!(args.contains(config.to_str().unwrap()));
        assert!(args.contains("local-workspace"));
        assert!(args.contains("deepseek/chat"));
        let prompt = std::fs::read_to_string(prompt_path).unwrap();
        assert!(prompt.contains("Product instructions"));
        assert!(prompt.contains("Fix the task"));
        assert!(prompt.contains("editable submission root"));
    }

    #[test]
    fn parses_usage_and_counts_executed_tools() {
        let mut output = tempfile::tempfile().unwrap();
        writeln!(
            output,
            r#"{{"schemaVersion":1,"command":"code.exec","type":"event","sequence":1,"event":{{"type":"tool_execution_start","id":"1","name":"Read","args":{{}}}}}}"#
        )
        .unwrap();
        writeln!(
            output,
            r#"{{"schemaVersion":1,"command":"code.exec","type":"result","sequence":2,"ok":true,"data":{{"usage":{{"prompt_tokens":12,"completion_tokens":5,"total_tokens":17,"cache_read_tokens":3,"cache_write_tokens":null}},"toolPolicy":"local-workspace"}}}}"#
        )
        .unwrap();
        let usage = parse_output(&mut output).unwrap();
        assert_eq!(usage.prompt_tokens, 12);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 17);
        assert_eq!(usage.cache_read_tokens, Some(3));
        assert_eq!(usage.cache_write_tokens, None);
        assert_eq!(usage.tool_calls_count, 1);
    }

    #[test]
    fn rejects_a_weaker_reported_tool_policy() {
        let mut output = tempfile::tempfile().unwrap();
        writeln!(
            output,
            r#"{{"schemaVersion":1,"command":"code.exec","type":"result","sequence":1,"ok":true,"data":{{"usage":{{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}},"toolPolicy":"standard"}}}}"#
        )
        .unwrap();
        let error = parse_output(&mut output).unwrap_err();
        assert!(format!("{error:#}").contains("local-workspace policy"));
    }

    #[test]
    fn maps_an_extracted_source_directory_to_the_workspace_root() {
        let contract =
            workspace_contract(Some("/home/workspace/juliet-static-analyzer/agent-start"));
        assert!(contract.contains("already the `agent-start` directory"));
        assert!(contract.contains("workspace-relative path `path`"));
        assert!(contract.contains("do not create another `agent-start` directory"));
    }

    #[test]
    fn rejects_public_tool_network_before_starting_the_product() {
        let root = tempfile::tempdir().unwrap();
        let error = execute_with_command(
            A3sCodeRequest {
                workspace: root.path(),
                config_path: &root.path().join("config.acl"),
                instructions: "instructions",
                task_prompt: "task",
                model: None,
                workspace_source_path: None,
                public_internet: true,
                timeout_sec: 1,
            },
            Command::new("not-started"),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("does not support Tasks that require public internet")
        );
    }
}
