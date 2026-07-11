use super::*;

#[test]
fn lock_schemas_reject_unknown_fields() {
    let task = serde_json::json!({
        "schema":"a3s.bench.task-lock.v1",
        "task_revision":"sha256:test",
        "artifact_digest":"sha256:test",
        "resolved_images":{},
        "unexpected":true
    });
    let candidate = serde_json::json!({
        "schema":"a3s.bench.candidate-lock.v1",
        "candidate_revision":"sha256:test",
        "artifact_digest":"sha256:test",
        "model":null,
        "unexpected":true
    });
    assert!(serde_json::from_value::<TaskLock>(task).is_err());
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
