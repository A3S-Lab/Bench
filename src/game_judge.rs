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
const PARALLEL_GAME_SESSION_LIMIT: usize = 3;

#[derive(Debug)]
struct GameHistoryScore {
    raw: f64,
    best_moves: Value,
    best_peak_score: Value,
}

fn required_finite_number(value: &Value, field: &str) -> Result<f64> {
    let number = value
        .get(field)
        .and_then(Value::as_f64)
        .with_context(|| format!("game history field `{field}` is missing or not numeric"))?;
    anyhow::ensure!(
        number.is_finite(),
        "game history field `{field}` is not finite"
    );
    Ok(number)
}

fn parse_game_history(value: &Value) -> Result<GameHistoryScore> {
    let reported_best = required_finite_number(value, "best_score")?;
    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .context("game history `entries` is missing or not an array")?;
    if entries.is_empty() {
        anyhow::ensure!(
            reported_best == 0.0,
            "empty game history reported a non-zero best score"
        );
        return Ok(GameHistoryScore {
            raw: 0.0,
            best_moves: Value::from(0),
            best_peak_score: Value::from(0),
        });
    }

    let mut best_score = None;
    let mut best_entry = None;
    for entry in entries {
        let score = required_finite_number(entry, "score")?;
        let final_score = required_finite_number(entry, "final_score")?;
        anyhow::ensure!(
            score == final_score,
            "game history score does not match final_score"
        );
        let replace = match best_score {
            Some(current_score) => score > current_score,
            None => true,
        };
        if replace {
            best_score = Some(score);
            best_entry = Some(entry);
        }
    }

    let raw = best_score.expect("non-empty history has a best score");
    anyhow::ensure!(
        reported_best == raw,
        "game history best_score does not match archived entries"
    );
    let best_entry = best_entry.expect("non-empty history has a best entry");
    Ok(GameHistoryScore {
        raw,
        best_moves: best_entry
            .get("moves")
            .cloned()
            .unwrap_or_else(|| Value::from(0)),
        best_peak_score: best_entry
            .get("peak_score")
            .cloned()
            .unwrap_or_else(|| Value::from(0)),
    })
}

pub struct GameSession {
    network: String,
    container: String,
}

impl GameSession {
    #[cfg(test)]
    pub fn start(
        source: &LegacyJudgeSource,
        resources: crate::task::RoleResources,
        state_root: &Path,
    ) -> Result<Self> {
        Self::start_with_parallel_sessions(source, resources, state_root, true)
    }

    pub fn start_with_parallel_sessions(
        source: &LegacyJudgeSource,
        resources: crate::task::RoleResources,
        state_root: &Path,
        parallel_game_sessions: bool,
    ) -> Result<Self> {
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
        make_runtime_asset_readable(&script)?;
        let command = game_server_command(source_command, parallel_game_sessions)
            .replace("/tmp/game_server_app.py", "/opt/a3s/game_server_app.py");
        let mut process = Command::new("docker");
        process.args(["run", "-d"]);
        if let Some(platform) = source.platform.as_deref() {
            process.args(["--platform", platform]);
        }
        let output = process
            .args(crate::runtime_profile::judge_docker_args(resources))
            .args(crate::runtime_profile::host_dns_args())
            .args(crate::runtime_profile::READ_ONLY_JUDGE_TMPFS)
            .args([
                "--name",
                &session.container,
                "--network",
                &session.network,
                "--workdir",
                "/tmp",
                "--mount",
            ])
            .arg(format!(
                "type=bind,src={},dst=/opt/a3s/game_server_app.py,readonly",
                script.display()
            ))
            .arg(&source.image)
            .args(["/bin/bash", "-c", &command])
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
            "import urllib.request;r=urllib.request.Request('http://127.0.0.1:8000/close-all',data=b'',method='POST');urllib.request.urlopen(r).read();print(urllib.request.urlopen('http://127.0.0.1:8000/history').read().decode())",
        ])?;
        let value: Value =
            serde_json::from_str(&output).context("could not parse game Judge history")?;
        let history = parse_game_history(&value)?;
        let raw = history.raw;
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
                "adapter": "edgebench-game-v1",
                "moves": history.best_moves,
                "peak_score": history.best_peak_score,
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

    #[cfg(test)]
    fn game_status(&self, session_id: &str) -> Result<String> {
        let script = format!(
            "import urllib.request;print(urllib.request.urlopen('http://127.0.0.1:8000/{session_id}/status').read().decode())"
        );
        docker(&["exec", &self.container, "python", "-c", &script])
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
        let logs = docker_output(&["logs", &self.container])?;
        let inspect = docker_output(&[
            "inspect",
            "--format",
            "status={{.State.Status}} exit={{.State.ExitCode}} error={{.State.Error}}",
            &self.container,
        ])?;
        anyhow::bail!("game Judge did not become ready: {inspect}\n{logs}")
    }
}

