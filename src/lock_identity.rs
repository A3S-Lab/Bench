use crate::lock::{CandidateLock, TaskLock};
use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct LegacyTaskLockIdentity<'a> {
    schema: &'a str,
    task_revision: &'a str,
    artifact_digest: &'a str,
    judge_revision: &'a str,
    judge_artifact_digest: &'a str,
    judge_model: &'a Option<String>,
    resolved_images: &'a std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct TaskLockIdentity<'a> {
    schema: &'a str,
    task_revision: &'a str,
    artifact_digest: &'a str,
    judge_revision: &'a str,
    judge_artifact_digest: &'a str,
    judge_model: &'a Option<String>,
    resolved_images: &'a std::collections::BTreeMap<String, String>,
    resources: &'a Option<crate::task::TaskResources>,
    workspace_imports: &'a Option<Vec<crate::task::WorkWorkspaceImport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_allow_hosts: &'a Option<Vec<String>>,
}

#[derive(Serialize)]
struct CandidateLockIdentity<'a> {
    schema: &'a str,
    candidate_revision: &'a str,
    artifact_digest: &'a str,
    model: &'a Option<String>,
    reasoning_effort: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    product: &'a Option<crate::lock::CandidateProductLock>,
}

#[derive(Serialize)]
struct LegacyCandidateLockIdentity<'a> {
    schema: &'a str,
    candidate_revision: &'a str,
    artifact_digest: &'a str,
    model: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    product: &'a Option<crate::lock::CandidateProductLock>,
}

pub fn task(value: &TaskLock) -> Result<String> {
    if value.schema == "a3s.bench.task-lock.v2" {
        digest(&TaskLockIdentity {
            schema: &value.schema,
            task_revision: &value.task_revision,
            artifact_digest: &value.artifact_digest,
            judge_revision: &value.judge_revision,
            judge_artifact_digest: &value.judge_artifact_digest,
            judge_model: &value.judge_model,
            resolved_images: &value.resolved_images,
            resources: &value.resources,
            workspace_imports: &value.workspace_imports,
            network_allow_hosts: &value.network_allow_hosts,
        })
    } else {
        digest(&LegacyTaskLockIdentity {
            schema: &value.schema,
            task_revision: &value.task_revision,
            artifact_digest: &value.artifact_digest,
            judge_revision: &value.judge_revision,
            judge_artifact_digest: &value.judge_artifact_digest,
            judge_model: &value.judge_model,
            resolved_images: &value.resolved_images,
        })
    }
}

pub fn candidate(value: &CandidateLock) -> Result<String> {
    if value.schema == "a3s.bench.candidate-lock.v3" {
        digest(&CandidateLockIdentity {
            schema: &value.schema,
            candidate_revision: &value.candidate_revision,
            artifact_digest: &value.artifact_digest,
            model: &value.model,
            reasoning_effort: &value.reasoning_effort,
            product: &value.product,
        })
    } else {
        // v1/v2 are intentionally hashed with their historical identity.  A
        // legacy native-Codex lock is not reinterpreted as a v3 containerized
        // lock merely because it can be deserialized.
        digest(&LegacyCandidateLockIdentity {
            schema: &value.schema,
            candidate_revision: &value.candidate_revision,
            artifact_digest: &value.artifact_digest,
            model: &value.model,
            product: &value.product,
        })
    }
}

