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
    configure_judge_container(&mut command);
    command.args(crate::runtime_profile::JUDGE_DOCKER_LIMITS);
    configure_model_gateway(&mut command, source.requires_model_gateway, model)?;
    if let Some(platform) = source.platform.as_deref() {
        command.args(["--platform", platform]);
    }
    // Judge command runs under `timeout` inside the container; no
    // environment variable passthrough is needed.
    let timeout_runner = format!(
        "timeout --kill-after=10 {} /bin/bash -lc {}",
        source.timeout_sec,
        shell_quote(&source.command),
    );
    let judge_command = legacy_judge_command(
        "/a3s/submission",
        &source.workspace_source_path,
        &timeout_runner,
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

    let exit_code = output.status.code();

    // Signal kills (OOM, SIGTERM) are infrastructure failures.  A timeout
    // (exit_code 124) is NOT: the judge ran for the full descriptor timeout
    // without crashing, meaning the candidate's code was too slow.  In that
    // case we fall through to the normal scoring path.
    if abnormal_judge_exit(exit_code) {
        let snippet: String = raw.chars().take(4096).collect();
        anyhow::bail!(
            "Judge process terminated abnormally (exit_code: {:?}): {}",
            exit_code,
            snippet
        );
    }

    if exit_code == Some(124) {
        eprintln!("Judge exceeded descriptor timeout; scoring 0.0");
    }

    // An ordinary exit without a structured result means the candidate's
    // submission could not be scored (for example, it failed to compile).
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
            "adapter": "edgebench-v1",
            "exit_code": exit_code,
            "parser": source.parser
        }),
    })
}

fn configure_judge_container(command: &mut Command) {
    command.args([
        "run",
        "--rm",
        "--user",
        "0:0",
        "--cap-drop",
        "ALL",
        "--cap-add",
        "DAC_OVERRIDE",
        "--security-opt",
        "no-new-privileges",
    ]);
}