impl Drop for GameSession {
    fn drop(&mut self) {
        let _ = docker(&["rm", "-f", &self.container]);
        let _ = docker(&["network", "rm", &self.network]);
    }
}

fn game_server_command(source_command: &str, parallel_game_sessions: bool) -> String {
    let max_active_sessions = if parallel_game_sessions {
        PARALLEL_GAME_SESSION_LIMIT
    } else {
        1
    };
    format!("{source_command} --max-active-sessions {max_active_sessions}")
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

fn docker_output(args: &[&str]) -> Result<String> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .context("could not run Docker for game Judge diagnostics")?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .trim()
    .to_owned())
}

fn make_runtime_asset_readable(path: &Path) -> Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o444);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
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
    fn game_server_command_configures_parallel_session_capacity() {
        let source = "python /tmp/game_server_app.py --rom game.z5";
        assert_eq!(
            game_server_command(source, true),
            format!("{source} --max-active-sessions 3")
        );
        assert_eq!(
            game_server_command(source, false),
            format!("{source} --max-active-sessions 1")
        );
    }

    #[test]
    fn history_scoring_uses_best_final_score_instead_of_peak() {
        let history = serde_json::json!({
            "best_score": 20,
            "entries": [
                {
                    "session_id": "first",
                    "score": 10,
                    "final_score": 10,
                    "peak_score": 50,
                    "pass_rate": 0.9
                },
                {
                    "session_id": "second",
                    "score": 20,
                    "final_score": 20,
                    "peak_score": 20,
                    "pass_rate": 0.2
                }
            ]
        });

        let parsed = parse_game_history(&history).unwrap();
        assert_eq!(parsed.raw, 20.0);
        assert_eq!(parsed.best_moves, Value::from(0));
        assert_eq!(parsed.best_peak_score, Value::from(20));
    }

    #[test]
    fn history_scoring_preserves_best_negative_score() {
        let history = serde_json::json!({
            "best_score": -2,
            "entries": [
                {"score": -5, "final_score": -5, "peak_score": 4, "pass_rate": -0.5},
                {"score": -2, "final_score": -2, "peak_score": 1, "pass_rate": -0.2}
            ]
        });
        assert_eq!(parse_game_history(&history).unwrap().raw, -2.0);
    }

    #[test]
    fn empty_history_scores_zero() {
        let history = serde_json::json!({"best_score": 0, "entries": []});
        let parsed = parse_game_history(&history).unwrap();
        assert_eq!(parsed.raw, 0.0);
        assert_eq!(parsed.best_moves, Value::from(0));
        assert_eq!(parsed.best_peak_score, Value::from(0));
    }

    #[test]
    fn malformed_or_inconsistent_history_is_rejected() {
        assert!(parse_game_history(&serde_json::json!({
            "best_score": "20",
            "entries": []
        }))
        .is_err());
        assert!(parse_game_history(&serde_json::json!({
            "best_score": 20,
            "entries": [{"score": 20, "final_score": 19, "pass_rate": 0.2}]
        }))
        .is_err());
        assert!(parse_game_history(&serde_json::json!({
            "best_score": 99,
            "entries": [{"score": 20, "final_score": 20, "pass_rate": 0.2}]
        }))
        .is_err());
    }

    #[test]
    #[ignore = "requires Docker and the linux/amd64 imported Judge image"]
    fn imported_game_server_serial_mode_archives_previous_session() {
        let task = crate::task::load_local(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks/anchorhead_text_adventure"),
        )
        .unwrap();
        let source = task.legacy_judge.as_ref().unwrap();
        let state = tempfile::tempdir().unwrap();
        let session = GameSession::start_with_parallel_sessions(
            source,
            task.resources.judge,
            state.path(),
            false,
        )
        .unwrap();
        let first_game = session.start_game().unwrap_or_else(|error| {
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
        let first_game: Value = serde_json::from_str(&first_game).unwrap();
        let first_id = first_game
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap();
        let second_game: Value = serde_json::from_str(&session.start_game().unwrap()).unwrap();
        let second_id = second_game
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap();
        assert!(first_game.get("observation").is_some());
        assert!(session.game_status(first_id).is_err());
        assert!(session.game_status(second_id).is_ok());
        let result = session.finish(&task, source).unwrap();
        assert_eq!(
            result.metrics.get("score").and_then(Value::as_str),
            Some("0")
        );
        assert_eq!(
            result.diagnostics.get("moves").and_then(Value::as_i64),
            Some(0)
        );
        assert_eq!(
            result.diagnostics.get("peak_score").and_then(Value::as_i64),
            Some(0)
        );
    }
}
