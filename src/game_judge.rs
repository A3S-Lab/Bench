use crate::legacy_judge::{canonical_ratio, normalize_raw};
use crate::runtime::JudgeResult;
use crate::task::{LegacyJudgeSource, TaskInfo};
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static GAME_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct GameSession {
    network: String,
    container: String,
}

impl GameSession {
    pub fn start(source: &LegacyJudgeSource, state_root: &Path) -> Result<Self> {
        anyhow::ensure!(source.mode == "game_server", "Judge is not a game server");
        let source_command = source
            .game_server_command
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("game Judge has no server command"))?;
        let sequence = GAME_SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let suffix = format!("{}-{}-{sequence}", std::process::id(), epoch_millis()?);
        let network = format!("a3s-bench-{suffix}");
        let container = format!("a3s-bench-game-{suffix}");
        docker(&["network", "create", "--internal", &network])?;
        let session = Self { network, container };

        let asset_root = state_root.join("runtime-assets");
        std::fs::create_dir_all(&asset_root)?;
        let script = asset_root.join("game_server_app.py");
        crate::state_fs::secure_atomic_write(
            &script,
            include_bytes!("../runtime_assets/game_server_app.py"),
        )?;
        let command =
            source_command.replace("/tmp/game_server_app.py", "/opt/a3s/game_server_app.py");
        let mut process = Command::new("docker");
        process.args(["run", "-d", "--rm"]);
        if let Some(platform) = source.platform.as_deref() {
            process.args(["--platform", platform]);
        }
        let output = process
            .args([
                "--name",
                &session.container,
                "--network",
                &session.network,
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
                "/tmp:rw,exec,nosuid,size=256m",
                "--workdir",
                "/tmp",
                "--mount",
            ])
            .arg(format!(
                "type=bind,src={},dst=/opt/a3s/game_server_app.py,readonly",
                script.display()
            ))
            .arg(&source.image)
            .args(["/bin/bash", "-lc", &command])
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "could not start game Judge: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        session.wait_ready()?;
        Ok(session)
    }

    pub fn network(&self) -> &str {
        &self.network
    }

    pub fn url(&self) -> String {
        format!("http://{}:8000", self.container)
    }

    pub fn finish(&self, task: &TaskInfo, source: &LegacyJudgeSource) -> Result<JudgeResult> {
        let output = docker(&[
            "exec",
            &self.container,
            "python",
            "-c",
            "import json,urllib.request;print(urllib.request.urlopen('http://127.0.0.1:8000/status').read().decode())",
        ])?;
        let value: Value = serde_json::from_str(&output)?;
        let raw = value
            .get("peak_score")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let ratio = normalize_raw(source.rescale.as_ref(), raw)?;
        let primary = task
            .metrics
            .iter()
            .find(|metric| metric.role == "primary")
            .expect("Task parser guarantees a primary metric");
        let mut metrics = Map::new();
        metrics.insert(primary.name.clone(), Value::String(canonical_ratio(ratio)));
        Ok(JudgeResult {
            schema: "bench.judge.result.v1".into(),
            solution_verdict: "valid".into(),
            metrics,
            diagnostics: serde_json::json!({
                "adapter":"edgebench-game-v1",
                "peak_score":raw,
                "moves":value.get("moves")
            }),
        })
    }

    #[cfg(test)]
    fn start_game(&self) -> Result<String> {
        docker(&[
            "exec",
            &self.container,
            "python",
            "-c",
            "import urllib.request,urllib.error;r=urllib.request.Request('http://127.0.0.1:8000/new',data=b'{}',headers={'Content-Type':'application/json'});\ntry: print(urllib.request.urlopen(r).read().decode())\nexcept urllib.error.HTTPError as e: print(e.read().decode()); raise",
        ])
    }

    fn wait_ready(&self) -> Result<()> {
        for _ in 0..60 {
            let ready = Command::new("docker")
                .args([
                    "exec",
                    &self.container,
                    "python",
                    "-c",
                    "import urllib.request;urllib.request.urlopen('http://127.0.0.1:8000/health')",
                ])
                .output()?;
            if ready.status.success() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let logs = docker(&["logs", &self.container]).unwrap_or_default();
        anyhow::bail!("game Judge did not become ready: {logs}")
    }
}

impl Drop for GameSession {
    fn drop(&mut self) {
        let _ = docker(&["rm", "-f", &self.container]);
        let _ = docker(&["network", "rm", &self.network]);
    }
}

fn docker(args: &[&str]) -> Result<String> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .context("could not run Docker for game Judge")?;
    anyhow::ensure!(
        output.status.success(),
        "Docker game Judge command failed: {}{}{}",
        String::from_utf8_lossy(&output.stderr).trim(),
        if output.stdout.is_empty() { "" } else { "\n" },
        String::from_utf8_lossy(&output.stdout).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn epoch_millis() -> Result<u128> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Docker and the linux/amd64 imported Judge image"]
    fn imported_game_server_starts_and_reports_zero_score() {
        let task = crate::task::load_local(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks/anchorhead_text_adventure"),
        )
        .unwrap();
        let source = task.legacy_judge.as_ref().unwrap();
        let state = tempfile::tempdir().unwrap();
        let session = GameSession::start(source, state.path()).unwrap();
        let new_game = session.start_game().unwrap_or_else(|error| {
            let output = Command::new("docker")
                .args(["logs", &session.container])
                .output()
                .unwrap();
            let logs = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            panic!("{error:#}\n{logs}")
        });
        assert!(new_game.contains("observation"));
        let result = session.finish(&task, source).unwrap();
        assert_eq!(
            result.metrics.get("score").and_then(Value::as_str),
            Some("0")
        );
    }
}
