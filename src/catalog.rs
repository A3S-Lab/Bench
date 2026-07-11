use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const CATALOG_JSON: &str = include_str!("../builtin/catalog.json");
const BUILTIN_TASK_COUNT: usize = 51;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub schema: String,
    pub tasks: Vec<CatalogTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogTask {
    pub id: String,
    pub path: String,
    pub name: String,
    pub category: String,
    pub admission: String,
    pub admission_reason: String,
    pub provenance_ref: String,
}

pub fn load() -> Result<Catalog> {
    let catalog: Catalog =
        serde_json::from_str(CATALOG_JSON).context("invalid built-in catalog")?;
    anyhow::ensure!(
        catalog.schema == "a3s-bench/builtin-catalog/v1",
        "unsupported built-in catalog schema {}",
        catalog.schema
    );
    validate(&catalog, &builtin_root())?;
    Ok(catalog)
}

fn builtin_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin")
}

fn validate(catalog: &Catalog, root: &Path) -> Result<()> {
    anyhow::ensure!(
        catalog.tasks.len() == BUILTIN_TASK_COUNT,
        "built-in catalog must contain exactly {BUILTIN_TASK_COUNT} Tasks"
    );
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for entry in &catalog.tasks {
        anyhow::ensure!(
            ids.insert(&entry.id),
            "duplicate built-in Task ID {:?}",
            entry.id
        );
        anyhow::ensure!(
            paths.insert(&entry.path),
            "duplicate built-in Task path {:?}",
            entry.path
        );
        anyhow::ensure!(
            entry.path == format!("tasks/{}", entry.id),
            "built-in Task {:?} has a non-canonical path",
            entry.id
        );
        anyhow::ensure!(
            matches!(entry.admission.as_str(), "admitted" | "quarantined"),
            "built-in Task {:?} has invalid admission status",
            entry.id
        );
        anyhow::ensure!(
            !entry.admission_reason.trim().is_empty(),
            "built-in Task {:?} has no admission reason",
            entry.id
        );
        let task = crate::task::load_local(&root.join(&entry.path))
            .with_context(|| format!("built-in Task {:?} is invalid", entry.id))?;
        anyhow::ensure!(
            task.id == entry.id,
            "catalog ID does not match Task descriptor"
        );
        anyhow::ensure!(
            task.name == entry.name,
            "catalog name does not match Task descriptor"
        );
        anyhow::ensure!(
            task.category == entry.category,
            "catalog category does not match Task descriptor"
        );
        let source = task.legacy_judge.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "built-in Task {:?} has no executable Judge source",
                entry.id
            )
        })?;
        anyhow::ensure!(
            matches!(source.mode.as_str(), "batch" | "game_server"),
            "built-in Task {:?} uses unsupported Judge mode {:?}",
            entry.id,
            source.mode
        );
        anyhow::ensure!(
            source.platform.as_deref() == Some("linux/amd64"),
            "built-in Task {:?} must bind its Judge platform",
            entry.id
        );
    }
    let task_directories = std::fs::read_dir(root.join("tasks"))?
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir())
                .map(|_| entry)
        })
        .count();
    anyhow::ensure!(
        task_directories == catalog.tasks.len(),
        "built-in task directory count does not match catalog"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_51_builtins_satisfy_the_catalog_contract() {
        let catalog = load().unwrap();
        assert_eq!(catalog.tasks.len(), BUILTIN_TASK_COUNT);
        assert!(catalog
            .tasks
            .iter()
            .all(|entry| builtin_root().join(&entry.path).is_dir()));
    }
}