fn digest(value: &impl Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

pub fn validate_digest(value: &str) -> Result<()> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow::anyhow!("lock digest must use sha256"))?;
    anyhow::ensure!(
        hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "lock digest must contain exactly 64 hexadecimal characters"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn identity_is_stable_and_covers_semantic_fields() {
        let mut value = CandidateLock {
            schema: "a3s.bench.candidate-lock.v1".into(),
            lock_digest: String::new(),
            candidate_revision: format!("sha256:{}", "a".repeat(64)),
            artifact_digest: format!("sha256:{}", "b".repeat(64)),
            model: None,
            reasoning_effort: None,
            product: None,
        };
        let first = candidate(&value).unwrap();
        assert_eq!(first, candidate(&value).unwrap());
        value.model = Some("openai/test".into());
        assert_ne!(first, candidate(&value).unwrap());
        let model_digest = candidate(&value).unwrap();
        value.schema = "a3s.bench.candidate-lock.v2".into();
        value.product = Some(crate::lock::CandidateProductLock {
            name: "codex-cli".into(),
            version: "codex-cli 1.0.0".into(),
            target_triple: None,
            artifact_set_digest: None,
        });
        assert_ne!(model_digest, candidate(&value).unwrap());

        value.schema = "a3s.bench.candidate-lock.v3".into();
        value.product.as_mut().unwrap().target_triple = Some("x86_64-unknown-linux-musl".into());
        value.product.as_mut().unwrap().artifact_set_digest =
            Some(format!("sha256:{}", "f".repeat(64)));
        let without_reasoning = candidate(&value).unwrap();
        value.reasoning_effort = Some("none".into());
        assert_ne!(without_reasoning, candidate(&value).unwrap());

        let task_lock = TaskLock {
            schema: "a3s.bench.task-lock.v1".into(),
            lock_digest: String::new(),
            task_revision: format!("sha256:{}", "c".repeat(64)),
            artifact_digest: format!("sha256:{}", "c".repeat(64)),
            judge_revision: format!("sha256:{}", "d".repeat(64)),
            judge_artifact_digest: format!("sha256:{}", "e".repeat(64)),
            judge_model: None,
            resolved_images: BTreeMap::new(),
            resources: None,
            workspace_imports: None,
            network_allow_hosts: None,
        };
        let first = task(&task_lock).unwrap();
        validate_digest(&first).unwrap();
        let mut with_model = task_lock;
        with_model.judge_model = Some("custom/grader".into());
        assert_ne!(first, task(&with_model).unwrap());
    }

    #[test]
    fn historical_task_v1_identity_vector_is_unchanged() {
        let value = TaskLock {
            schema: "a3s.bench.task-lock.v1".into(),
            lock_digest: String::new(),
            task_revision: format!("sha256:{}", "c".repeat(64)),
            artifact_digest: format!("sha256:{}", "c".repeat(64)),
            judge_revision: format!("sha256:{}", "d".repeat(64)),
            judge_artifact_digest: format!("sha256:{}", "e".repeat(64)),
            judge_model: None,
            resolved_images: BTreeMap::new(),
            resources: None,
            workspace_imports: None,
            network_allow_hosts: None,
        };
        assert_eq!(
            task(&value).unwrap(),
            "sha256:7b5f8996f5e81032293be4d6345182c8e858049687f74782e879317a622c6e15"
        );
    }

    #[test]
    fn task_v2_identity_covers_resources() {
        let mut value = TaskLock {
            schema: "a3s.bench.task-lock.v2".into(),
            lock_digest: String::new(),
            task_revision: format!("sha256:{}", "c".repeat(64)),
            artifact_digest: format!("sha256:{}", "c".repeat(64)),
            judge_revision: format!("sha256:{}", "d".repeat(64)),
            judge_artifact_digest: format!("sha256:{}", "e".repeat(64)),
            judge_model: None,
            resolved_images: BTreeMap::new(),
            resources: Some(crate::task::TaskResources::default()),
            workspace_imports: Some(Vec::new()),
            network_allow_hosts: None,
        };
        let first = task(&value).unwrap();
        value.resources.as_mut().unwrap().work.memory_bytes += 1;
        assert_ne!(first, task(&value).unwrap());
        let resource_digest = task(&value).unwrap();
        value
            .workspace_imports
            .as_mut()
            .unwrap()
            .push(crate::task::WorkWorkspaceImport {
                name: "cache".into(),
                source_path: "/root/cache".into(),
                target_path: ".cache".into(),
            });
        assert_ne!(resource_digest, task(&value).unwrap());
        let import_digest = task(&value).unwrap();
        value.network_allow_hosts = Some(vec!["pypi.org".into()]);
        assert_ne!(import_digest, task(&value).unwrap());
    }

    #[test]
    fn historical_task_v2_empty_network_identity_vector_is_unchanged() {
        let value = TaskLock {
            schema: "a3s.bench.task-lock.v2".into(),
            lock_digest: String::new(),
            task_revision: format!("sha256:{}", "c".repeat(64)),
            artifact_digest: format!("sha256:{}", "c".repeat(64)),
            judge_revision: format!("sha256:{}", "d".repeat(64)),
            judge_artifact_digest: format!("sha256:{}", "e".repeat(64)),
            judge_model: None,
            resolved_images: BTreeMap::new(),
            resources: Some(crate::task::TaskResources::default()),
            workspace_imports: Some(Vec::new()),
            network_allow_hosts: None,
        };
        assert_eq!(
            task(&value).unwrap(),
            "sha256:337c13a292069440ed9c808ef66208947dc09cdee01bd337992801487121e990"
        );
    }

    #[test]
    fn historical_v2_identity_vector_is_unchanged() {
        // This literal is a frozen historical record used only to prove old
        // identity compatibility. It never selects or invokes a test model.
        let value = CandidateLock {
            schema: "a3s.bench.candidate-lock.v2".into(),
            lock_digest: String::new(),
            candidate_revision: format!("sha256:{}", "a".repeat(64)),
            artifact_digest: format!("sha256:{}", "b".repeat(64)),
            model: Some("gpt-5.6-luna".into()),
            reasoning_effort: None,
            product: Some(crate::lock::CandidateProductLock {
                name: "codex-cli".into(),
                version: "codex-cli 0.147.0".into(),
                target_triple: None,
                artifact_set_digest: None,
            }),
        };

        assert_eq!(
            candidate(&value).unwrap(),
            "sha256:9023418da17f4ca30548996ee730c1d04efcacc01833e05ee9de66d34652e9b3"
        );
    }
}
