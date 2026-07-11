use a3s_acl::{Block, Document, Value};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LocalAssetPackage {
    pub root: PathBuf,
    pub entrypoint: String,
    pub definition_path: Option<String>,
    pub identity: String,
}

pub fn load_local(reference: &Path) -> Result<LocalAssetPackage> {
    let metadata = std::fs::symlink_metadata(reference)
        .with_context(|| format!("Asset package does not exist: {}", reference.display()))?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "local Asset package must be a real directory"
    );
    load_directory(reference, tree_identity(reference)?)
}

pub fn resolve(reference: &str, state_root: &Path) -> Result<LocalAssetPackage> {
    if reference == "a3s-code" {
        return load_local(&crate::catalog::builtin_root().join("candidates/a3s-code"));
    }
    if reference.starts_with("./") || reference.starts_with("../") {
        return load_local(Path::new(reference));
    }
    let image = reference
        .strip_prefix("oci://")
        .ok_or_else(|| anyhow::anyhow!("unsupported Asset package reference {reference:?}"))?;
    crate::oci_asset::resolve(image, state_root)
}

pub(crate) fn load_directory(reference: &Path, identity: String) -> Result<LocalAssetPackage> {
    let manifest = reference.join(".a3s/asset.acl");
    let source = std::fs::read_to_string(&manifest)
        .with_context(|| format!("could not read {}", manifest.display()))?;
    let document = a3s_acl::parse(&source)
        .map_err(|error| anyhow::anyhow!("invalid {}: {error}", manifest.display()))?;
    validate_asset_schema(&document)?;
    require_top(&document, "version", "a3s.asset.v1")?;
    require_top(&document, "category", "agent")?;
    let source = unique_top_block(&document, "source")?;
    let entrypoint = source
        .attributes
        .get("entrypoint")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("source.entrypoint must be a string"))?;
    let file = entrypoint.split(':').next().unwrap_or(entrypoint);
    validate_package_path(file, "source.entrypoint")?;
    anyhow::ensure!(
        reference.join(file).is_file(),
        "Asset package entrypoint is missing: {file}"
    );
    let definition_path = source
        .attributes
        .get("definition_path")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("source.definition_path must be a string"))
        })
        .transpose()?;
    if let Some(path) = definition_path {
        validate_package_path(path, "source.definition_path")?;
        anyhow::ensure!(
            reference.join(path).is_file(),
            "Asset package definition is missing: {path}"
        );
    }
    Ok(LocalAssetPackage {
        root: reference.canonicalize()?,
        entrypoint: entrypoint.to_owned(),
        definition_path: definition_path.map(str::to_owned),
        identity,
    })
}

impl LocalAssetPackage {
    pub fn model_instructions_path(&self) -> Result<PathBuf> {
        let relative = self.definition_path.as_deref().ok_or_else(|| {
            anyhow::anyhow!("model-backed Candidate adapter must define source.definition_path")
        })?;
        Ok(self.root.join(relative))
    }

    pub fn model_max_steps(&self) -> Result<usize> {
        let definition = std::fs::read_to_string(self.model_instructions_path()?)?;
        let mut lines = definition.lines();
        anyhow::ensure!(
            lines.next().map(str::trim) == Some("---"),
            "model Candidate definition must start with frontmatter"
        );
        let mut value = None;
        let mut closed = false;
        for line in lines {
            if line.trim() == "---" {
                closed = true;
                break;
            }
            if let Some(raw) = line.trim().strip_prefix("max_steps:") {
                anyhow::ensure!(value.is_none(), "model Candidate max_steps is duplicated");
                value = Some(raw.trim().parse::<usize>()?);
            }
        }
        anyhow::ensure!(closed, "model Candidate frontmatter is not closed");
        let value = value
            .ok_or_else(|| anyhow::anyhow!("model Candidate definition must declare max_steps"))?;
        anyhow::ensure!(
            (1..=1000).contains(&value),
            "model Candidate max_steps must be between 1 and 1000"
        );
        Ok(value)
    }
}

fn validate_package_path(path: &str, field: &str) -> Result<()> {
    use std::path::Component;

    anyhow::ensure!(!path.is_empty(), "{field} must not be empty");
    anyhow::ensure!(
        Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "{field} must be a normalized package-relative path"
    );
    Ok(())
}

pub(crate) fn load_manifest_entrypoint(manifest: &Path) -> Result<String> {
    let source = std::fs::read_to_string(manifest)?;
    let document = a3s_acl::parse(&source)
        .map_err(|error| anyhow::anyhow!("invalid {}: {error}", manifest.display()))?;
    validate_asset_schema(&document)?;
    let block = unique_top_block(&document, "source")?;
    block
        .attributes
        .get("entrypoint")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("source.entrypoint must be a string"))
}

