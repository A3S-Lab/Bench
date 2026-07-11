use super::*;

#[test]
fn concurrent_snapshot_publication_uses_one_complete_artifact() {
    let source = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("task.acl"), "complete snapshot").unwrap();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let source = source.path().to_path_buf();
            let state = state.path().to_path_buf();
            std::thread::spawn(move || snapshot(&source, &state).unwrap())
        })
        .collect();
    let digests: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert!(digests.iter().all(|digest| digest == &digests[0]));
    let artifact = artifact_path(state.path(), &digests[0]).unwrap();
    assert_eq!(
        std::fs::read_to_string(artifact.join("task.acl")).unwrap(),
        "complete snapshot"
    );
    assert!(std::fs::read_dir(state.path().join("artifacts"))
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp-")));
    crate::state_fs::remove_sealed_tree(&artifact).unwrap();
}

#[cfg(unix)]
#[test]
fn artifact_lookup_rejects_symlink_directory() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let linked = root.path().join("linked");
    std::fs::create_dir(&real).unwrap();
    symlink(&real, &linked).unwrap();
    assert!(real_directory(&linked).is_err());
}

#[test]
fn artifact_verification_detects_content_tampering() {
    let source = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("agent.md"), "original").unwrap();
    let digest = snapshot(source.path(), state.path()).unwrap();
    let artifact = artifact_path(state.path(), &digest).unwrap();
    let file = artifact.join("agent.md");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&file, permissions).unwrap();
    }
    std::fs::write(file, "tampered").unwrap();
    assert!(verify_artifact(&artifact, &digest).is_err());
    crate::state_fs::remove_sealed_tree(&artifact).unwrap();
}

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
