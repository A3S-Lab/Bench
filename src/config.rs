use a3s_acl::{Block, Document, Value};
use a3s_runtime::ProviderId;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::runtime_selection::RuntimeSelection;

#[derive(Debug, Clone)]
pub struct LocalConfig {
    pub path: Option<PathBuf>,
    pub bench_path: Option<PathBuf>,
    pub runtime: RuntimeSelection,
    pub judge_model: Option<String>,
    pub codex_reasoning_effort: Option<String>,
    pub disable_auto_resume: bool,
    pub parallel_game_sessions: bool,
}

#[derive(Default)]
struct BenchConfig {
    judge_model: Option<String>,
}

#[derive(Debug)]
struct PrivateBenchConfig {
    codex_reasoning_effort: Option<String>,
    disable_auto_resume: bool,
    parallel_game_sessions: bool,
}

impl Default for PrivateBenchConfig {
    fn default() -> Self {
        Self {
            codex_reasoning_effort: None,
            disable_auto_resume: false,
            parallel_game_sessions: true,
        }
    }
}

pub struct ModelRoute {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
}

pub fn discover(start: &Path) -> Result<LocalConfig> {
    let path = discover_path(start);
    let (runtime, judge_model) = if let Some(config_path) = path.as_deref() {
        let source = std::fs::read_to_string(config_path)
            .with_context(|| format!("could not read {}", config_path.display()))?;
        let document = a3s_acl::parse(&source)
            .map_err(|error| anyhow::anyhow!("invalid {}: {error}", config_path.display()))?;
        let bench = parse_bench(&document)?;
        (parse_runtime(&document)?, bench.judge_model)
    } else {
        (RuntimeSelection::bench_default()?, None)
    };
    let bench_path = discover_private_bench_path(start);
    let private_bench = bench_path
        .as_deref()
        .map(|config_path| {
            let source = std::fs::read_to_string(config_path)
                .with_context(|| format!("could not read {}", config_path.display()))?;
            let document = a3s_acl::parse(&source)
                .map_err(|error| anyhow::anyhow!("invalid {}: {error}", config_path.display()))?;
            parse_private_bench(&document)
        })
        .transpose()?
        .unwrap_or_default();
    Ok(LocalConfig {
        runtime,
        judge_model,
        codex_reasoning_effort: private_bench.codex_reasoning_effort,
        disable_auto_resume: private_bench.disable_auto_resume,
        parallel_game_sessions: private_bench.parallel_game_sessions,
        path,
        bench_path,
    })
}

pub fn resolve_model_route(path: &Path, reference: &str) -> Result<ModelRoute> {
    let (provider_id, model_id) = parse_model_reference(reference)?;
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let document = a3s_acl::parse(&source)
        .map_err(|error| anyhow::anyhow!("invalid {}: {error}", path.display()))?;
    let providers: Vec<_> = document
        .blocks
        .iter()
        .filter(|block| {
            block.name == "providers"
                && block.labels.first().map(String::as_str) == Some(provider_id)
        })
        .collect();
    anyhow::ensure!(
        providers.len() == 1,
        "provider {provider_id:?} must be configured exactly once"
    );
    let provider = providers[0];
    anyhow::ensure!(
        provider.labels.len() == 1,
        "provider block must have one label"
    );
    let models = provider
        .blocks
        .iter()
        .filter(|block| {
            block.name == "models" && block.labels.first().map(String::as_str) == Some(model_id)
        })
        .count();
    anyhow::ensure!(
        models == 1,
        "model {reference:?} must be configured exactly once"
    );
    let api_key = provider
        .attributes
        .get("api_key")
        .or(provider.attributes.get("apiKey"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("provider {provider_id:?} has no api_key"))?;
    let base_url = provider
        .attributes
        .get("base_url")
        .or(provider.attributes.get("baseUrl"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("provider {provider_id:?} has no base_url"))?;
    Ok(ModelRoute {
        model: model_id.to_owned(),
        api_key: api_key.to_owned(),
        base_url: base_url.to_owned(),
    })
}

fn parse_bench(document: &Document) -> Result<BenchConfig> {
    let blocks: Vec<_> = document
        .blocks
        .iter()
        .filter(|block| block.name == "bench")
        .collect();
    anyhow::ensure!(
        blocks.len() <= 1,
        "config.acl contains duplicate bench blocks"
    );
    let Some(block) = blocks.first() else {
        return Ok(BenchConfig::default());
    };
    anyhow::ensure!(block.labels.is_empty(), "bench block must not have labels");
    anyhow::ensure!(
        block.attributes.keys().all(|name| name == "judge_model") && block.blocks.is_empty(),
        "shared .a3s/config.acl bench block supports only judge_model; move codex_reasoning_effort to .a3s/bench/config.acl"
    );
    let judge_model = block
        .attributes
        .get("judge_model")
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("bench.judge_model must be a non-empty provider/model")
                })
        })
        .transpose()?;
    if let Some(model) = judge_model {
        parse_model_reference(model)?;
    }
    Ok(BenchConfig {
        judge_model: judge_model.map(str::to_owned),
    })
}

