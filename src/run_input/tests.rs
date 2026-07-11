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
