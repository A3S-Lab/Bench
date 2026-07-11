use a3s_acl::{Block, Value};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub use crate::judge_source::LegacyJudgeSource;

#[derive(Debug, Clone, Serialize)]
pub struct MetricInfo {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceSeed {
    pub image: String,
    pub source_path: String,
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmissionPolicy {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_file_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub name: String,
    pub category: String,
    pub judge_asset: String,
    pub work_image: String,
    pub work_platform: Option<String>,
    pub metrics: Vec<MetricInfo>,
    pub workspace_seed: Option<WorkspaceSeed>,
    pub submission: SubmissionPolicy,
    pub legacy_judge: Option<LegacyJudgeSource>,
    pub root: PathBuf,
}

pub fn load_local(reference: &Path) -> Result<TaskInfo> {
    let metadata = std::fs::symlink_metadata(reference)
        .with_context(|| format!("Task source does not exist: {}", reference.display()))?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink(),
        "Task source must not be a symlink"
    );
    let (root, acl_path) = if metadata.is_dir() {
        (reference.to_path_buf(), reference.join("task.acl"))
    } else {
        anyhow::ensure!(
            reference.file_name().and_then(|v| v.to_str()) == Some("task.acl"),
            "local Task file must be named task.acl"
        );
        (
            reference
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            reference.to_path_buf(),
        )
    };
    let source = std::fs::read_to_string(&acl_path)
        .with_context(|| format!("could not read {}", acl_path.display()))?;
    let document = a3s_acl::parse(&source)
        .map_err(|error| anyhow::anyhow!("invalid {}: {error}", acl_path.display()))?;
    anyhow::ensure!(
        document.blocks.len() == 1,
        "task.acl must contain exactly one root block"
    );
    let block = &document.blocks[0];
    anyhow::ensure!(
        block.name == "bench" && block.labels.len() == 1,
        "root block must be bench \"<task-id>\""
    );
    validate_task_schema(block)?;
    require_string(block, "schema", Some("a3s-bench/task/v1"))?;
    require_string(block, "version", None)?;
    let judge = unique_block(block, "judge")?;
    let judge_asset = require_string(judge, "asset", None)?.to_owned();
    let work = unique_block(block, "work")?;
    let image = unique_block(work, "image")?;
    let work_image = require_string(image, "ref", None)?.to_owned();
    let metrics = block
        .blocks
        .iter()
        .filter(|child| child.name == "metric")
        .map(parse_metric)
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        metrics
            .iter()
            .filter(|metric| metric.role == "primary")
            .count()
            == 1,
        "Task must declare exactly one primary metric"
    );
    let workspace_seed = parse_workspace_seed(block)?;
    let submission = parse_submission(block)?;
    let legacy_judge = crate::judge_source::load(&root.join("private/judge/judge.source.json"))?;
    let judge_path = root.join(&judge_asset);
    if !judge_asset.starts_with("oci://")
        && !judge_asset.starts_with("asset:")
        && !judge_asset.starts_with("asset://")
        && !judge_asset.starts_with("https://")
    {
        anyhow::ensure!(
            judge_path.join(".a3s/asset.acl").is_file(),
            "local Judge Asset is missing {}/.a3s/asset.acl",
            judge_path.display()
        );
    }
    Ok(TaskInfo {
        id: block.labels[0].clone(),
        name: require_string(block, "name", None)?.to_owned(),
        category: require_string(block, "category", None)?.to_owned(),
        judge_asset,
        work_image,
        work_platform: workspace_seed
            .as_ref()
            .and_then(|seed| seed.platform.clone()),
        metrics,
        workspace_seed,
        submission,
        legacy_judge,
        root,
    })
}

fn parse_submission(root: &Block) -> Result<SubmissionPolicy> {
    let matches: Vec<_> = root
        .blocks
        .iter()
        .filter(|block| block.name == "submission")
        .collect();
    anyhow::ensure!(
        matches.len() <= 1,
        "Task may contain at most one submission block"
    );
    let (include, exclude) = if let Some(block) = matches.first() {
        (
            string_list(block, "include")?,
            string_list(block, "exclude")?,
        )
    } else {
        (
            vec!["**".into()],
            vec![".git".into(), "node_modules".into(), "target".into()],
        )
    };
    let policy = SubmissionPolicy {
        include,
        exclude,
        max_files: 50_000,
        max_total_bytes: 536_870_912,
        max_file_bytes: 67_108_864,
    };
    crate::submission::validate_policy(&policy)?;
    Ok(policy)
}

fn string_list(block: &Block, name: &str) -> Result<Vec<String>> {
    let value = block
        .attributes
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("{}.{} must be a list", block.name, name))?;
    let Value::List(items) = value else {
        anyhow::bail!("{}.{} must be a list", block.name, name);
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("{}.{} items must be strings", block.name, name))
        })
        .collect()
}

