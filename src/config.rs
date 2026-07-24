use a3s_acl::{Block, Document, Value};
use a3s_runtime::ProviderId;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::runtime_selection::RuntimeSelection;

#[derive(Debug, Clone)]
pub struct LocalConfig {
    pub path: Option<PathBuf>,
    pub runtime: RuntimeSelection,
    pub judge_model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelRoute {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
}

pub fn discover(start: &Path) -> Result<LocalConfig> {
    let path = discover_path(start);
    let Some(path) = path else {
        return Ok(LocalConfig {
            path: None,
            judge_model: None,
            runtime: RuntimeSelection::bench_default()?,
        });
    };
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let document = a3s_acl::parse(&source)
        .map_err(|error| anyhow::anyhow!("invalid {}: {error}", path.display()))?;
    Ok(LocalConfig {
        runtime: parse_runtime(&document)?,
        judge_model: parse_judge_model(&document)?,
        path: Some(path),
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
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("provider {provider_id:?} has no api_key"))?;
    let base_url = provider
        .attributes
        .get("base_url")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("provider {provider_id:?} has no base_url"))?;
    Ok(ModelRoute {
        model: model_id.to_owned(),
        api_key: api_key.to_owned(),
        base_url: base_url.to_owned(),
    })
}

fn parse_judge_model(document: &Document) -> Result<Option<String>> {
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
        return Ok(None);
    };
    anyhow::ensure!(block.labels.is_empty(), "bench block must not have labels");
    anyhow::ensure!(
        block.attributes.keys().all(|name| name == "judge_model") && block.blocks.is_empty(),
        "bench block supports only judge_model"
    );
    let model = block
        .attributes
        .get("judge_model")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("bench.judge_model must be a non-empty provider/model"))?;
    parse_model_reference(model)?;
    Ok(Some(model.to_owned()))
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
}
