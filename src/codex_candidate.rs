use crate::model_candidate::ModelExecution;
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub enum CodexOutcome {
    Completed(Option<ModelExecution>),
    TimedOut,
}

pub fn version() -> Result<String> {
    let output = command()
        .arg("--version")
        .output()
        .context("Codex Candidate requires the codex CLI on PATH")?;
    anyhow::ensure!(
        output.status.success(),
        "could not query Codex CLI version: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let version = String::from_utf8(output.stdout)?.trim().to_owned();
    anyhow::ensure!(!version.is_empty(), "Codex CLI returned an empty version");
    Ok(version)
}

pub fn execute(
    workspace: &Path,
    instructions: &str,
    task_prompt: &str,
    model: Option<&str>,
    public_internet: bool,
    timeout_sec: u64,
) -> Result<CodexOutcome> {
    let prompt = format!(
        "{instructions}\n\n# Benchmark task\n\n{task_prompt}\n\nWork only in the supplied workspace and complete the task."
    );
    let mut command = command();
    command.args([
        "exec",
        "--cd",
        workspace
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Codex workspace path is not UTF-8"))?,
        "--sandbox",
        "workspace-write",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--color",
        "never",
        "--json",
        "-c",
        "shell_environment_policy.inherit=none",
    ]);
    if public_internet {
        command.args(["-c", "sandbox_workspace_write.network_access=true"]);
    }
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    command.arg(prompt);

    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()
        .context("could not start Codex Candidate")?;
    let deadline = Instant::now() + Duration::from_secs(timeout_sec);
    loop {
        if let Some(status) = child.try_wait()? {
            stdout.seek(SeekFrom::Start(0))?;
            stderr.seek(SeekFrom::Start(0))?;
            let mut events = String::new();
            let mut diagnostics = String::new();
            stdout.read_to_string(&mut events)?;
            stderr.read_to_string(&mut diagnostics)?;
            anyhow::ensure!(
                status.success(),
                "Codex Candidate exited with {status}: {}",
                diagnostics.trim()
            );
            return Ok(CodexOutcome::Completed(parse_usage(&events)?));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(CodexOutcome::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn command() -> Command {
    if let Some(path) = std::env::var_os("A3S_BENCH_CODEX_BIN") {
        return Command::new(path);
    }
    #[cfg(windows)]
    return Command::new("codex.cmd");
    #[cfg(not(windows))]
    Command::new("codex")
}

fn parse_usage(events: &str) -> Result<Option<ModelExecution>> {
    let mut usage = None;
    let mut tool_calls_count = 0;
    for line in events.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value = serde_json::from_str(line).context("Codex emitted invalid JSONL")?;
        if event.get("type").and_then(Value::as_str) == Some("item.completed")
            && matches!(
                event.pointer("/item/type").and_then(Value::as_str),
                Some("command_execution" | "mcp_tool_call" | "file_change")
            )
        {
            tool_calls_count += 1;
        }
        let Some(value) = event.get("usage") else {
            continue;
        };
        let prompt_tokens = usize_field(value, "input_tokens")?;
        let completion_tokens = usize_field(value, "output_tokens")?;
        usage = Some(ModelExecution {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
            cache_read_tokens: optional_usize_field(value, "cached_input_tokens")?,
            cache_write_tokens: None,
            tool_calls_count,
        });
    }
    Ok(usage)
}

fn usize_field(value: &Value, name: &str) -> Result<usize> {
    optional_usize_field(value, name)?
        .ok_or_else(|| anyhow::anyhow!("Codex usage is missing {name}"))
}

fn optional_usize_field(value: &Value, name: &str) -> Result<Option<usize>> {
    value
        .get(name)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| anyhow::anyhow!("Codex usage {name} is invalid"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_final_usage_and_counts_commands() {
        let events = concat!(
            r#"{"type":"item.completed","item":{"type":"command_execution"}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":3,"output_tokens":5}}"#
        );
        let usage = parse_usage(events).unwrap().unwrap();
        assert_eq!(usage.prompt_tokens, 12);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 17);
        assert_eq!(usage.cache_read_tokens, Some(3));
        assert_eq!(usage.tool_calls_count, 1);
    }
}
