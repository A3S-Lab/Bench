use crate::runtime::JudgeResult;
use crate::task::{LegacyJudgeSource, TaskInfo};
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{Map, Value};
use std::path::Path;
use std::process::Command;

pub fn execute(
    task: &TaskInfo,
    source: &LegacyJudgeSource,
    submission: &Path,
    model: Option<&crate::config::ModelRoute>,
) -> Result<JudgeResult> {
    anyhow::ensure!(
        source.mode == "batch",
        "interactive Judge mode is not implemented yet"
    );
    let mut command = Command::new("docker");
    command.args([
        "run",
        "--rm",
        "--user",
        "0:0",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "1024",
        "--memory",
        "8g",
        "--cpus",
        "4",
        "--tmpfs",
        "/tmp:rw,nosuid,size=2g",
    ]);
    configure_model_gateway(&mut command, source.requires_model_gateway, model)?;
    if let Some(platform) = source.platform.as_deref() {
        command.args(["--platform", platform]);
    }
    let destination = shell_quote(&source.workspace_source_path);
    let judge_command = format!(
        "cp -R /a3s/submission/. {destination}/ && chmod -R u+rwX {destination} && {}",
        source.command
    );
    let output = command
        .arg("--mount")
        .arg(format!(
            "type=bind,src={},dst=/a3s/submission,readonly",
            submission.display()
        ))
        .arg(&source.image)
        .args(["/bin/bash", "-lc", &judge_command])
        .output()
        .context("could not start legacy OCI Judge")?;
    let mut raw = String::from_utf8_lossy(&output.stdout).into_owned();
    raw.push_str(&String::from_utf8_lossy(&output.stderr));
    anyhow::ensure!(raw.len() <= 16 * 1024 * 1024, "Judge output exceeds 16 MiB");
    let ratio = parse_score(source, &raw)?;
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
            "adapter":"edgebench-v1",
            "exit_code":output.status.code(),
            "parser":source.parser
        }),
    })
}

fn configure_model_gateway(
    command: &mut Command,
    required: bool,
    model: Option<&crate::config::ModelRoute>,
) -> Result<()> {
    if required {
        let model = model.ok_or_else(|| anyhow::anyhow!("Judge requires a model gateway route"))?;
        let base_url = container_base_url(&model.base_url);
        command
            .args(["--network", "bridge"])
            .args(["--add-host", "host.docker.internal:host-gateway"])
            .args(["--env", "SFORGE_JUDGE_API_KEY"])
            .args(["--env", "SFORGE_JUDGE_API_BASE_URL"])
            .args(["--env", "SFORGE_JUDGE_MODEL"])
            .env("SFORGE_JUDGE_API_KEY", &model.api_key)
            .env("SFORGE_JUDGE_API_BASE_URL", base_url)
            .env("SFORGE_JUDGE_MODEL", &model.model);
    } else {
        command.args(["--network", "none"]);
    }
    Ok(())
}

fn container_base_url(value: &str) -> String {
    for local in ["localhost", "127.0.0.1", "[::1]"] {
        for scheme in ["http", "https"] {
            let prefix = format!("{scheme}://{local}");
            if let Some(suffix) = value.strip_prefix(&prefix) {
                if suffix.is_empty() || suffix.starts_with(':') || suffix.starts_with('/') {
                    return format!("{scheme}://host.docker.internal{suffix}");
                }
            }
        }
    }
    value.to_owned()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn parse_score(source: &LegacyJudgeSource, output: &str) -> Result<f64> {
    match source.parser.as_str() {
        "structured_json" => {
            let value = extract_structured(output)?;
            anyhow::ensure!(
                value.get("valid").and_then(Value::as_bool).unwrap_or(true),
                "Judge marked result invalid"
            );
            if let Some(score) = value.get("score").and_then(Value::as_f64) {
                normalize_raw(source.rescale.as_ref(), score)
            } else {
                Ok(value
                    .get("pass_rate")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0))
            }
        }
        "score_sum" => {
            let expression =
                Regex::new(r"TOTAL_SCORE\s+(?:inf|([0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?))")?;
            let raw = expression
                .captures(output)
                .and_then(|captures| captures.get(1))
                .and_then(|value| value.as_str().parse::<f64>().ok())
                .unwrap_or(0.0);
            normalize_raw(source.rescale.as_ref(), raw)
        }
        "pytest_v" => pytest_ratio(output),
        value => anyhow::bail!("unsupported legacy Judge parser {value:?}"),
    }
}

