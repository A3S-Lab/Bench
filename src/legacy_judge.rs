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
    command.args(crate::runtime_profile::judge_docker_args(
        task.resources.judge,
    ));
    configure_model_gateway(&mut command, source.requires_model_gateway, model)?;
    if let Some(platform) = source.platform.as_deref() {
        command.args(["--platform", platform]);
    }
    // Judge command runs under `timeout` inside the container; no
    // environment variable passthrough is needed.
    let timeout_sec = source
        .timeout_sec
        .checked_add(if source.requires_model_gateway { 30 } else { 0 })
        .context("Judge timeout plus gateway cleanup grace overflowed")?;
    let timeout_runner = build_timeout_runner(timeout_sec, &source.command);
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
        .args(non_login_bash(&judge_command))
        .output()
        .context("could not start legacy OCI Judge")?;
    let mut raw = String::from_utf8_lossy(&output.stdout).into_owned();
    raw.push_str(&String::from_utf8_lossy(&output.stderr));
    anyhow::ensure!(raw.len() <= 16 * 1024 * 1024, "Judge output exceeds 16 MiB");

    let exit_code = output.status.code();

    let ratio = match classify_judge_exit(source, exit_code) {
        JudgeExitClass::Completed => parse_score(source, &raw)?,
        JudgeExitClass::CandidateTimeout => {
            eprintln!("Judge exceeded descriptor timeout; scoring 0.0");
            0.0
        }
        JudgeExitClass::ModelGatewayTimeout => {
            anyhow::bail!("model-backed Judge exceeded descriptor timeout");
        }
        JudgeExitClass::Abnormal => {
            let snippet: String = raw.chars().take(4096).collect();
            anyhow::bail!(
                "Judge process terminated abnormally (exit_code: {:?}): {}",
                exit_code,
                snippet
            );
        }
    };

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
            "parser": source.parser,
            "output_head": output_head(&raw, 4096),
            "output_tail": output_tail(&raw, 4096)
        }),
    })
}

fn output_head(value: &str, max_chars: usize) -> String {
    let end = value
        .char_indices()
        .nth(max_chars)
        .map_or(value.len(), |(index, _)| index);
    value[..end].to_owned()
}

fn output_tail(value: &str, max_chars: usize) -> String {
    let start = value
        .char_indices()
        .rev()
        .nth(max_chars.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    value[start..].to_owned()
}

fn configure_judge_container(command: &mut Command) {
    command.args([
        "run",
        "--rm",
        "--user",
        "0:0",
        "--add-host",
        "host.docker.internal:host-gateway",
    ]);
    // Inject host DNS servers so Judge containers can resolve domains even
    // when Docker Desktop's bridge-network DNS forwarding is broken.
    command.args(crate::runtime_profile::host_dns_args());
}

#[derive(Debug, PartialEq, Eq)]
enum JudgeExitClass {
    Completed,
    CandidateTimeout,
    ModelGatewayTimeout,
    Abnormal,
}

fn classify_judge_exit(source: &LegacyJudgeSource, exit_code: Option<i32>) -> JudgeExitClass {
    match exit_code {
        // Docker reserves 125 for a failure to run the container and 126/127
        // for an uninvokable or missing container command. GNU timeout also
        // propagates 126/127 when its command cannot be invoked. None of
        // these statuses represents a scoreable candidate result.
        None | Some(125..=127 | 137 | 143) => JudgeExitClass::Abnormal,
        Some(124) if source.requires_model_gateway => JudgeExitClass::ModelGatewayTimeout,
        Some(124) => JudgeExitClass::CandidateTimeout,
        _ => JudgeExitClass::Completed,
    }
}

fn configure_model_gateway(
    command: &mut Command,
    required: bool,
    model: Option<&crate::config::ModelRoute>,
) -> Result<()> {
    if required {
        let model = model.ok_or_else(|| anyhow::anyhow!("Judge requires a model gateway route"))?;
        let base_url = container_base_url(&model.base_url)?;
        command
            .args(["--env", "SFORGE_JUDGE_API_KEY"])
            .args(["--env", "SFORGE_JUDGE_API_BASE_URL"])
            .args(["--env", "SFORGE_JUDGE_MODEL"])
            .env("SFORGE_JUDGE_API_KEY", &model.api_key)
            .env("SFORGE_JUDGE_API_BASE_URL", base_url)
            .env("SFORGE_JUDGE_MODEL", &model.model);
    }
    Ok(())
}

fn container_base_url(value: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(value)
        .map_err(|_| anyhow::anyhow!("Judge model base_url must be an absolute HTTP(S) URL"))?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Judge model base_url must use HTTP or HTTPS"
    );
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Judge model base_url must not contain user information"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "Judge model base_url must not contain a query or fragment"
    );

    let root_path = url.path() == "/";
    if root_path {
        url.set_path("/v1");
    }
    let local = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    if local {
        url.set_host(Some("host.docker.internal"))
            .map_err(|_| anyhow::anyhow!("could not map local Judge model base_url to Docker"))?;
        return Ok(url.into());
    }
    if root_path {
        Ok(url.into())
    } else {
        Ok(value.to_owned())
    }
}

