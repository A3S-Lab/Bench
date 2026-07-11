use super::*;

#[test]
fn concurrent_capture_publishes_one_complete_artifact() {
    let source = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("task.acl"), "complete snapshot").unwrap();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let source = source.path().to_path_buf();
            let state = state.path().to_path_buf();
            std::thread::spawn(move || capture(&source, &state).unwrap())
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
    crate::state_fs::remove_sealed_tree(&artifact).unwrap();
}

#[test]
fn mutation_after_initial_read_fails_without_publication() {
    let source = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let file = source.path().join("task.acl");
    std::fs::write(&file, "generation one").unwrap();
    let error = capture_with_hook(source.path(), state.path(), || {
        std::fs::write(&file, "generation two")?;
        Ok(())
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("source_changed"));
    let artifacts = state.path().join("artifacts");
    assert!(std::fs::read_dir(artifacts).unwrap().all(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".tmp-")));
}

#[test]
fn verification_detects_content_tampering() {
    let source = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("agent.md"), "original").unwrap();
    let digest = capture(source.path(), state.path()).unwrap();
    let artifact = artifact_path(state.path(), &digest).unwrap();
    let file = artifact.join("agent.md");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(file, "tampered").unwrap();
    assert!(verify(&artifact, &digest).is_err());
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
