use a3s_acl::{Block, Document, Value};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSelection {
    DockerDefault,
    Configured { provider: String },
}

#[derive(Debug, Clone)]
pub struct LocalConfig {
    pub path: Option<PathBuf>,
    pub runtime: RuntimeSelection,
}

pub fn discover(start: &Path) -> Result<LocalConfig> {
    let path = discover_path(start);
    let Some(path) = path else {
        return Ok(LocalConfig {
            path: None,
            runtime: RuntimeSelection::DockerDefault,
        });
    };
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let document = a3s_acl::parse(&source)
        .map_err(|error| anyhow::anyhow!("invalid {}: {error}", path.display()))?;
    Ok(LocalConfig {
        runtime: parse_runtime(&document)?,
        path: Some(path),
    })
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
        return Ok(RuntimeSelection::DockerDefault);
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
    Ok(RuntimeSelection::Configured {
        provider: provider.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_runtime_block_defaults_to_docker() {
        let document = a3s_acl::parse("default_model = \"openai/test\"").unwrap();
        assert_eq!(
            parse_runtime(&document).unwrap(),
            RuntimeSelection::DockerDefault
        );
    }

    #[test]
    fn configured_provider_wins() {
        let document = a3s_acl::parse("runtime { provider = \"a3s-box\" }").unwrap();
        assert_eq!(
            parse_runtime(&document).unwrap(),
            RuntimeSelection::Configured {
                provider: "a3s-box".to_owned()
            }
        );
    }
}