fn parse_private_bench(document: &Document) -> Result<PrivateBenchConfig> {
    let Some(block) = document.blocks.first() else {
        return Ok(PrivateBenchConfig::default());
    };
    anyhow::ensure!(
        document.blocks.len() == 1
            && block.name == "bench"
            && block.labels.is_empty()
            && block.blocks.is_empty()
            && block.attributes.keys().all(|name| matches!(
                name.as_str(),
                "codex_reasoning_effort" | "disable_auto_resume" | "parallel_game_sessions"
            )),
        "private bench config supports only codex_reasoning_effort, disable_auto_resume, or parallel_game_sessions"
    );
    let codex_reasoning_effort = block
        .attributes
        .get("codex_reasoning_effort")
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "bench.codex_reasoning_effort must be a non-empty reasoning effort"
                    )
                })
        })
        .transpose()?;
    if let Some(effort) = codex_reasoning_effort {
        crate::lock::validate_reasoning_effort(effort)?;
    }
    let disable_auto_resume = block
        .attributes
        .get("disable_auto_resume")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("bench.disable_auto_resume must be a boolean"))
        })
        .transpose()?
        .unwrap_or(false);
    let parallel_game_sessions = block
        .attributes
        .get("parallel_game_sessions")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("bench.parallel_game_sessions must be a boolean"))
        })
        .transpose()?
        .unwrap_or(true);
    Ok(PrivateBenchConfig {
        codex_reasoning_effort: codex_reasoning_effort.map(str::to_owned),
        disable_auto_resume,
        parallel_game_sessions,
    })
}

fn parse_model_reference(value: &str) -> Result<(&str, &str)> {
    let (provider, model) = value
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("model route {value:?} must use provider/model"))?;
    anyhow::ensure!(
        !provider.is_empty() && !model.is_empty() && !model.contains('/'),
        "model route {value:?} must use provider/model"
    );
    Ok((provider, model))
}

pub fn validate_model_reference(value: &str) -> Result<()> {
    parse_model_reference(value).map(|_| ())
}

fn discover_path(start: &Path) -> Option<PathBuf> {
    for directory in start.ancestors() {
        let candidate = directory.join(".a3s/config.acl");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".a3s/config.acl"))
        .filter(|path| path.is_file())
}

fn discover_private_bench_path(start: &Path) -> Option<PathBuf> {
    for directory in start.ancestors() {
        let candidate = directory.join(".a3s/bench/config.acl");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".a3s/bench/config.acl"))
        .filter(|path| path.is_file())
}