fn non_login_bash(command: &str) -> [&str; 3] {
    ["/bin/bash", "-c", command]
}

fn build_timeout_runner(timeout_sec: u64, command: &str) -> String {
    format!(
        "timeout --kill-after=10 {} /bin/bash -c {}",
        timeout_sec,
        shell_quote(command),
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn legacy_judge_command(source: &str, destination: &str, timeout_runner: &str) -> String {
    let source = format!("{}/.", source.trim_end_matches('/'));
    let destination = shell_quote(destination);
    // The trusted judge container runs as root with Docker's default
    // capabilities, matching the imported EdgeBench harness.
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
                anyhow::ensure!(
                    !source.requires_model_gateway,
                    "model-backed structured Judge produced no structured result"
                );
                eprintln!("Judge produced no structured result; scoring 0.0");
                return Ok(0.0);
            };
            anyhow::ensure!(
                value.get("valid").and_then(Value::as_bool).unwrap_or(true),
                "structured Judge marked result invalid"
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
            let raw = extract_total_score(output)?.unwrap_or(0.0);
            normalize_raw(source.rescale.as_ref(), raw)
        }
        "pytest_v" => {
            let missing_score = match source.selection_hint.as_str() {
                "pass_rate_first" => pytest_ratio(output)?,
                "score_first" | "valid_then_score" => 0.0,
                value => anyhow::bail!("unsupported Judge selection policy {value:?}"),
            };
            match extract_total_score(output)? {
                Some(raw) => normalize_raw(source.rescale.as_ref(), raw),
                None => Ok(missing_score),
            }
        }
        value => anyhow::bail!("unsupported legacy Judge parser {value:?}"),
    }
}

