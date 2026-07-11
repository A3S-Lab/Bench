use crate::lock::{CandidateLock, TaskLock};
use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct TaskLockIdentity<'a> {
    schema: &'a str,
    task_revision: &'a str,
    artifact_digest: &'a str,
    judge_revision: &'a str,
    judge_artifact_digest: &'a str,
    judge_model: &'a Option<String>,
    resolved_images: &'a std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct CandidateLockIdentity<'a> {
    schema: &'a str,
    candidate_revision: &'a str,
    artifact_digest: &'a str,
    model: &'a Option<String>,
}

pub fn task(value: &TaskLock) -> Result<String> {
    digest(&TaskLockIdentity {
        schema: &value.schema,
        task_revision: &value.task_revision,
        artifact_digest: &value.artifact_digest,
        judge_revision: &value.judge_revision,
        judge_artifact_digest: &value.judge_artifact_digest,
        judge_model: &value.judge_model,
        resolved_images: &value.resolved_images,
    })
}

pub fn candidate(value: &CandidateLock) -> Result<String> {
    digest(&CandidateLockIdentity {
        schema: &value.schema,
        candidate_revision: &value.candidate_revision,
        artifact_digest: &value.artifact_digest,
        model: &value.model,
    })
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
        };
        let first = candidate(&value).unwrap();
        assert_eq!(first, candidate(&value).unwrap());
        value.model = Some("openai/test".into());
        assert_ne!(first, candidate(&value).unwrap());

        let task_lock = TaskLock {
            schema: "a3s.bench.task-lock.v1".into(),
            lock_digest: String::new(),
            task_revision: format!("sha256:{}", "c".repeat(64)),
            artifact_digest: format!("sha256:{}", "c".repeat(64)),
            judge_revision: format!("sha256:{}", "d".repeat(64)),
            judge_artifact_digest: format!("sha256:{}", "e".repeat(64)),
            judge_model: None,
            resolved_images: BTreeMap::new(),
        };
        let first = task(&task_lock).unwrap();
        validate_digest(&first).unwrap();
        let mut with_model = task_lock;
        with_model.judge_model = Some("custom/grader".into());
        assert_ne!(first, task(&with_model).unwrap());
    }
}