fn parse_runtime(document: &Document) -> Result<RuntimeSelection> {
    let blocks: Vec<&Block> = document
        .blocks
        .iter()
        .filter(|block| block.name == "runtime")
        .collect();
    anyhow::ensure!(
        blocks.len() <= 1,
        "config.acl contains duplicate runtime blocks"
    );
    let Some(block) = blocks.first() else {
        return Ok(RuntimeSelection::bench_default()?);
    };
    anyhow::ensure!(
        block.labels.is_empty(),
        "runtime block must not have labels"
    );
    let provider = block
        .attributes
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("runtime.provider must be a non-empty string"))?;
    anyhow::ensure!(
        !provider.trim().is_empty(),
        "runtime.provider must not be empty"
    );
    let provider = ProviderId::parse(provider.to_owned()).map_err(anyhow::Error::from)?;
    Ok(RuntimeSelection::operator(provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_runtime_block_defaults_to_docker() {
        let document = a3s_acl::parse("default_model = \"openai/test\"").unwrap();
        assert_eq!(
            parse_runtime(&document).unwrap().provider.as_str(),
            "docker"
        );
    }

    #[test]
    fn configured_provider_wins() {
        let document = a3s_acl::parse("runtime { provider = \"a3s-box\" }").unwrap();
        let selected = parse_runtime(&document).unwrap();
        assert_eq!(selected.provider.as_str(), "a3s-box");
        assert_eq!(
            selected.source,
            crate::runtime_selection::RuntimeSelectionSource::OperatorConfig
        );
    }

    #[test]
    fn os_runtime_is_a_valid_explicit_provider() {
        let document = a3s_acl::parse("runtime { provider = \"os-runtime\" }").unwrap();
        let selected = parse_runtime(&document).unwrap();
        assert_eq!(selected.provider.as_str(), "os-runtime");
        assert_eq!(
            selected.source,
            crate::runtime_selection::RuntimeSelectionSource::OperatorConfig
        );
    }

    #[test]
    fn resolves_local_judge_model_route() {
        let directory = tempfile::tempdir().unwrap();
        let config_directory = directory.path().join(".a3s");
        std::fs::create_dir(&config_directory).unwrap();
        let path = config_directory.join("config.acl");
        std::fs::write(
            &path,
            "bench {\n  judge_model = \"custom/grader\"\n}\nproviders \"custom\" {\n  api_key = \"secret\"\n  base_url = \"https://example.test/v1\"\n  models \"grader\" {\n    name = \"Grader\"\n  }\n}\n",
        )
        .unwrap();
        let discovered = discover(directory.path()).unwrap();
        assert_eq!(discovered.judge_model.as_deref(), Some("custom/grader"));
        let route = resolve_model_route(&path, "custom/grader").unwrap();
        assert_eq!(route.model, "grader");
        assert_eq!(route.base_url, "https://example.test/v1");
        assert_eq!(route.api_key, "secret");
    }

    #[test]
    fn discovers_codex_reasoning_effort_without_judge_model() {
        let directory = tempfile::tempdir().unwrap();
        let config_directory = directory.path().join(".a3s/bench");
        std::fs::create_dir_all(&config_directory).unwrap();
        std::fs::write(
            directory.path().join(".a3s/config.acl"),
            "default_model = \"openai/test\"",
        )
        .unwrap();
        std::fs::write(
            config_directory.join("config.acl"),
            "bench { codex_reasoning_effort = \"none\" }",
        )
        .unwrap();

        let discovered = discover(directory.path()).unwrap();
        assert_eq!(discovered.judge_model, None);
        assert_eq!(discovered.codex_reasoning_effort.as_deref(), Some("none"));
        assert!(!discovered.disable_auto_resume);
        assert!(discovered.parallel_game_sessions);
        assert_eq!(
            discovered.bench_path.as_deref(),
            Some(config_directory.join("config.acl").as_path())
        );
    }

    #[test]
    fn discovers_disable_auto_resume() {
        let directory = tempfile::tempdir().unwrap();
        let config_directory = directory.path().join(".a3s/bench");
        std::fs::create_dir_all(&config_directory).unwrap();
        std::fs::write(
            config_directory.join("config.acl"),
            "bench { disable_auto_resume = true }",
        )
        .unwrap();
        let discovered = discover(directory.path()).unwrap();
        assert!(discovered.disable_auto_resume);
        assert!(discovered.parallel_game_sessions);
    }

    #[test]
    fn discovers_disabled_parallel_game_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let config_directory = directory.path().join(".a3s/bench");
        std::fs::create_dir_all(&config_directory).unwrap();
        std::fs::write(
            config_directory.join("config.acl"),
            "bench { parallel_game_sessions = false }",
        )
        .unwrap();
        let discovered = discover(directory.path()).unwrap();
        assert!(!discovered.parallel_game_sessions);
    }

    #[test]
    fn rejects_non_boolean_parallel_game_sessions() {
        let document = a3s_acl::parse(r#"bench { parallel_game_sessions = "false" }"#).unwrap();
        let error = parse_private_bench(&document).unwrap_err();
        assert!(error
            .to_string()
            .contains("bench.parallel_game_sessions must be a boolean"));
    }

    #[test]
    fn rejects_invalid_codex_reasoning_effort() {
        let document =
            a3s_acl::parse("bench { codex_reasoning_effort = \"invalid-reasoning-effort\" }")
                .unwrap();
        let error = parse_private_bench(&document).err().unwrap();
        assert!(error
            .to_string()
            .contains("reasoning effort must be one of"));
    }

    #[test]
    fn rejects_unknown_bench_attributes() {
        let document = a3s_acl::parse("bench { reasoning_effort = \"none\" }").unwrap();
        let error = parse_bench(&document).err().unwrap();
        assert!(error.to_string().contains("supports only judge_model"));
    }

    #[test]
    fn shared_config_points_legacy_reasoning_to_private_path() {
        let document = a3s_acl::parse("bench { codex_reasoning_effort = \"none\" }").unwrap();
        let error = parse_bench(&document).err().unwrap();
        assert!(error.to_string().contains(".a3s/bench/config.acl"));
    }

    #[test]
    fn rejects_unknown_private_bench_attributes() {
        let document =
            a3s_acl::parse("bench { codex_reasoning_effort = \"none\" unknown = \"value\" }")
                .unwrap();
        let error = parse_private_bench(&document).unwrap_err();
        assert!(error
            .to_string()
            .contains("supports only codex_reasoning_effort"));
    }
}
