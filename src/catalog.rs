use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const CATALOG_JSON: &str = include_str!("../builtin/catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: String,
    pub tasks: Vec<CatalogTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogTask {
    pub id: String,
    pub path: String,
    pub name: String,
    pub category: String,
    pub execution_class: String,
    pub availability: String,
    pub availability_reason: String,
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

pub(crate) fn builtin_root() -> PathBuf {
    if let Ok(executable) = std::env::current_exe() {
        if let Some(component_root) = executable.parent().and_then(Path::parent) {
            let packaged = component_root.join("builtin");
            if packaged.is_dir() {
                return packaged;
            }
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin")
}

pub fn runnable_task_path(id: &str) -> Result<PathBuf> {
    let catalog = load()?;
    let entry = catalog
        .tasks
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| anyhow::anyhow!("unknown built-in Task {id:?}"))?;
    anyhow::ensure!(
        entry.availability == "ready",
        "built-in Task {id:?} is not locally runnable: {}",
        entry.availability_reason
    );
    Ok(builtin_root().join(&entry.path))
}

pub fn resolve_task_reference(reference: &str) -> Result<PathBuf> {
    if reference.starts_with("./") || reference.starts_with("../") {
        Ok(Path::new(reference).to_path_buf())
    } else {
        runnable_task_path(reference)
    }
}

fn validate(catalog: &Catalog, root: &Path) -> Result<()> {
    anyhow::ensure!(
        !catalog.tasks.is_empty(),
        "built-in catalog must not be empty"
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
            matches!(
                entry.execution_class.as_str(),
                "conformance" | "long_horizon"
            ),
            "built-in Task {:?} has invalid execution class",
            entry.id
        );
        anyhow::ensure!(
            matches!(entry.availability.as_str(), "ready" | "blocked"),
            "built-in Task {:?} has invalid availability",
            entry.id
        );
        anyhow::ensure!(
            !entry.availability_reason.trim().is_empty(),
            "built-in Task {:?} has no availability reason",
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
        if let Some(source) = task.legacy_judge.as_ref() {
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
    fn current_builtins_satisfy_the_catalog_contract() {
        let catalog = load().unwrap();
        assert!(!catalog.tasks.is_empty());
        assert!(catalog
            .tasks
            .iter()
            .all(|entry| builtin_root().join(&entry.path).is_dir()));
    }

    #[test]
    fn available_builtin_resolves_to_a_task_bundle() {
        let path = runnable_task_path("quick_file_edit").unwrap();
        assert!(path.join("task.acl").is_file());
    }

    #[test]
    fn local_availability_is_independent_from_official_admission() {
        let path = runnable_task_path("juliet_vulnerability_analyzer").unwrap();
        assert!(path.join("task.acl").is_file());
        let model_judge = runnable_task_path("college_english_exam_bank").unwrap();
        assert!(model_judge.join("task.acl").is_file());
    }
}
