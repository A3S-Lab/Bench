use super::*;

fn write_test_task_lock(
    source: &Path,
    schema: &str,
    resources: Option<crate::task::TaskResources>,
    state_root: &Path,
    output: &Path,
) -> TaskLock {
    let task_digest = crate::task_snapshot::capture(source, state_root).unwrap();
    let task_artifact = crate::task_snapshot::artifact_path(state_root, &task_digest).unwrap();
    let task = crate::task::load_local(&task_artifact).unwrap();
    let judge = resolve_judge(&task, state_root).unwrap();
    let judge_artifact_digest = crate::task_snapshot::capture(&judge.root, state_root).unwrap();
    let judge_artifact =
        crate::task_snapshot::artifact_path(state_root, &judge_artifact_digest).unwrap();
    let judge = crate::asset::load_local(&judge_artifact).unwrap();
    let mut value = TaskLock {
        schema: schema.into(),
        lock_digest: String::new(),
        task_revision: task_digest.clone(),
        artifact_digest: task_digest,
        judge_revision: judge.identity,
        judge_artifact_digest,
        judge_model: None,
        resolved_images: BTreeMap::new(),
        resources,
        workspace_imports: (schema == "a3s.bench.task-lock.v2")
            .then(|| task.work_workspace_imports.clone()),
        network_allow_hosts: (schema == "a3s.bench.task-lock.v2"
            && !task.work_network_allow_hosts.is_empty())
        .then(|| task.work_network_allow_hosts.clone()),
    };
    value.lock_digest = crate::lock_identity::task(&value).unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    value
}

#[test]
fn lock_schemas_reject_unknown_fields() {
    let task = serde_json::json!({
        "schema":"a3s.bench.task-lock.v1",
        "lock_digest":"sha256:test",
        "task_revision":"sha256:test",
        "artifact_digest":"sha256:test",
        "judge_revision":"sha256:test",
        "judge_artifact_digest":"sha256:test",
        "judge_model":null,
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

#[test]
fn judge_model_references_are_closed() {
    assert!(crate::config::validate_model_reference("provider/model").is_ok());
    for value in ["", "provider", "/model", "provider/", "a/b/c"] {
        assert!(
            crate::config::validate_model_reference(value).is_err(),
            "{value}"
        );
    }
}

#[test]
fn codex_reasoning_default_is_ignored_for_agent_tool() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("candidate.lock.json");
    let value = create_candidate_with_codex_default(
        "a3s-code-core",
        None,
        None,
        Some("low".into()),
        state.path(),
        &output,
    )
    .unwrap();

    assert_eq!(value.schema, "a3s.bench.candidate-lock.v1");
    assert_eq!(value.reasoning_effort, None);
}

#[test]
fn task_lock_creation_preserves_the_v1_contract() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("task.lock.json");
    let value = create_task_with_provider(
        Path::new("examples/smoke"),
        None,
        state.path(),
        &output,
        crate::os_runtime::PROVIDER,
    )
    .unwrap();
    assert_eq!(value.schema, "a3s.bench.task-lock.v1");
    assert_eq!(value.resources, None);
    assert_eq!(value.workspace_imports, None);
    assert_eq!(value.network_allow_hosts, None);
    load_task(&output, state.path()).unwrap();
}

#[test]
fn task_lock_v2_rejects_resources_that_disagree_with_artifact() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("task.lock.json");
    let source = Path::new("builtin/tasks/ad_placement_optimization");
    let task = crate::task::load_local(source).unwrap();
    let mut value = write_test_task_lock(
        source,
        "a3s.bench.task-lock.v2",
        Some(task.resources),
        state.path(),
        &output,
    );
    value.resources.as_mut().unwrap().work.memory_bytes += 1;
    value.lock_digest = crate::lock_identity::task(&value).unwrap();
    std::fs::write(&output, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = load_task(&output, state.path()).err().unwrap();
    assert!(error
        .to_string()
        .contains("resources do not match the locked Task artifact"));
}

#[test]
fn task_lock_v2_rejects_workspace_imports_that_disagree_with_artifact() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("task.lock.json");
    let source = Path::new("builtin/tasks/exchange_core_throughput");
    let task = crate::task::load_local(source).unwrap();
    let mut value = write_test_task_lock(
        source,
        "a3s.bench.task-lock.v2",
        Some(task.resources),
        state.path(),
        &output,
    );
    value.workspace_imports.as_mut().unwrap()[0].target_path = ".cache/maven".into();
    value.lock_digest = crate::lock_identity::task(&value).unwrap();
    std::fs::write(&output, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = load_task(&output, state.path()).err().unwrap();
    assert!(error
        .to_string()
        .contains("workspace imports do not match the locked Task artifact"));
}

#[test]
fn task_lock_v2_rejects_network_allow_hosts_that_disagree_with_artifact() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("task.lock.json");
    let source = Path::new("builtin/tasks/cta_risk_budget_optimization");
    let task = crate::task::load_local(source).unwrap();
    let mut value = write_test_task_lock(
        source,
        "a3s.bench.task-lock.v2",
        Some(task.resources),
        state.path(),
        &output,
    );
    value.network_allow_hosts = Some(vec!["example.com".into()]);
    value.lock_digest = crate::lock_identity::task(&value).unwrap();
    std::fs::write(&output, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = load_task(&output, state.path()).err().unwrap();
    assert!(error
        .to_string()
        .contains("network allow hosts do not match the locked Task artifact"));
}

#[test]
fn task_lock_loader_rejects_both_schema_artifact_cross_pairs() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("task.lock.json");
    write_test_task_lock(
        Path::new("examples/smoke"),
        "a3s.bench.task-lock.v2",
        Some(crate::task::TaskResources::default()),
        state.path(),
        &output,
    );
    let error = load_task(&output, state.path()).err().unwrap();
    assert!(error
        .to_string()
        .contains("TaskLock v2 cannot bind a task/v1 artifact"));

    let source = Path::new("builtin/tasks/ad_placement_optimization");
    write_test_task_lock(
        source,
        "a3s.bench.task-lock.v1",
        None,
        state.path(),
        &output,
    );
    let error = load_task(&output, state.path()).err().unwrap();
    assert!(error
        .to_string()
        .contains("TaskLock v1 cannot bind a task/v2 artifact"));
}