fn extract_total_score(output: &str) -> Result<Option<f64>> {
    let expression = Regex::new(r"TOTAL_SCORE\s+(inf|[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)")?;
    Ok(expression
        .captures(output)
        .and_then(|captures| captures.get(1))
        .and_then(|value| value.as_str().parse::<f64>().ok()))
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
    value.to_string()
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

    #[test]
    fn canonical_ratio_preserves_small_nonzero_edgebench_scores() {
        assert_eq!(canonical_ratio(0.1), "0.1");
        assert_eq!(canonical_ratio(0.000000000033), "0.000000000033");
        assert_eq!(canonical_ratio(0.0), "0");
    }

    fn structured_source() -> LegacyJudgeSource {
        LegacyJudgeSource {
            image: "judge:latest".into(),
            command: "judge".into(),
            mode: "batch".into(),
            parser: "structured_json".into(),
            workspace_source_path: "/workspace".into(),
            rescale: None,
            selection_hint: "score_first".into(),
            platform: None,
            game_server_command: None,
            requires_model_gateway: false,
            timeout_sec: 60,
        }
    }

    fn pytest_v_source(rescale: Option<Value>) -> LegacyJudgeSource {
        LegacyJudgeSource {
            parser: "pytest_v".into(),
            rescale,
            selection_hint: "pass_rate_first".into(),
            ..structured_source()
        }
    }

    fn score_sum_source(rescale: Option<Value>) -> LegacyJudgeSource {
        LegacyJudgeSource {
            parser: "score_sum".into(),
            rescale,
            ..structured_source()
        }
    }

    #[test]
    fn total_score_extraction_distinguishes_missing_zero_scientific_and_infinite() {
        assert_eq!(extract_total_score("no score here").unwrap(), None);
        assert_eq!(extract_total_score("TOTAL_SCORE 0").unwrap(), Some(0.0));
        assert_eq!(
            extract_total_score("TOTAL_SCORE 3.3e-9").unwrap(),
            Some(3.3e-9)
        );
        assert_eq!(
            extract_total_score("TOTAL_SCORE 1E+2").unwrap(),
            Some(100.0)
        );
        assert_eq!(
            extract_total_score("TOTAL_SCORE inf").unwrap(),
            Some(f64::INFINITY)
        );
    }

    #[test]
    fn pytest_v_selection_hint_does_not_affect_rescaled_total_score() {
        let output = "=== 4 passed, 1 failed in 0.28s ===\nTOTAL_SCORE 50.0\n";
        for selection_hint in ["score_first", "pass_rate_first", "valid_then_score"] {
            let mut source = pytest_v_source(Some(
                serde_json::json!({"kind":"linear","lower":0.0,"upper":100.0}),
            ));
            source.selection_hint = selection_hint.into();
            assert_eq!(
                parse_score(&source, output).unwrap(),
                0.5,
                "selection_hint={selection_hint}"
            );
        }
    }

    #[test]
    fn pytest_v_applies_non_linear_rescale_to_total_score() {
        let source = pytest_v_source(Some(serde_json::json!({
            "kind":"log_anchor",
            "anchor_raw":14.155,
            "anchor_score":43.0
        })));
        let output = "=== 1 passed in 0.01s ===\nTOTAL_SCORE 14.155\n";
        assert!((parse_score(&source, output).unwrap() - 0.43).abs() < 0.001);
    }

    #[test]
    fn pytest_v_distinguishes_explicit_zero_from_missing_total_score() {
        let source = pytest_v_source(Some(
            serde_json::json!({"kind":"linear","lower":0.0,"upper":100.0}),
        ));
        let summary = "=== 4 passed, 1 failed in 0.28s ===";
        assert_eq!(parse_score(&source, summary).unwrap(), 0.8);
        assert_eq!(
            parse_score(&source, &format!("{summary}\nTOTAL_SCORE 0\n")).unwrap(),
            0.0
        );
    }

    #[test]
    fn pytest_v_keeps_total_score_when_an_unrelated_test_fails() {
        let source = pytest_v_source(Some(
            serde_json::json!({"kind":"linear","lower":0.0,"upper":100.0}),
        ));
        let output = concat!(
            "tests/test_final_result.py::test_private_judge_files_are_available FAILED\n",
            "tests/test_final_result.py::test_final_result_is_valid_permutation_and_cost ",
            "TOTAL_SCORE 0.0000000033\nPASSED\n",
            "========================= 1 failed, 4 passed =========================\n",
        );
        assert_eq!(parse_score(&source, output).unwrap(), 0.000000000033);
    }

    #[test]
    fn pytest_v_missing_total_score_falls_back_at_full_pass_rate() {
        let source = pytest_v_source(Some(
            serde_json::json!({"kind":"linear","lower":0.0,"upper":100.0}),
        ));
        assert_eq!(
            parse_score(&source, "=== 2 passed in 0.01s ===").unwrap(),
            1.0
        );
    }

    #[test]
    fn pytest_v_missing_total_score_follows_selection_hint() {
        let output = "=== 4 passed, 1 failed in 0.28s ===";
        for (selection_hint, expected) in [
            ("score_first", 0.0),
            ("pass_rate_first", 0.8),
            ("valid_then_score", 0.0),
        ] {
            let mut source = pytest_v_source(None);
            source.selection_hint = selection_hint.into();
            assert_eq!(
                parse_score(&source, output).unwrap(),
                expected,
                "selection_hint={selection_hint}"
            );
        }
        let mut unknown = pytest_v_source(None);
        unknown.selection_hint = "unknown".into();
        assert!(parse_score(&unknown, output).is_err());
    }

    #[test]
    fn infinite_total_score_normalizes_to_zero_for_both_legacy_parsers() {
        let pytest = pytest_v_source(None);
        let score_sum = score_sum_source(None);
        let output = "=== 1 passed in 0.01s ===\nTOTAL_SCORE inf\n";
        assert_eq!(parse_score(&pytest, output).unwrap(), 0.0);
        assert_eq!(parse_score(&score_sum, output).unwrap(), 0.0);
    }

    #[test]
    fn score_sum_reuses_total_score_extraction() {
        let source = score_sum_source(Some(
            serde_json::json!({"kind":"linear","lower":0.0,"upper":1.0}),
        ));
        assert_eq!(parse_score(&source, "TOTAL_SCORE 2.5e-1").unwrap(), 0.25);
        assert_eq!(parse_score(&source, "no score").unwrap(), 0.0);
    }

    #[test]
    fn exchange_score_sum_exact_regression_normalizes_below_baseline_to_zero() {
        let source = score_sum_source(Some(serde_json::json!({
            "kind": "log_max",
            "baseline": 2090.634594076933,
            "expert": 6000.0
        })));
        assert_eq!(parse_score(&source, "TOTAL_SCORE 1078").unwrap(), 0.0);
    }

    #[test]
    fn output_excerpt_is_bounded_and_utf8_safe() {
        assert_eq!(output_head("abcdef", 3), "abc");
        assert_eq!(output_tail("abcdef", 3), "def");
        assert_eq!(
            output_tail("\u{7532}\u{4e59}\u{4e19}\u{4e01}", 2),
            "\u{4e19}\u{4e01}"
        );
        assert_eq!(output_head("short", 10), "short");
        assert_eq!(output_tail("short", 10), "short");
    }

    #[test]
    fn missing_structured_result_scores_zero() {
        let source = structured_source();
        assert_eq!(parse_score(&source, "compiler error").unwrap(), 0.0);
    }

    #[test]
    fn model_backed_structured_result_requires_a_result_marker() {
        let mut source = structured_source();
        source.requires_model_gateway = true;
        let output = concat!(
            "[codex-judge] model=glm-5.2, timeout=900s\n",
            "[codex-judge] Running codex attempt 1/3...\nTerminated\n"
        );
        let error = parse_score(&source, output).unwrap_err();
        assert!(error
            .to_string()
            .contains("model-backed structured Judge produced no structured result"));
    }

    #[test]
    fn invalid_structured_result_is_not_promoted_to_a_scoreable_result() {
        let error = parse_score(
            &structured_source(),
            ">>>>> Start Structured Result\n{\"valid\":false,\"score\":0,\"metrics\":{\"grader_status\":\"codex_failed\"}}\n>>>>> End Structured Result",
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("structured Judge marked result invalid"));
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
    fn judge_container_matches_edgebench_trusted_judge_baseline() {
        let mut command = Command::new("docker");
        configure_judge_container(&mut command);
        let arguments = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(arguments.windows(2).any(|pair| pair == ["--user", "0:0"]));
        assert!(arguments
            .windows(2)
            .any(|pair| { pair == ["--add-host", "host.docker.internal:host-gateway"] }));
        assert!(!arguments.iter().any(|argument| argument == "--cap-drop"));
        assert!(!arguments.iter().any(|argument| argument == "--cap-add"));
        assert!(!arguments
            .iter()
            .any(|argument| argument == "--security-opt"));
    }

    #[test]
    fn judge_commands_use_non_login_bash_and_default_network() {
        assert_eq!(non_login_bash("judge"), ["/bin/bash", "-c", "judge"]);
        assert_eq!(
            build_timeout_runner(15, "judge"),
            "timeout --kill-after=10 15 /bin/bash -c 'judge'"
        );
        let mut command = Command::new("docker");
        configure_model_gateway(&mut command, false, None).unwrap();
        assert!(!command.get_args().any(|argument| argument == "--network"));
    }

    #[test]
    fn judge_exit_classification_separates_candidate_and_infrastructure_failures() {
        let source = structured_source();
        for exit_code in [None, Some(125), Some(126), Some(127), Some(137), Some(143)] {
            assert_eq!(
                classify_judge_exit(&source, exit_code),
                JudgeExitClass::Abnormal
            );
        }
        // Timeout (124) is not abnormal — it means the candidate was too slow.
        assert_eq!(
            classify_judge_exit(&source, Some(124)),
            JudgeExitClass::CandidateTimeout
        );
        // Ordinary non-zero Judge exits remain scoreable: pytest uses 1 for
        // test failures and 2 for interrupted/collection failures, and both
        // can be caused by the candidate submission.
        for exit_code in [Some(0), Some(1), Some(2)] {
            assert_eq!(
                classify_judge_exit(&source, exit_code),
                JudgeExitClass::Completed
            );
        }
        let mut model_source = source;
        model_source.requires_model_gateway = true;
        assert_eq!(
            classify_judge_exit(&model_source, Some(124)),
            JudgeExitClass::ModelGatewayTimeout
        );
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
            base_url: "https://example.test".into(),
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
            environment["SFORGE_JUDGE_API_BASE_URL"].as_deref(),
            Some("https://example.test/v1")
        );
        assert_eq!(
            container_base_url("http://127.0.0.1:8080/v1").unwrap(),
            "http://host.docker.internal:8080/v1"
        );
        assert_eq!(
            container_base_url("https://api.example.test/v1").unwrap(),
            "https://api.example.test/v1"
        );
    }

    #[test]
    fn container_base_url_normalization_is_conservative() {
        let cases = [
            (
                "https://token.pjlab.org.cn",
                "https://token.pjlab.org.cn/v1",
            ),
            (
                "https://token.pjlab.org.cn/",
                "https://token.pjlab.org.cn/v1",
            ),
            ("https://api.example.test/v1", "https://api.example.test/v1"),
            (
                "https://api.example.test/api/v1",
                "https://api.example.test/api/v1",
            ),
            (
                "https://api.example.test/custom/",
                "https://api.example.test/custom/",
            ),
            (
                "http://localhost:8080",
                "http://host.docker.internal:8080/v1",
            ),
            (
                "http://127.0.0.1:8080/",
                "http://host.docker.internal:8080/v1",
            ),
            (
                "http://[::1]:8080/api/v1",
                "http://host.docker.internal:8080/api/v1",
            ),
            (
                "http://localhost.example:8080/",
                "http://localhost.example:8080/v1",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(container_base_url(input).unwrap(), expected, "{input}");
        }

        for input in [
            "not a URL",
            "https://",
            "ftp://api.example.test/",
            "https://api.example.test/?api-version=2026",
            "https://api.example.test/v1?api-version=2026",
            "https://api.example.test/v1#fragment",
            "https://user:secret@api.example.test/",
        ] {
            assert!(container_base_url(input).is_err(), "{input}");
        }
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