fn extract_structured(output: &str) -> Result<Value> {
    const START: &str = ">>>>> Start Structured Result";
    const END: &str = ">>>>> End Structured Result";
    if let (Some(start), Some(end)) = (output.find(START), output.find(END)) {
        let body = output[start + START.len()..end].trim();
        return serde_json::from_str(body).context("invalid structured Judge JSON");
    }
    for (index, byte) in output.bytes().enumerate() {
        if byte == b'{' {
            if let Some(end) = json_object_end(&output[index..]) {
                if let Ok(value) = serde_json::from_str::<Value>(&output[index..index + end]) {
                    if value.get("score").is_some() || value.get("pass_rate").is_some() {
                        return Ok(value);
                    }
                }
            }
        }
    }
    let diagnostic: String = output.chars().take(4096).collect();
    anyhow::bail!("Judge produced no structured result: {diagnostic}")
}

fn json_object_end(value: &str) -> Option<usize> {
    let mut depth = 0_u32;
    let mut string = false;
    let mut escape = false;
    for (index, character) in value.char_indices() {
        if escape {
            escape = false;
        } else if string && character == '\\' {
            escape = true;
        } else if character == '"' {
            string = !string;
        } else if !string && character == '{' {
            depth += 1;
        } else if !string && character == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(index + 1);
            }
        }
    }
    None
}

fn pytest_ratio(output: &str) -> Result<f64> {
    let summary = Regex::new(r"(?m)=+\s+([^\n]+?)\s+in\s+[0-9.]+s?\s+=*")?;
    let counts = Regex::new(r"([0-9]+)\s+(passed|xfailed|xpassed|failed|errors?|skipped)")?;
    let Some(summary) = summary.captures_iter(output).last() else {
        return Ok(0.0);
    };
    let mut passed = 0_u64;
    let mut failed = 0_u64;
    for item in counts.captures_iter(&summary[1]) {
        let count = item[1].parse::<u64>()?;
        match &item[2] {
            "passed" | "xfailed" | "xpassed" => passed += count,
            "failed" | "error" | "errors" => failed += count,
            _ => {}
        }
    }
    let total = passed + failed;
    Ok(if total == 0 {
        0.0
    } else {
        passed as f64 / total as f64
    })
}

pub(crate) fn normalize_raw(spec: Option<&Value>, raw: f64) -> Result<f64> {
    let Some(spec) = spec else {
        return Ok(raw.clamp(0.0, 1.0));
    };
    let get = |name: &str| -> Result<f64> {
        spec.get(name)
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow::anyhow!("rescale is missing {name}"))
    };
    let percent = match spec.get("kind").and_then(Value::as_str).unwrap_or("") {
        "linear" => 100.0 * (raw - get("lower")?) / (get("upper")? - get("lower")?),
        "log_anchor" => get("anchor_score")? * raw.ln() / get("anchor_raw")?.ln(),
        "log_max" => {
            100.0 * (raw / get("baseline")?).ln() / (get("expert")? / get("baseline")?).ln()
        }
        "log_min" => {
            100.0 * (get("baseline")? / raw).ln() / (get("baseline")? / get("expert")?).ln()
        }
        "log1p_max" => {
            100.0 * (raw / get("baseline")?).ln_1p() / (get("upper")? / get("baseline")?).ln_1p()
        }
        "piecewise_max" => piecewise(raw, spec, false, false)?,
        "piecewise_min" => piecewise(raw, spec, true, false)?,
        "piecewise_log_min" => piecewise(raw, spec, true, true)?,
        kind => anyhow::bail!("unsupported rescale kind {kind:?}"),
    };
    anyhow::ensure!(
        percent.is_finite(),
        "Judge rescale produced a non-finite value"
    );
    Ok((percent.clamp(0.0, 100.0)) / 100.0)
}