#[test]
fn os_runtime_task_lock_binds_managed_runner_images_without_docker() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("task.lock.json");
    let value = create_task_with_provider(
        Path::new("examples/smoke"),
        None,
        state.path(),
        &output,
        crate::os_runtime::PROVIDER,
    )
    .unwrap();
    assert_eq!(value.schema, "a3s.bench.task-lock.v1");
    assert_eq!(value.resources, None);
    assert_eq!(value.workspace_imports, None);
    assert_eq!(value.network_allow_hosts, None);
    assert_eq!(value.resolved_images.len(), 2);
    assert!(value
        .resolved_images
        .contains_key(crate::os_runtime::CANDIDATE_IMAGE_KEY));
    assert!(value
        .resolved_images
        .contains_key(crate::os_runtime::JUDGE_IMAGE_KEY));
}

#[test]
fn os_runtime_rejects_explicit_task_v2_resources_before_locking() {
    let state = tempfile::tempdir().unwrap();
    let output = state.path().join("task.lock.json");
    let error = create_task_with_provider(
        Path::new("builtin/tasks/ad_placement_optimization"),
        None,
        state.path(),
        &output,
        crate::os_runtime::PROVIDER,
    )
    .err()
    .unwrap();
    assert!(error
        .to_string()
        .contains("cannot enforce explicit a3s-bench/task/v2 resources"));
    assert!(!output.exists());
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

#[test]
fn model_candidate_requires_declared_definition() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("candidate.lock.json");
    let error = create_candidate(
        "./examples/executable-candidate",
        Some("openai/test".into()),
        root.path(),
        &output,
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("source.definition_path"), "{message}");
}

#[test]
fn model_candidate_uses_manifest_definition_path() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("candidate.lock.json");
    let value = create_candidate(
        "./examples/model-candidate",
        Some("openai/test".into()),
        root.path(),
        &output,
    )
    .unwrap();
    assert_eq!(value.model.as_deref(), Some("openai/test"));
    let (_, captured) = load_candidate(&output, root.path()).unwrap();
    let loaded = crate::asset::load_local(&captured).unwrap();
    assert_eq!(
        loaded.definition_path.as_deref(),
        Some("prompts/controller.md")
    );
}
