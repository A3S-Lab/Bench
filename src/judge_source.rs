use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct LegacyJudgeSource {
    pub image: String,
    pub command: String,
    pub mode: String,
    pub parser: String,
    pub workspace_source_path: String,
    pub rescale: Option<serde_json::Value>,
    pub platform: Option<String>,
    pub game_server_command: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JudgeSourceDescriptor {
    schema: String,
    admission: String,
    kind: String,
    requirements: Vec<String>,
    evaluation: Evaluation,
    image: Image,
    source_result: SourceResult,
    workspace: Workspace,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evaluation {
    mode: String,
    source_command: String,
    source_game_server_command: Option<String>,
    timeout_sec: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    digest_resolution: String,
    platform: Option<String>,
    #[serde(rename = "ref")]
    reference: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceResult {
    kind: String,
    parser: String,
    rescale_hint: Option<RescaleHint>,
    score_direction: String,
    selection_hint: String,
    target_metric: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Workspace {
    source_path: String,
    submission_exclude: Vec<String>,
    submission_paths: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum RescaleHint {
    #[serde(rename = "linear")]
    Linear { lower: f64, upper: f64 },
    #[serde(rename = "log_anchor")]
    LogAnchor { anchor_raw: f64, anchor_score: f64 },
    #[serde(rename = "piecewise_max")]
    PiecewiseMax {
        baseline: f64,
        rank1: f64,
        rank30: f64,
        super_anchor: f64,
    },
    #[serde(rename = "log_max")]
    LogMax { baseline: f64, expert: f64 },
    #[serde(rename = "piecewise_log_min")]
    PiecewiseLogMin {
        baseline: f64,
        rank1: f64,
        rank30: f64,
        super_anchor: f64,
    },
    #[serde(rename = "log1p_max")]
    Log1pMax { baseline: f64, upper: f64 },
    #[serde(rename = "log_min")]
    LogMin { baseline: f64, expert: f64 },
    #[serde(rename = "piecewise_min")]
    PiecewiseMin {
        baseline: f64,
        rank1: f64,
        rank30: f64,
        super_anchor: f64,
    },
}

pub fn load(path: &Path) -> Result<Option<LegacyJudgeSource>> {
    let Some(bytes) = crate::state_fs::read_optional_regular_file(path, "Judge source descriptor")?
    else {
        return Ok(None);
    };
    let descriptor: JudgeSourceDescriptor = serde_json::from_slice(&bytes)?;
    validate(&descriptor)?;
    Ok(Some(LegacyJudgeSource {
        image: descriptor.image.reference,
        command: descriptor.evaluation.source_command,
        mode: descriptor.evaluation.mode,
        parser: descriptor.source_result.parser,
        workspace_source_path: descriptor.workspace.source_path,
        rescale: descriptor
            .source_result
            .rescale_hint
            .map(serde_json::to_value)
            .transpose()?,
        platform: descriptor.image.platform,
        game_server_command: descriptor
            .evaluation
            .source_game_server_command
            .filter(|command| !command.is_empty()),
    }))
}

fn validate(descriptor: &JudgeSourceDescriptor) -> Result<()> {
    anyhow::ensure!(
        descriptor.schema == "a3s-bench/judge-source/v1",
        "unsupported Judge source descriptor schema"
    );
    anyhow::ensure!(
        descriptor.admission == "quarantined",
        "invalid Judge source admission"
    );
    anyhow::ensure!(descriptor.kind == "oci", "unsupported Judge source kind");
    anyhow::ensure!(
        !descriptor.requirements.is_empty(),
        "Judge source requirements are empty"
    );
    anyhow::ensure!(
        descriptor.evaluation.timeout_sec > 0,
        "Judge source timeout must be positive"
    );
    anyhow::ensure!(
        matches!(descriptor.evaluation.mode.as_str(), "batch" | "game_server"),
        "unsupported Judge source mode"
    );
    if descriptor.evaluation.mode == "batch" {
        anyhow::ensure!(
            !descriptor.evaluation.source_command.is_empty(),
            "batch Judge command is empty"
        );
    } else {
        anyhow::ensure!(
            descriptor
                .evaluation
                .source_game_server_command
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
            "game Judge server command is empty"
        );
    }
    anyhow::ensure!(
        !descriptor.image.reference.is_empty(),
        "Judge image reference is empty"
    );
    anyhow::ensure!(
        descriptor.image.digest_resolution == "required_at_task_lock",
        "unsupported Judge image digest resolution"
    );
    anyhow::ensure!(
        descriptor.source_result.kind == "legacy_stdout",
        "unsupported Judge source result kind"
    );
    anyhow::ensure!(
        matches!(
            descriptor.source_result.score_direction.as_str(),
            "maximize" | "minimize"
        ),
        "unsupported Judge score direction"
    );
    anyhow::ensure!(
        matches!(
            descriptor.source_result.selection_hint.as_str(),
            "score_first" | "pass_rate_first" | "valid_then_score"
        ),
        "unsupported Judge selection hint"
    );
    anyhow::ensure!(
        !descriptor.source_result.target_metric.is_empty(),
        "Judge target metric is empty"
    );
    anyhow::ensure!(
        descriptor.workspace.source_path.starts_with('/'),
        "Judge workspace source path must be absolute"
    );
    let _ = (
        &descriptor.workspace.submission_exclude,
        &descriptor.workspace.submission_paths,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_51_imported_descriptors_use_the_closed_schema() {
        let tasks = Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks");
        let mut count = 0;
        for entry in std::fs::read_dir(tasks).unwrap() {
            let path = entry
                .unwrap()
                .path()
                .join("private/judge/judge.source.json");
            assert!(load(&path).unwrap().is_some(), "{}", path.display());
            count += 1;
        }
        assert_eq!(count, 51);
    }

    #[test]
    fn rejects_unknown_nested_and_rescale_fields() {
        let base = serde_json::json!({
            "schema":"a3s-bench/judge-source/v1", "admission":"quarantined", "kind":"oci",
            "requirements":["oci_judge_admission"],
            "evaluation":{"mode":"batch","source_command":"true","source_game_server_command":null,"timeout_sec":1},
            "image":{"digest_resolution":"required_at_task_lock","platform":"linux/amd64","ref":"image"},
            "source_result":{"kind":"legacy_stdout","parser":"structured_json","rescale_hint":{"kind":"linear","lower":0.0,"upper":1.0},"score_direction":"maximize","selection_hint":"score_first","target_metric":"score"},
            "workspace":{"source_path":"/workspace","submission_exclude":[],"submission_paths":[]}
        });
        let mut nested = base.clone();
        nested["evaluation"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<JudgeSourceDescriptor>(nested).is_err());
        let mut rescale = base;
        rescale["source_result"]["rescale_hint"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<JudgeSourceDescriptor>(rescale).is_err());
    }
}