fn abnormal_judge_exit(exit_code: Option<i32>) -> bool {
    matches!(exit_code, None | Some(137 | 143))
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

fn legacy_judge_command(source: &str, destination: &str, timeout_runner: &str) -> String {
    let source = format!("{}/.", source.trim_end_matches('/'));
    let destination = shell_quote(destination);
    // The judge container runs as root with DAC_OVERRIDE, so no permission
    // fixup is needed for the destination tree.
    // A previous `chmod -R u+rwX {destination}` was removed because it
    // recursed over the entire judge workspace (which can contain 130K+
    // files from the judge image), stalling for over an hour before the
    // judge script could start.
    format!(
        "cp -R {} {destination}/ && {}",
        shell_quote(&source),
        timeout_runner,
    )
}

fn parse_score(source: &LegacyJudgeSource, output: &str) -> Result<f64> {
    match source.parser.as_str() {
        "structured_json" => {
            // If the judge ran but produced no structured result (e.g. the
            // candidate's code was too broken for the judge script to
            // complete), score 0.0 instead of failing the entire run.
            let Some(value) = extract_structured(output)? else {
                eprintln!("Judge produced no structured result; scoring 0.0");
                return Ok(0.0);
            };
            if !value.get("valid").and_then(Value::as_bool).unwrap_or(true) {
                eprintln!("Judge marked result invalid; scoring 0.0");
                return Ok(0.0);
            }
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

fn extract_structured(output: &str) -> Result<Option<Value>> {
    const START: &str = ">>>>> Start Structured Result";
    const END: &str = ">>>>> End Structured Result";
    if let (Some(start), Some(end)) = (output.find(START), output.find(END)) {
        let body = output[start + START.len()..end].trim();
        return serde_json::from_str(body)
            .map(Some)
            .context("invalid structured Judge JSON");
    }
    for (index, byte) in output.bytes().enumerate() {
        if byte == b'{' {
            if let Some(end) = json_object_end(&output[index..]) {
                if let Ok(value) = serde_json::from_str::<Value>(&output[index..index + end]) {
                    if value.get("score").is_some() || value.get("pass_rate").is_some() {
                        return Ok(Some(value));
                    }
                }
            }
        }
    }
    Ok(None)
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
    if !raw.is_finite() {
        return Ok(0.0);
    }
    let Some(spec) = spec else {
        return Ok(raw.clamp(0.0, 100.0) / 100.0);
    };
    let get = |name: &str| -> Result<f64> {
        let value = spec
            .get(name)
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow::anyhow!("rescale is missing {name}"))?;
        anyhow::ensure!(value.is_finite(), "rescale {name} is not finite");
        Ok(value)
    };
    let percent = match spec.get("kind").and_then(Value::as_str).unwrap_or("") {
        "linear" => scale(100.0 * (raw - get("lower")?), get("upper")? - get("lower")?),
        "min_linear" => scale(
            100.0 * (get("baseline")? - raw),
            get("baseline")? - get("expert")?,
        ),
        "min_linear_positive" => {
            if raw <= 0.0 {
                0.0
            } else {
                scale(
                    100.0 * (get("baseline")? - raw),
                    get("baseline")? - get("expert")?,
                )
            }
        }
        "min_inverse_anchor" => {
            let anchor_raw = get("anchor_raw")?;
            if raw <= 0.0 || anchor_raw <= 0.0 {
                0.0
            } else {
                get("anchor_score")? * anchor_raw / raw
            }
        }
        "compression_ratio_cropped_guarded" => {
            if raw < 0.05 {
                0.0
            } else {
                scale(
                    100.0 * (get("baseline")? - raw),
                    get("baseline")? - get("expert")?,
                )
            }
        }
        "log_anchor" => {
            let anchor_raw = get("anchor_raw")?;
            if raw <= 1.0 || anchor_raw <= 1.0 {
                0.0
            } else {
                scale(get("anchor_score")? * raw.ln(), anchor_raw.ln())
            }
        }
        "log_max" => {
            let baseline = get("baseline")?;
            let expert = get("expert")?;
            if raw <= 0.0 || baseline <= 0.0 || expert <= 0.0 || baseline == expert {
                0.0
            } else {
                scale(100.0 * (raw / baseline).ln(), (expert / baseline).ln())
            }
        }
        "log_min" => {
            let baseline = get("baseline")?;
            let expert = get("expert")?;
            if raw <= 0.0 || baseline <= 0.0 || expert <= 0.0 || baseline == expert {
                0.0
            } else {
                scale(100.0 * (baseline / raw).ln(), (baseline / expert).ln())
            }
        }
        "log1p_max" => {
            let baseline = get("baseline")?;
            let upper = get("upper")?;
            if raw <= 0.0 || baseline <= 0.0 || upper <= 0.0 {
                0.0
            } else {
                scale(100.0 * (raw / baseline).ln_1p(), (upper / baseline).ln_1p())
            }
        }
        "piecewise_max" => piecewise(raw, spec, false, false)?,
        "piecewise_min" => piecewise(raw, spec, true, false)?,
        "piecewise_log_min" => piecewise(raw, spec, true, true)?,
        kind => anyhow::bail!("unsupported rescale kind {kind:?}"),
    };
    Ok(if percent.is_finite() {
        percent.clamp(0.0, 100.0) / 100.0
    } else {
        0.0
    })
}

fn scale(numerator: f64, denominator: f64) -> f64 {
    if denominator == 0.0 || !numerator.is_finite() || !denominator.is_finite() {
        0.0
    } else {
        numerator / denominator
    }
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
    let ordered = if minimize {
        points.windows(2).all(|pair| pair[0] > pair[1])
    } else {
        points.windows(2).all(|pair| pair[0] < pair[1])
    };
    if !ordered || (logarithmic && points.iter().any(|point| *point <= 0.0)) {
        return Ok(0.0);
    }
    if minimize && raw <= 0.0 {
        return Ok(0.0);
    }
    let transformed = |item: f64| if logarithmic { item.ln() } else { item };
    if logarithmic && raw <= 0.0 {
        return Ok(0.0);
    }
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
            let fraction = scale(
                transformed(raw) - transformed(points[index]),
                transformed(points[index + 1]) - transformed(points[index]),
            );
            return Ok(
                (scores[index] + fraction * (scores[index + 1] - scores[index]))
                    .clamp(scores[index], scores[index + 1]),
            );
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

    fn rescale(value: Value, raw: f64) -> f64 {
        normalize_raw(Some(&value), raw).unwrap()
    }

    #[test]
    fn parses_upstream_output_forms() {
        assert_eq!(
            pytest_ratio("=== 2 passed, 1 failed in 1.0s ===").unwrap(),
            2.0 / 3.0
        );
        let structured = extract_structured(
            ">>>>> Start Structured Result\n{\"valid\":true,\"score\":0.75}\n>>>>> End Structured Result",
        )
        .unwrap()
        .unwrap();
        assert_eq!(structured["score"], 0.75);
    }

    fn structured_source() -> LegacyJudgeSource {
        LegacyJudgeSource {
            image: "judge:latest".into(),
            command: "judge".into(),
            mode: "batch".into(),
            parser: "structured_json".into(),
            workspace_source_path: "/workspace".into(),
            rescale: None,
            platform: None,
            game_server_command: None,
            requires_model_gateway: false,
            timeout_sec: 60,
        }
    }

    #[test]
    fn candidate_quality_failures_score_zero() {
        let source = structured_source();
        assert_eq!(parse_score(&source, "compiler error").unwrap(), 0.0);
        assert_eq!(
            parse_score(
                &source,
                ">>>>> Start Structured Result\n{\"valid\":false}\n>>>>> End Structured Result",
            )
            .unwrap(),
            0.0
        );
    }

    #[test]
    fn malformed_marked_result_remains_a_protocol_error() {
        let error = parse_score(
            &structured_source(),
            ">>>>> Start Structured Result\n{not json}\n>>>>> End Structured Result",
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid structured Judge JSON"));
    }

    #[test]
    fn judge_container_uses_only_the_required_override_capability() {
        let mut command = Command::new("docker");
        configure_judge_container(&mut command);
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--cap-drop", "ALL"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--cap-add", "DAC_OVERRIDE"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--security-opt", "no-new-privileges"]));
    }

    #[test]
    fn judge_exit_classification_separates_candidate_and_infrastructure_failures() {
        for exit_code in [None, Some(137), Some(143)] {
            assert!(abnormal_judge_exit(exit_code));
        }
        // Timeout (124) is not abnormal — it means the candidate was too slow.
        assert!(!abnormal_judge_exit(Some(124)));
        for exit_code in [Some(0), Some(1), Some(2)] {
            assert!(!abnormal_judge_exit(exit_code));
        }
    }

    #[test]
    fn normalization_supports_all_upstream_rescale_kinds() {
        assert_eq!(
            rescale(
                serde_json::json!({"kind":"min_linear","baseline":10.0,"expert":0.0}),
                5.0,
            ),
            0.5
        );
        assert_eq!(
            rescale(
                serde_json::json!({"kind":"min_linear_positive","baseline":10.0,"expert":0.0}),
                5.0,
            ),
            0.5
        );
        assert_eq!(
            rescale(
                serde_json::json!({"kind":"min_inverse_anchor","anchor_raw":10.0,"anchor_score":50.0}),
                20.0,
            ),
            0.25
        );
        assert_eq!(
            rescale(
                serde_json::json!({
                    "kind":"compression_ratio_cropped_guarded",
                    "baseline":10.0,
                    "expert":0.0
                }),
                5.0,
            ),
            0.5
        );
    }

    #[test]
    fn normalization_returns_finite_zero_for_invalid_domains_and_degenerate_specs() {
        let cases = [
            (
                serde_json::json!({"kind":"linear","lower":1.0,"upper":1.0}),
                1.0,
            ),
            (
                serde_json::json!({"kind":"min_linear","baseline":1.0,"expert":1.0}),
                1.0,
            ),
            (
                serde_json::json!({"kind":"log_anchor","anchor_raw":1.0,"anchor_score":50.0}),
                0.0,
            ),
            (
                serde_json::json!({"kind":"log_max","baseline":0.0,"expert":10.0}),
                0.0,
            ),
            (
                serde_json::json!({"kind":"log_min","baseline":10.0,"expert":0.0}),
                0.0,
            ),
            (
                serde_json::json!({"kind":"log1p_max","baseline":1.0,"upper":0.0}),
                1.0,
            ),
            (
                serde_json::json!({
                    "kind":"piecewise_max",
                    "baseline":1.0,
                    "rank30":1.0,
                    "rank1":2.0,
                    "super_anchor":3.0
                }),
                1.0,
            ),
            (
                serde_json::json!({
                    "kind":"piecewise_min",
                    "baseline":3.0,
                    "rank30":2.0,
                    "rank1":2.0,
                    "super_anchor":1.0
                }),
                2.0,
            ),
            (
                serde_json::json!({
                    "kind":"piecewise_min",
                    "baseline":4.0,
                    "rank30":3.0,
                    "rank1":2.0,
                    "super_anchor":1.0
                }),
                0.0,
            ),
            (
                serde_json::json!({
                    "kind":"piecewise_log_min",
                    "baseline":3.0,
                    "rank30":2.0,
                    "rank1":1.0,
                    "super_anchor":0.0
                }),
                1.0,
            ),
        ];
        for (spec, raw) in cases {
            let value = rescale(spec, raw);
            assert!(value.is_finite());
            assert_eq!(value, 0.0);
        }
        for raw in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let value = normalize_raw(None, raw).unwrap();
            assert!(value.is_finite());
            assert_eq!(value, 0.0);
        }
    }

    #[test]
    fn normalize_raw_without_rescale_maps_0_100_to_0_1() {
        // Tasks without a rescale_hint output raw scores already in the
        // 0–100 range (e.g. structured_json judges).  normalize_raw(None, _)
        // must clamp to 0–100 and divide by 100 so that e.g. a raw 15.1
        // becomes 0.151 — not a clamped 1.0 perfect score.
        assert_eq!(normalize_raw(None, 0.0).unwrap(), 0.0);
        assert_eq!(normalize_raw(None, 50.0).unwrap(), 0.5);
        assert_eq!(normalize_raw(None, 100.0).unwrap(), 1.0);
        assert_eq!(normalize_raw(None, 15.1).unwrap(), 0.151);
        // out-of-range values are clamped to the 0–100 band before scaling
        assert_eq!(normalize_raw(None, 150.0).unwrap(), 1.0);
        assert_eq!(normalize_raw(None, -10.0).unwrap(), 0.0);
    }

    #[test]
    fn piecewise_segments_are_clipped_to_their_score_bands() {
        let max = serde_json::json!({
            "kind":"piecewise_max",
            "baseline":0.0,
            "rank30":10.0,
            "rank1":20.0,
            "super_anchor":30.0
        });
        assert_eq!(rescale(max.clone(), 5.0), 0.1);
        assert_eq!(rescale(max.clone(), 15.0), 0.5);
        assert_eq!(rescale(max, 25.0), 0.9);

        let min = serde_json::json!({
            "kind":"piecewise_min",
            "baseline":30.0,
            "rank30":20.0,
            "rank1":10.0,
            "super_anchor":0.0
        });
        assert_eq!(rescale(min.clone(), 25.0), 0.1);
        assert_eq!(rescale(min.clone(), 15.0), 0.5);
        assert_eq!(rescale(min, 5.0), 0.9);
    }

    #[cfg(unix)]
    #[test]
    fn judge_runs_after_successful_copy_but_not_after_copy_failure() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("submission");
        let destination = root.path().join("workspace");
        let bin = root.path().join("bin");
        let marker = root.path().join("judge-ran");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(source.join("answer"), "42").unwrap();
        // The timeout_runner is now a plain shell command (no python3);
        // use a script that touches the marker file.
        std::fs::write(bin.join("judge-mock"), "#!/bin/sh\n: > \"$MARKER\"\n").unwrap();
        std::fs::set_permissions(
            bin.join("judge-mock"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let path = format!("{}:/usr/bin:/bin", bin.display());

        // Successful copy → judge runs
        let command = legacy_judge_command(
            source.to_str().unwrap(),
            destination.to_str().unwrap(),
            "judge-mock",
        );
        let output = Command::new("/bin/bash")
            .args(["-c", &command])
            .env("PATH", &path)
            .env("MARKER", &marker)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("answer")).unwrap(),
            "42"
        );
        assert!(marker.is_file());

        // Copy failure (source missing) → judge does not run
        std::fs::remove_file(&marker).unwrap();
        let command = legacy_judge_command(
            root.path().join("missing").to_str().unwrap(),
            destination.to_str().unwrap(),
            "judge-mock",
        );
        let output = Command::new("/bin/bash")
            .args(["-c", &command])
            .env("PATH", path)
            .env("MARKER", &marker)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!marker.exists());
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
