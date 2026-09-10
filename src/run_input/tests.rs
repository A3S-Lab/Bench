use super::*;

#[test]
fn rejects_duplicate_run_options() {
    let args = vec![
        "./task".into(),
        "--agent".into(),
        "./agent".into(),
        "--json".into(),
        "--json".into(),
    ];
    assert!(RunOptions::parse(&args).is_err());
}

#[test]
fn parses_codex_reasoning_effort_and_rejects_locked_override() {
    let args = vec![
        "./task".into(),
        "--agent".into(),
        "codex".into(),
        "--model".into(),
        "gpt-5.3-codex-spark".into(),
        "--reasoning-effort".into(),
        "low".into(),
    ];
    let parsed = RunOptions::parse(&args).unwrap();
    assert_eq!(parsed.model.as_deref(), Some("gpt-5.3-codex-spark"));
    assert_eq!(parsed.reasoning_effort.as_deref(), Some("low"));
    let mut locked_args = args;
    locked_args.push("--locked".into());
    let locked = RunOptions::parse(&locked_args).unwrap();
    let error = locked
        .load(
            Path::new("/tmp/does-not-exist"),
            "run",
            None,
            Some("low".into()),
            "docker",
        )
        .err()
        .unwrap();
    assert!(error
        .to_string()
        .contains("cannot alter a locked Candidate"));
}
