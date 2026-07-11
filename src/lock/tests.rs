use super::*;

#[test]
fn lock_schemas_reject_unknown_fields() {
    let task = serde_json::json!({
        "schema":"a3s.bench.task-lock.v1",
        "lock_digest":"sha256:test",
        "task_revision":"sha256:test",
        "artifact_digest":"sha256:test",
        "judge_revision":"sha256:test",
        "judge_artifact_digest":"sha256:test",
        "resolved_images":{},
        "unexpected":true
    });
    let candidate = serde_json::json!({
        "schema":"a3s.bench.candidate-lock.v1",
        "lock_digest":"sha256:test",
        "candidate_revision":"sha256:test",
        "artifact_digest":"sha256:test",
        "model":null,
        "unexpected":true
    });
    assert!(serde_json::from_value::<TaskLock>(task).is_err());
    assert!(serde_json::from_value::<TaskLock>(serde_json::json!({
        "schema":"a3s.bench.task-lock.v1",
        "lock_digest":"sha256:test",
        "task_revision":"sha256:test",
        "artifact_digest":"sha256:test",
        "resolved_images":{}
    }))
    .is_err());
    assert!(serde_json::from_value::<CandidateLock>(candidate).is_err());
}

#[cfg(unix)]
#[test]
fn lock_loader_rejects_symlink_file() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real.json");
    let linked = root.path().join("linked.json");
    std::fs::write(&real, "{}").unwrap();
    symlink(&real, &linked).unwrap();
    assert!(read_lock_file(&linked).is_err());
}

#[test]
fn candidate_loader_rejects_revision_substitution() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("candidate.lock.json");
    create_candidate("./examples/smoke-candidate", None, state.path(), &output).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    value["candidate_revision"] = serde_json::Value::String(format!("sha256:{}", "0".repeat(64)));
    std::fs::write(&output, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert!(load_candidate(&output, state.path()).is_err());
}

#[test]
fn candidate_loader_rejects_semantic_field_tampering() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("candidate.lock.json");
    create_candidate("./examples/smoke-candidate", None, state.path(), &output).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    value["model"] = serde_json::Value::String("openai/substituted".into());
    std::fs::write(&output, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = load_candidate(&output, state.path()).unwrap_err();
    assert!(format!("{error:#}").contains("semantic digest mismatch"));
}
