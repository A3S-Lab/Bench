use a3s_acl::{Block, Value};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
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

pub const MAX_CPU_LIMIT: u64 = 64;
pub const MAX_MEMORY_BYTES: u64 = 256 * 1024 * 1024 * 1024;
pub const MAX_NETWORK_ALLOW_HOSTS: usize = 64;

pub const LEGACY_WORK_RESOURCES: RoleResources = RoleResources {
    cpu_limit: 4,
    memory_bytes: 8 * 1024 * 1024 * 1024,
};

pub const LEGACY_JUDGE_RESOURCES: RoleResources = RoleResources {
    cpu_limit: 4,
    memory_bytes: 16 * 1024 * 1024 * 1024,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleResources {
    pub cpu_limit: u64,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResources {
    pub work: RoleResources,
    pub judge: RoleResources,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkWorkspaceImport {
    pub name: String,
    pub source_path: String,
    pub target_path: String,
}

impl Default for TaskResources {
    fn default() -> Self {
        Self {
            work: LEGACY_WORK_RESOURCES,
            judge: LEGACY_JUDGE_RESOURCES,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskInfo {
    #[serde(skip_serializing)]
    pub schema: String,
    pub id: String,
    pub name: String,
    pub category: String,
    pub judge_asset: String,
    pub work_image: String,
    pub work_platform: Option<String>,
    pub work_network_need: String,
    pub work_network_allow_hosts: Vec<String>,
    pub candidate_timeout_sec: u64,
    pub metrics: Vec<MetricInfo>,
    pub workspace_seed: Option<WorkspaceSeed>,
    pub submission: SubmissionPolicy,
    pub resources: TaskResources,
    pub work_workspace_imports: Vec<WorkWorkspaceImport>,
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
    let schema = require_string(block, "schema", None)?;
    anyhow::ensure!(
        matches!(schema, "a3s-bench/task/v1" | "a3s-bench/task/v2"),
        "bench.schema must be a3s-bench/task/v1 or a3s-bench/task/v2"
    );
    require_string(block, "version", None)?;
    let judge = unique_block(block, "judge")?;
    let judge_asset = require_string(judge, "asset", None)?.to_owned();
    let candidate_timeout_sec =
        optional_positive_integer(judge, "solution_timeout_sec")?.unwrap_or(300);
    let work = unique_block(block, "work")?;
    let work_network_need = work
        .attributes
        .get("network_need")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_owned();
    anyhow::ensure!(
        matches!(
            work_network_need.as_str(),
            "none" | "restricted_https" | "public_internet"
        ),
        "work.network_need must be none, restricted_https, or public_internet"
    );
    let work_network_allow_hosts =
        parse_work_network_allow_hosts(work, schema, &work_network_need)?;
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
    let resources = TaskResources {
        work: parse_role_resources(work, schema, LEGACY_WORK_RESOURCES)?,
        judge: parse_role_resources(judge, schema, LEGACY_JUDGE_RESOURCES)?,
    };
    let work_workspace_imports = parse_work_workspace_imports(work, schema)?;
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
        schema: schema.to_owned(),
        id: block.labels[0].clone(),
        name: require_string(block, "name", None)?.to_owned(),
        category: require_string(block, "category", None)?.to_owned(),
        judge_asset,
        work_image,
        work_platform: workspace_seed
            .as_ref()
            .and_then(|seed| seed.platform.clone()),
        work_network_need,
        work_network_allow_hosts,
        candidate_timeout_sec,
        metrics,
        workspace_seed,
        submission,
        resources,
        work_workspace_imports,
        legacy_judge,
        root,
    })
}

fn parse_work_network_allow_hosts(
    work: &Block,
    schema: &str,
    network_need: &str,
) -> Result<Vec<String>> {
    let mut hosts = if work.attributes.contains_key("https_allow_hosts") {
        anyhow::ensure!(
            schema == "a3s-bench/task/v2",
            "Task v1 does not support work.https_allow_hosts; use a3s-bench/task/v2"
        );
        string_list(work, "https_allow_hosts")?
    } else {
        Vec::new()
    };
    anyhow::ensure!(
        hosts.len() <= MAX_NETWORK_ALLOW_HOSTS,
        "work.https_allow_hosts exceeds the maximum of {MAX_NETWORK_ALLOW_HOSTS} hosts"
    );
    for host in &hosts {
        validate_network_allow_host(host)?;
    }
    hosts.sort_unstable();
    hosts.dedup();
    match network_need {
        "restricted_https" => anyhow::ensure!(
            !hosts.is_empty(),
            "work.network_need restricted_https requires work.https_allow_hosts"
        ),
        "none" | "public_internet" => anyhow::ensure!(
            hosts.is_empty(),
            "work.https_allow_hosts requires work.network_need restricted_https"
        ),
        _ => unreachable!("work.network_need was validated"),
    }
    Ok(hosts)
}

fn validate_network_allow_host(host: &str) -> Result<()> {
    anyhow::ensure!(
        !host.is_empty()
            && host.len() <= 253
            && host.is_ascii()
            && host.bytes().all(|byte| !byte.is_ascii_control())
            && host.bytes().all(|byte| !byte.is_ascii_uppercase())
            && !host.ends_with('.'),
        "work.https_allow_hosts entries must be lowercase canonical ASCII DNS hostnames"
    );
    anyhow::ensure!(
        host.parse::<std::net::IpAddr>().is_err(),
        "work.https_allow_hosts entries must not be IP literals"
    );
    anyhow::ensure!(
        host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }),
        "work.https_allow_hosts entries must be exact DNS hostnames without URLs, ports, or wildcards"
    );
    Ok(())
}

fn parse_role_resources(
    block: &Block,
    schema: &str,
    legacy_default: RoleResources,
) -> Result<RoleResources> {
    if schema == "a3s-bench/task/v1" {
        anyhow::ensure!(
            !block.attributes.contains_key("cpu_limit")
                && !block.attributes.contains_key("memory_bytes"),
            "Task v1 does not support {} resource fields; use a3s-bench/task/v2",
            block.name
        );
        return Ok(legacy_default);
    }
    let resources = RoleResources {
        cpu_limit: required_positive_integer(block, "cpu_limit")?,
        memory_bytes: required_positive_integer(block, "memory_bytes")?,
    };
    anyhow::ensure!(
        resources.cpu_limit <= MAX_CPU_LIMIT,
        "{}.cpu_limit exceeds operator maximum {}",
        block.name,
        MAX_CPU_LIMIT
    );
    anyhow::ensure!(
        resources.memory_bytes <= MAX_MEMORY_BYTES,
        "{}.memory_bytes exceeds operator maximum {}",
        block.name,
        MAX_MEMORY_BYTES
    );
    Ok(resources)
}

fn parse_work_workspace_imports(work: &Block, schema: &str) -> Result<Vec<WorkWorkspaceImport>> {
    let imports = work
        .blocks
        .iter()
        .filter(|block| block.name == "workspace_import")
        .map(|block| {
            anyhow::ensure!(
                block.labels.len() == 1 && !block.labels[0].is_empty(),
                "work.workspace_import must have one non-empty name label"
            );
            let value = WorkWorkspaceImport {
                name: block.labels[0].clone(),
                source_path: require_string(block, "source_path", None)?.to_owned(),
                target_path: require_string(block, "target_path", None)?.to_owned(),
            };
            validate_workspace_import(&value)?;
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        schema == "a3s-bench/task/v2" || imports.is_empty(),
        "Task v1 does not support work.workspace_import; use a3s-bench/task/v2"
    );
    let mut names = BTreeSet::new();
    for import in &imports {
        anyhow::ensure!(
            names.insert(import.name.as_str()),
            "duplicate work.workspace_import name {:?}",
            import.name
        );
    }
    for (index, import) in imports.iter().enumerate() {
        for other in &imports[index + 1..] {
            anyhow::ensure!(
                !relative_paths_overlap(&import.target_path, &other.target_path),
                "work.workspace_import targets {:?} and {:?} overlap",
                import.target_path,
                other.target_path
            );
        }
    }
    Ok(imports)
}

fn validate_workspace_import(value: &WorkWorkspaceImport) -> Result<()> {
    validate_workspace_import_source(&value.source_path)?;
    validate_workspace_import_target(&value.target_path)?;
    anyhow::ensure!(
        !value.name.contains(',')
            && !value.name.contains('\0')
            && !value.name.chars().any(char::is_control),
        "work.workspace_import name contains an unsafe character"
    );
    Ok(())
}

fn validate_workspace_import_source(path: &str) -> Result<()> {
    anyhow::ensure!(
        path.starts_with('/')
            && path != "/"
            && !path.contains(',')
            && !path.contains('\0')
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .skip(1)
                .all(|component| !component.is_empty() && !matches!(component, "." | "..")),
        "work.workspace_import.source_path must be a clean absolute directory path"
    );
    for pseudo in ["/proc", "/sys", "/dev", "/workspace", "/agent"] {
        anyhow::ensure!(
            path != pseudo && !path.starts_with(&format!("{pseudo}/")),
            "work.workspace_import.source_path must not select a runtime mount"
        );
    }
    Ok(())
}

fn validate_workspace_import_target(path: &str) -> Result<()> {
    let absolute_home_path = path.starts_with("/home/");
    anyhow::ensure!(
        !path.is_empty()
            && (!path.starts_with('/') || absolute_home_path)
            && !path.contains(',')
            && !path.contains('\0')
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .skip(usize::from(path.starts_with('/')))
                .all(|component| !component.is_empty() && !matches!(component, "." | "..")),
        "work.workspace_import.target_path must be a clean relative path or absolute /home/... path"
    );
    anyhow::ensure!(
        !path.split('/').any(|component| component == ".codex"),
        "work.workspace_import.target_path must not select .codex"
    );
    Ok(())
}

fn relative_paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

fn required_positive_integer(block: &Block, name: &str) -> Result<u64> {
    optional_positive_integer(block, name)?
        .ok_or_else(|| anyhow::anyhow!("{}.{} is required by a3s-bench/task/v2", block.name, name))
}

fn optional_positive_integer(block: &Block, name: &str) -> Result<Option<u64>> {
    let Some(value) = block.attributes.get(name) else {
        return Ok(None);
    };
    let number = value
        .as_number()
        .ok_or_else(|| anyhow::anyhow!("{}.{} must be a positive integer", block.name, name))?;
    anyhow::ensure!(
        number.is_finite() && number >= 1.0 && number <= u64::MAX as f64 && number.fract() == 0.0,
        "{}.{} must be a positive integer",
        block.name,
        name
    );
    Ok(Some(number as u64))
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
            "work" => (
                &[
                    "network_need",
                    "https_allow_hosts",
                    "cpu_limit",
                    "memory_bytes",
                ],
                &["image", "workspace_import"],
                Labels::None,
            ),
            "submission" => (&["include", "exclude"], &[], Labels::None),
            "judge" => (
                &["asset", "solution_timeout_sec", "cpu_limit", "memory_bytes"],
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
                "workspace_import" => &["source_path", "target_path"],
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
                    labels: if child.name == "workspace_import" {
                        Labels::Exactly(1)
                    } else {
                        Labels::None
                    },
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
        assert_eq!(task.candidate_timeout_sec, 300);
        assert_eq!(task.metrics[0].name, "correctness");
    }

    #[test]
    fn loads_builtin_workspace_seed() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks/ad_placement_optimization");
        let task = load_local(&root).unwrap();
        assert_eq!(task.candidate_timeout_sec, 43200);
        let seed = task.workspace_seed.unwrap();
        assert!(seed.image.starts_with("docker.io/seededge/"));
        assert_eq!(seed.source_path, "/home/workspace/ad-placement");
    }

    #[test]
    fn task_v1_uses_frozen_legacy_resource_defaults() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/smoke");
        let task = load_local(&root).unwrap();
        assert_eq!(task.resources, TaskResources::default());
    }

    #[test]
    fn task_v2_requires_and_parses_explicit_role_resources() {
        let document = a3s_acl::parse(
            r#"bench "test" {
              schema = "a3s-bench/task/v2"
              version = "0.1.0"
              work { cpu_limit = 6 memory_bytes = 17179869184 image { ref = "alpine" } }
              judge { asset = "judge" cpu_limit = 3 memory_bytes = 8589934592 }
            }"#,
        )
        .unwrap();
        let root = &document.blocks[0];
        validate_task_schema(root).unwrap();
        let work = unique_block(root, "work").unwrap();
        let judge = unique_block(root, "judge").unwrap();
        assert_eq!(
            parse_role_resources(work, "a3s-bench/task/v2", LEGACY_WORK_RESOURCES).unwrap(),
            RoleResources {
                cpu_limit: 6,
                memory_bytes: 17_179_869_184,
            }
        );
        assert_eq!(
            parse_role_resources(judge, "a3s-bench/task/v2", LEGACY_JUDGE_RESOURCES).unwrap(),
            RoleResources {
                cpu_limit: 3,
                memory_bytes: 8_589_934_592,
            }
        );

        let missing = a3s_acl::parse("work { cpu_limit = 4 }").unwrap();
        assert!(parse_role_resources(
            &missing.blocks[0],
            "a3s-bench/task/v2",
            LEGACY_WORK_RESOURCES
        )
        .is_err());
    }

    #[test]
    fn task_v2_resource_limits_accept_official_maximum_and_reject_unsafe_values() {
        let parse = |cpu_limit: u64, memory_bytes: u64| {
            let source =
                format!("work {{ cpu_limit = {cpu_limit} memory_bytes = {memory_bytes} }}");
            let document = a3s_acl::parse(&source).unwrap();
            parse_role_resources(
                &document.blocks[0],
                "a3s-bench/task/v2",
                LEGACY_WORK_RESOURCES,
            )
        };

        assert_eq!(
            parse(16, 16 * 1024 * 1024 * 1024).unwrap(),
            RoleResources {
                cpu_limit: 16,
                memory_bytes: 16 * 1024 * 1024 * 1024,
            }
        );
        assert!(parse(0, 1024).is_err());
        assert!(parse(1, 0).is_err());
        assert!(parse(MAX_CPU_LIMIT + 1, 1024).is_err());
        assert!(parse(1, MAX_MEMORY_BYTES + 1).is_err());
        assert!(parse(MAX_CPU_LIMIT, MAX_MEMORY_BYTES).is_ok());
    }

    fn parse_network_hosts(source: &str, schema: &str, need: &str) -> Result<Vec<String>> {
        let document = a3s_acl::parse(source).unwrap();
        parse_work_network_allow_hosts(&document.blocks[0], schema, need)
    }

    #[test]
    fn task_v2_normalizes_exact_https_allow_hosts() {
        let hosts = parse_network_hosts(
            r#"work {
              https_allow_hosts = ["pypi.org", "files.pythonhosted.org", "pypi.org"]
            }"#,
            "a3s-bench/task/v2",
            "restricted_https",
        )
        .unwrap();
        assert_eq!(hosts, ["files.pythonhosted.org", "pypi.org"]);

        assert!(parse_network_hosts(
            r#"work { https_allow_hosts = ["pypi.org"] }"#,
            "a3s-bench/task/v1",
            "restricted_https",
        )
        .is_err());
        assert!(parse_network_hosts("work {}", "a3s-bench/task/v2", "restricted_https").is_err());
        assert!(parse_network_hosts(
            r#"work { https_allow_hosts = ["pypi.org"] }"#,
            "a3s-bench/task/v2",
            "none",
        )
        .is_err());
    }

    #[test]
    fn task_v2_rejects_noncanonical_or_non_dns_allow_hosts() {
        for host in [
            "PyPI.org",
            "pypi.org.",
            "https://pypi.org",
            "pypi.org:443",
            "*.pypi.org",
            "127.0.0.1",
            "::1",
            "-bad.example",
            "bad-.example",
            "bad..example",
            "é.example",
        ] {
            let source = format!("work {{ https_allow_hosts = [{host:?}] }}");
            assert!(
                parse_network_hosts(&source, "a3s-bench/task/v2", "restricted_https").is_err(),
                "accepted unsafe host {host:?}"
            );
        }
    }

    fn parse_workspace_imports(source: &str, schema: &str) -> Result<Vec<WorkWorkspaceImport>> {
        let document = a3s_acl::parse(source).unwrap();
        let work = &document.blocks[0];
        parse_work_workspace_imports(work, schema)
    }

    #[test]
    fn task_v2_parses_safe_work_workspace_imports() {
        let imports = parse_workspace_imports(
            r#"work {
              workspace_import "maven_repository" {
                source_path = "/root/.m2/repository"
                target_path = "/home/agent/.m2/repository"
              }
            }"#,
            "a3s-bench/task/v2",
        )
        .unwrap();
        assert_eq!(
            imports,
            vec![WorkWorkspaceImport {
                name: "maven_repository".into(),
                source_path: "/root/.m2/repository".into(),
                target_path: "/home/agent/.m2/repository".into(),
            }]
        );

        let relative = parse_workspace_imports(
            r#"work {
              workspace_import "relative_cache" {
                source_path = "/root/cache"
                target_path = ".cache/tool"
              }
            }"#,
            "a3s-bench/task/v2",
        )
        .unwrap();
        assert_eq!(relative[0].target_path, ".cache/tool");
    }

    #[test]
    fn work_workspace_imports_reject_unsafe_and_ambiguous_paths() {
        for (source_path, target_path) in [
            ("/", ".m2/repository"),
            ("relative", ".m2/repository"),
            ("/root/../secret", ".m2/repository"),
            ("/root//secret", ".m2/repository"),
            ("/proc/self", ".m2/repository"),
            ("/sys/kernel", ".m2/repository"),
            ("/dev/shm", ".m2/repository"),
            ("/workspace/secret", ".m2/repository"),
            ("/agent/secret", ".m2/repository"),
            ("/root/cache,other", ".m2/repository"),
            ("/root/cache", ""),
            ("/root/cache", "/absolute"),
            ("/root/cache", "/etc/cache"),
            ("/root/cache", "/proc/cache"),
            ("/root/cache", "/home"),
            ("/root/cache", "/home//agent/cache"),
            ("/root/cache", "/home/agent/../root"),
            ("/root/cache", "/home/agent/.codex/cache"),
            ("/root/cache", "cache/../escape"),
            ("/root/cache", "cache//nested"),
            ("/root/cache", ".codex"),
            ("/root/cache", "safe/.codex/cache"),
            ("/root/cache", "cache,other"),
        ] {
            let source = format!(
                "work {{ workspace_import \"cache\" {{ source_path = {source_path:?} target_path = {target_path:?} }} }}"
            );
            assert!(
                parse_workspace_imports(&source, "a3s-bench/task/v2").is_err(),
                "accepted source={source_path:?}, target={target_path:?}"
            );
        }
    }

    #[test]
    fn work_workspace_imports_reject_v1_duplicates_and_overlaps() {
        let single = r#"work {
          workspace_import "cache" { source_path = "/root/cache" target_path = "cache" }
        }"#;
        assert!(parse_workspace_imports(single, "a3s-bench/task/v1").is_err());

        for source in [
            r#"work {
              workspace_import "cache" { source_path = "/root/a" target_path = "a" }
              workspace_import "cache" { source_path = "/root/b" target_path = "b" }
            }"#,
            r#"work {
              workspace_import "a" { source_path = "/root/a" target_path = ".m2" }
              workspace_import "b" { source_path = "/root/b" target_path = ".m2/repository" }
            }"#,
        ] {
            assert!(parse_workspace_imports(source, "a3s-bench/task/v2").is_err());
        }
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