fn validate_task_schema(root: &Block) -> Result<()> {
    use crate::acl_schema::{validate_block, BlockSchema, Labels};

    validate_block(
        root,
        "bench",
        BlockSchema {
            attributes: &[
                "schema",
                "version",
                "name",
                "category",
                "description",
                "tags",
            ],
            children: &["workspace", "work", "submission", "judge", "metric"],
            labels: Labels::Exactly(1),
        },
    )?;
    for block in &root.blocks {
        let (attributes, children, labels): (&[&str], &[&str], Labels) = match block.name.as_str() {
            "workspace" => (&[], &["oci"], Labels::None),
            "oci" => unreachable!("OCI is nested under workspace"),
            "work" => (&["network_need"], &["image"], Labels::None),
            "submission" => (&["include", "exclude"], &[], Labels::None),
            "judge" => (
                &["asset", "solution_timeout_sec"],
                &["requirements"],
                Labels::None,
            ),
            "metric" => (
                &[
                    "type",
                    "role",
                    "direction",
                    "min",
                    "max",
                    "normalization",
                    "gate",
                    "gate_failure_score_basis_points",
                    "solution_failure_value",
                    "public_report",
                ],
                &["measurement"],
                Labels::Exactly(1),
            ),
            _ => unreachable!("root child names were validated"),
        };
        validate_block(
            block,
            &format!("bench.{}", block.name),
            BlockSchema {
                attributes,
                children,
                labels,
            },
        )?;
        for child in &block.blocks {
            let attributes: &[&str] = match child.name.as_str() {
                "oci" => &["ref", "platform", "source_path"],
                "image" => &["ref", "platform"],
                "requirements" => &["cohort"],
                "measurement" => &[
                    "warmup_repeats",
                    "measured_repeats",
                    "estimator",
                    "outlier_policy",
                    "tolerance",
                ],
                _ => unreachable!("nested child names were validated"),
            };
            validate_block(
                child,
                &format!("bench.{}.{}", block.name, child.name),
                BlockSchema {
                    attributes,
                    children: &[],
                    labels: Labels::None,
                },
            )?;
        }
    }
    Ok(())
}

fn parse_workspace_seed(block: &Block) -> Result<Option<WorkspaceSeed>> {
    let matches: Vec<_> = block
        .blocks
        .iter()
        .filter(|child| child.name == "workspace")
        .collect();
    anyhow::ensure!(
        matches.len() <= 1,
        "Task may contain at most one workspace block"
    );
    let Some(workspace) = matches.first() else {
        return Ok(None);
    };
    let oci = unique_block(workspace, "oci")?;
    let source_path = require_string(oci, "source_path", None)?;
    anyhow::ensure!(
        source_path.starts_with('/'),
        "workspace.oci.source_path must be absolute"
    );
    Ok(Some(WorkspaceSeed {
        image: require_string(oci, "ref", None)?.to_owned(),
        source_path: source_path.to_owned(),
        platform: oci
            .attributes
            .get("platform")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }))
}

fn parse_metric(block: &Block) -> Result<MetricInfo> {
    anyhow::ensure!(block.labels.len() == 1, "metric block must have one label");
    let min = block
        .attributes
        .get("min")
        .and_then(Value::as_number)
        .ok_or_else(|| anyhow::anyhow!("metric.min must be a number"))?;
    let max = block
        .attributes
        .get("max")
        .and_then(Value::as_number)
        .ok_or_else(|| anyhow::anyhow!("metric.max must be a number"))?;
    anyhow::ensure!(
        min.is_finite() && max.is_finite() && min <= max,
        "invalid metric range"
    );
    Ok(MetricInfo {
        name: block.labels[0].clone(),
        min,
        max,
        role: require_string(block, "role", None)?.to_owned(),
    })
}

fn require_string<'a>(block: &'a Block, key: &str, expected: Option<&str>) -> Result<&'a str> {
    let value = block
        .attributes
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{}.{} must be a string", block.name, key))?;
    if let Some(expected) = expected {
        anyhow::ensure!(
            value == expected,
            "{}.{} must be {:?}",
            block.name,
            key,
            expected
        );
    }
    Ok(value)
}

fn unique_block<'a>(block: &'a Block, name: &str) -> Result<&'a Block> {
    let matches: Vec<&Block> = block
        .blocks
        .iter()
        .filter(|child| child.name == name)
        .collect();
    anyhow::ensure!(
        matches.len() == 1,
        "{} must contain exactly one {} block",
        block.name,
        name
    );
    Ok(matches[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_smoke_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/smoke");
        let task = load_local(&root).unwrap();
        assert_eq!(task.id, "smoke_answer");
        assert_eq!(task.judge_asset, "private/judge");
        assert_eq!(task.work_image, "docker.io/library/alpine:3.20");
        assert_eq!(task.metrics[0].name, "correctness");
    }

    #[test]
    fn loads_builtin_workspace_seed() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks/ad_placement_optimization");
        let task = load_local(&root).unwrap();
        let seed = task.workspace_seed.unwrap();
        assert!(seed.image.starts_with("docker.io/seededge/"));
        assert_eq!(seed.source_path, "/home/workspace/ad-placement");
    }

    #[test]
    fn rejects_unknown_task_attributes_and_blocks() {
        for source in [
            r#"bench "test" {
              schema = "a3s-bench/task/v1"
              version = "0.1.0"
              typo = true
            }"#,
            r#"bench "test" {
              schema = "a3s-bench/task/v1"
              version = "0.1.0"
              work { image { ref = "alpine" typo = true } }
            }"#,
        ] {
            let document = a3s_acl::parse(source).unwrap();
            assert!(validate_task_schema(&document.blocks[0]).is_err());
        }
    }

    #[test]
    fn all_example_task_descriptors_use_the_closed_schema() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        for entry in std::fs::read_dir(examples).unwrap() {
            let path = entry.unwrap().path();
            if path.join("task.acl").is_file() {
                load_local(&path).unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));
            }
        }
    }
}