fn piecewise(raw: f64, spec: &Value, minimize: bool, logarithmic: bool) -> Result<f64> {
    let value = |name: &str| -> Result<f64> {
        spec.get(name)
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow::anyhow!("rescale is missing {name}"))
    };
    let points = [
        value("baseline")?,
        value("rank30")?,
        value("rank1")?,
        value("super_anchor")?,
    ];
    let scores = [0.0, 20.0, 80.0, 100.0];
    let transformed = |item: f64| if logarithmic { item.ln() } else { item };
    if (minimize && raw >= points[0]) || (!minimize && raw <= points[0]) {
        return Ok(0.0);
    }
    if (minimize && raw <= points[3]) || (!minimize && raw >= points[3]) {
        return Ok(100.0);
    }
    for index in 0..3 {
        let inside = if minimize {
            raw <= points[index] && raw >= points[index + 1]
        } else {
            raw >= points[index] && raw <= points[index + 1]
        };
        if inside {
            let fraction = (transformed(raw) - transformed(points[index]))
                / (transformed(points[index + 1]) - transformed(points[index]));
            return Ok(scores[index] + fraction * (scores[index + 1] - scores[index]));
        }
    }
    Ok(0.0)
}

pub(crate) fn canonical_ratio(value: f64) -> String {
    let value = value.clamp(0.0, 1.0);
    let formatted = format!("{value:.10}");
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".into()
    } else {
        trimmed.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_upstream_output_forms() {
        assert_eq!(
            pytest_ratio("=== 2 passed, 1 failed in 1.0s ===").unwrap(),
            2.0 / 3.0
        );
        let structured = extract_structured(
            ">>>>> Start Structured Result\n{\"valid\":true,\"score\":0.75}\n>>>>> End Structured Result",
        )
        .unwrap();
        assert_eq!(structured["score"], 0.75);
    }

    #[test]
    fn model_gateway_uses_ephemeral_environment_not_cli_secrets() {
        let route = crate::config::ModelRoute {
            model: "grader".into(),
            api_key: "top-secret".into(),
            base_url: "https://example.test/v1".into(),
        };
        let mut command = Command::new("docker");
        configure_model_gateway(&mut command, true, Some(&route)).unwrap();
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(arguments.contains("SFORGE_JUDGE_API_KEY"));
        assert!(!arguments.contains("top-secret"));
        let environment = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            environment["SFORGE_JUDGE_API_KEY"].as_deref(),
            Some("top-secret")
        );
        assert_eq!(environment["SFORGE_JUDGE_MODEL"].as_deref(), Some("grader"));
        assert_eq!(
            container_base_url("http://127.0.0.1:8080/v1"),
            "http://host.docker.internal:8080/v1"
        );
        assert_eq!(
            container_base_url("https://api.example.test/v1"),
            "https://api.example.test/v1"
        );
    }

    #[test]
    fn all_imported_batch_protocols_have_adapters() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks");
        let mut batch = 0;
        let mut interactive = 0;
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry
                .unwrap()
                .path()
                .join("private/judge/judge.source.json");
            if !path.is_file() {
                continue;
            }
            let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let mode = value
                .pointer("/evaluation/mode")
                .and_then(Value::as_str)
                .unwrap();
            if mode == "game_server" {
                interactive += 1;
                continue;
            }
            batch += 1;
            let parser = value
                .pointer("/source_result/parser")
                .and_then(Value::as_str)
                .unwrap();
            assert!(matches!(
                parser,
                "structured_json" | "score_sum" | "pytest_v"
            ));
            if let Some(kind) = value
                .pointer("/source_result/rescale_hint/kind")
                .and_then(Value::as_str)
            {
                assert!(matches!(
                    kind,
                    "linear"
                        | "log_anchor"
                        | "log_max"
                        | "log_min"
                        | "log1p_max"
                        | "piecewise_max"
                        | "piecewise_min"
                        | "piecewise_log_min"
                ));
            }
        }
        assert_eq!(batch, 48);
        assert_eq!(interactive, 3);
    }
}