fn validate_asset_schema(document: &Document) -> Result<()> {
    use crate::acl_schema::{validate_block, validate_scalar_block, BlockSchema, Labels};

    const SCALARS: &[&str] = &[
        "version",
        "category",
        "kind",
        "name",
        "description",
        "service",
        "created_by",
    ];
    const STRUCTURED: &[&str] = &["source", "metadata", "runtime", "capability"];
    for block in &document.blocks {
        anyhow::ensure!(
            SCALARS.contains(&block.name.as_str()) || STRUCTURED.contains(&block.name.as_str()),
            "Asset package contains unknown top-level field or block {:?}",
            block.name
        );
        if SCALARS.contains(&block.name.as_str()) {
            validate_scalar_block(block)?;
            continue;
        }
        let (attributes, labels): (&[&str], Labels) = match block.name.as_str() {
            "source" => (
                &["package_path", "entrypoint", "definition_path"],
                Labels::None,
            ),
            "metadata" => (&["asset_acl_path"], Labels::None),
            "runtime" => (
                &[
                    "kind",
                    "isolation",
                    "runtime_kind",
                    "protocol",
                    "agent_kind",
                ],
                Labels::None,
            ),
            "capability" => (
                &["input_schema", "output_schema", "network", "model_gateway"],
                Labels::Exactly(1),
            ),
            _ => unreachable!("top-level names were validated"),
        };
        validate_block(
            block,
            &format!("Asset package {}", block.name),
            BlockSchema {
                attributes,
                children: &[],
                labels,
            },
        )?;
    }
    for name in SCALARS
        .iter()
        .chain(["source", "metadata", "runtime"].iter())
    {
        anyhow::ensure!(
            document
                .blocks
                .iter()
                .filter(|block| block.name == *name)
                .count()
                <= 1,
            "Asset package contains duplicate {name}"
        );
    }
    Ok(())
}

fn tree_identity(root: &Path) -> Result<String> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            anyhow::ensure!(
                !kind.is_symlink(),
                "Asset package must not contain symlinks"
            );
            if kind.is_dir() {
                visit(root, &entry.path(), files)?;
            } else if kind.is_file() {
                files.push(entry.path().strip_prefix(root)?.to_path_buf());
            } else {
                anyhow::bail!("Asset package contains a special file");
            }
        }
        Ok(())
    }
    let root = root.canonicalize()?;
    let mut files = Vec::new();
    visit(&root, &root, &mut files)?;
    files.sort();
    let mut digest = Sha256::new();
    for relative in files {
        digest.update(relative.to_string_lossy().as_bytes());
        digest.update([0]);
        digest.update([u8::from(crate::state_fs::is_executable(
            &root.join(&relative),
        )?)]);
        digest.update([0]);
        digest.update(std::fs::read(root.join(relative))?);
        digest.update([0]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn require_top(document: &Document, key: &str, expected: &str) -> Result<()> {
    let matches: Vec<_> = document
        .blocks
        .iter()
        .filter(|block| block.name == key)
        .collect();
    anyhow::ensure!(
        matches.len() == 1,
        "Asset package must define {key} exactly once"
    );
    let actual = matches[0].attributes.get(key).and_then(Value::as_str);
    anyhow::ensure!(
        actual == Some(expected),
        "Asset package {key} must be {expected:?}"
    );
    Ok(())
}

fn unique_top_block<'a>(document: &'a Document, name: &str) -> Result<&'a Block> {
    let matches: Vec<_> = document
        .blocks
        .iter()
        .filter(|child| child.name == name)
        .collect();
    anyhow::ensure!(
        matches.len() == 1,
        "Asset package must contain exactly one {name} block"
    );
    Ok(matches[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_smoke_candidate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/smoke-candidate");
        let asset = load_local(&root).unwrap();
        assert_eq!(asset.entrypoint, "run.sh");
        assert_eq!(asset.definition_path.as_deref(), Some("agent.md"));
        assert_eq!(asset.model_max_steps().unwrap(), 1);
        assert!(asset.identity.starts_with("sha256:"));
    }

    #[test]
    fn resolves_bundled_a3s_code_candidate() {
        let state = tempfile::tempdir().unwrap();
        let asset = resolve("a3s-code", state.path()).unwrap();
        assert_eq!(
            asset.definition_path.as_deref(),
            Some("prompts/controller.md")
        );
        assert_eq!(asset.model_max_steps().unwrap(), 256);
        assert!(asset.identity.starts_with("sha256:"));
    }

    #[test]
    fn rejects_paths_that_escape_the_asset() {
        for path in ["../run.sh", "/run.sh", "nested/../run.sh", ""] {
            assert!(validate_package_path(path, "source.entrypoint").is_err());
        }
        assert!(validate_package_path("nested/run.sh", "source.entrypoint").is_ok());
    }

    #[test]
    fn rejects_unknown_asset_fields_and_source_attributes() {
        for source in [
            "version = \"a3s.asset.v1\"\nunknown = true",
            "version = \"a3s.asset.v1\"\nsource { entrypoint = \"run.sh\" typo = true }",
        ] {
            let document = a3s_acl::parse(source).unwrap();
            assert!(validate_asset_schema(&document).is_err());
        }
    }
}
