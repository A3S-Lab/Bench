use crate::{catalog, config, lock, runtime, task, workspace};
use anyhow::Result;
use serde_json::json;
use std::path::Path;

const USAGE: &str = "a3s bench\n\nUsage:\n  a3s bench list [--all] [--json]\n  a3s bench info <task> [--all] [--json]\n  a3s bench run <task> --agent <candidate> [--model <provider/model>] [--locked] [--json]\n  a3s bench result [run-id] [--json]\n  a3s bench compare <baseline-run> <candidate-run> [<baseline-run> <candidate-run> ...] [--json]\n  a3s bench advanced check <./task>\n  a3s bench advanced doctor [--json]\n  a3s bench advanced task lock <source> --out <file>\n  a3s bench advanced candidate lock <candidate> [--model <provider/model>] --out <file>\n";

pub fn run(args: Vec<String>) -> Result<u8> {
    if args.as_slice() == ["--component-info", "--json"] {
        println!("{}", serde_json::to_string(&component_info())?);
        return Ok(0);
    }
    match args.first().map(String::as_str) {
        None | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("--version") if args.len() == 1 => {
            println!("a3s-bench {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Some("list") => list(&args[1..]),
        Some("info") => info(&args[1..]),
        Some("result") => result(&args[1..]),
        Some("compare") => compare(&args[1..]),
        Some("advanced") => advanced(&args[1..]),
        Some("run") => crate::bench_run::execute(&args[1..]),
        Some(command) => Err(anyhow::anyhow!("unknown command {command:?}\n\n{USAGE}")),
    }
}

fn compare(args: &[String]) -> Result<u8> {
    anyhow::ensure!(
        args.iter()
            .all(|arg| arg == "--json" || !arg.starts_with('-')),
        "unknown compare option"
    );
    let json_output = args.iter().any(|arg| arg == "--json");
    let run_ids = args
        .iter()
        .filter(|arg| arg.as_str() != "--json")
        .collect::<Vec<_>>();
    anyhow::ensure!(
        args.iter().filter(|arg| arg.as_str() == "--json").count() <= 1,
        "duplicate option \"--json\""
    );
    anyhow::ensure!(
        !run_ids.is_empty() && run_ids.len() % 2 == 0,
        "compare requires one or more <baseline-run> <candidate-run> pairs"
    );
    let state_root = std::env::current_dir()?.join(".a3s/bench");
    let mut pairs = Vec::with_capacity(run_ids.len() / 2);
    for pair in run_ids.chunks_exact(2) {
        crate::run_journal::validate_run_id(pair[0])?;
        crate::run_journal::validate_run_id(pair[1])?;
        let baseline = crate::result_record::LocalResultRecord::load(&state_root, pair[0])?
            .ok_or_else(|| {
                anyhow::anyhow!("completed baseline result {:?} is unavailable", pair[0])
            })?;
        let candidate = crate::result_record::LocalResultRecord::load(&state_root, pair[1])?
            .ok_or_else(|| {
                anyhow::anyhow!("completed candidate result {:?} is unavailable", pair[1])
            })?;
        pairs.push((baseline, candidate));
    }
    let summary = crate::comparison::compare_pairs(&pairs)?;
    if json_output {
        crate::output::print_success("compare", &summary)?;
    } else {
        println!(
            "COMPARED  pairs={}  candidate_wins={}  ties={}  baseline_wins={}",
            summary.pair_count, summary.candidate_wins, summary.ties, summary.baseline_wins
        );
        println!(
            "timeouts: baseline={} candidate={}",
            summary.baseline_timeouts, summary.candidate_timeouts
        );
        for pair in &summary.pairs {
            println!(
                "{:<32} baseline={} candidate={} {}",
                pair.task_id, pair.baseline_score, pair.candidate_score, pair.outcome
            );
        }
    }
    Ok(0)
}

fn component_info() -> serde_json::Value {
    json!({
        "component": "bench",
        "version": env!("CARGO_PKG_VERSION"),
        "target": release_target(),
        "cli_protocol": "a3s-bench-cli/v1"
    })
}

fn release_target() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        value => value,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        value => value,
    };
    format!("{os}-{arch}")
}

fn list(args: &[String]) -> Result<u8> {
    let (all, json_output) = parse_flags(args, &["--all", "--json"])?;
    let catalog = catalog::load()?;
    let tasks: Vec<_> = catalog
        .tasks
        .into_iter()
        .filter(|task| all || task.availability == "ready")
        .collect();
    if json_output {
        crate::output::print_success("list", json!({"tasks":tasks}))?;
    } else {
        for task in tasks {
            println!(
                "{:<40} {:<13} {:<12} {}",
                task.id, task.execution_class, task.availability, task.name
            );
        }
    }
    Ok(0)
}

fn info(args: &[String]) -> Result<u8> {
    anyhow::ensure!(!args.is_empty(), "info requires exactly one Task reference");
    let reference = &args[0];
    let (all, json_output) = parse_flags(&args[1..], &["--all", "--json"])?;
    if reference.starts_with("./") || reference.starts_with("../") {
        anyhow::ensure!(!all, "--all applies only to a built-in Task ID");
        let info = task::load_local(Path::new(reference))?;
        if json_output {
            crate::output::print_success("info", json!({"task":info}))?;
        } else {
            println!(
                "{}\n  name: {}\n  category: {}\n  judge: {}",
                info.id, info.name, info.category, info.judge_asset
            );
        }
        return Ok(0);
    }
    let entry = catalog::load()?
        .tasks
        .into_iter()
        .find(|task| task.id == *reference)
        .ok_or_else(|| anyhow::anyhow!("unknown built-in Task {reference:?}"))?;
    anyhow::ensure!(
        all || entry.availability == "ready",
        "built-in Task {reference:?} is not locally runnable; use --all to inspect it"
    );
    if json_output {
        crate::output::print_success("info", json!({"task":entry}))?;
    } else {
        println!(
            "{}\n  class: {}\n  availability: {}\n  availability reason: {}\n  admission: {}\n  admission reason: {}",
            entry.id,
            entry.execution_class,
            entry.availability,
            entry.availability_reason,
            entry.admission,
            entry.admission_reason
        );
    }
    Ok(0)
}

fn advanced(args: &[String]) -> Result<u8> {
    match args.first().map(String::as_str) {
        Some("check") if args.len() == 2 => {
            let info = task::load_local(Path::new(&args[1]))?;
            println!("valid Task {} with Judge {}", info.id, info.judge_asset);
            Ok(0)
        }
        Some("doctor") => doctor(&args[1..]),
        Some("task") if args.get(1).map(String::as_str) == Some("lock") => {
            advanced_task_lock(&args[2..])
        }
        Some("candidate") if args.get(1).map(String::as_str) == Some("lock") => {
            advanced_candidate_lock(&args[2..])
        }
        _ => Err(anyhow::anyhow!("invalid advanced command")),
    }
}

fn advanced_task_lock(args: &[String]) -> Result<u8> {
    anyhow::ensure!(
        args.len() == 3 && args[1] == "--out",
        "usage: advanced task lock <source> --out <file>"
    );
    let state_root = workspace::state_root()?;
    let source = catalog::resolve_task_reference(&args[0])?;
    let config = config::discover(&std::env::current_dir()?)?;
    if let (Some(path), Some(model)) = (config.path.as_deref(), config.judge_model.as_deref()) {
        config::resolve_model_route(path, model)?;
    }
    let runtime_provider = config.runtime.provider.as_str().to_owned();
    let value = lock::create_task_with_provider(
        &source,
        config.judge_model,
        &state_root,
        Path::new(&args[2]),
        &runtime_provider,
    )?;
    println!("locked Task {}", value.task_revision);
    Ok(0)
}

fn advanced_candidate_lock(args: &[String]) -> Result<u8> {
    anyhow::ensure!(
        !args.is_empty(),
        "candidate lock requires a Candidate adapter"
    );
    let source = &args[0];
    let mut output = None;
    let mut model = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--out" if output.is_none() && index + 1 < args.len() => {
                output = Some(args[index + 1].clone());
                index += 2;
            }
            "--model" if model.is_none() && index + 1 < args.len() => {
                model = Some(args[index + 1].clone());
                index += 2;
            }
            value => return Err(anyhow::anyhow!("invalid candidate lock option {value:?}")),
        }
    }
    let output = output.ok_or_else(|| anyhow::anyhow!("candidate lock requires --out"))?;
    let state_root = workspace::state_root()?;
    let value = lock::create_candidate(source, model, &state_root, Path::new(&output))?;
    println!("locked Candidate {}", value.candidate_revision);
    Ok(0)
}

fn doctor(args: &[String]) -> Result<u8> {
    let (_, json_output) = parse_flags(args, &["--json"])?;
    let cwd = std::env::current_dir()?;
    let config = config::discover(&cwd)?;
    let status = runtime::preflight(&config.runtime)?;
    if json_output {
        crate::output::print_success(
            "advanced doctor",
            json!({"config":config.path,"runtime":status,"judge_model":config.judge_model}),
        )?;
    } else {
        println!("Runtime {} is ready ({})", status.provider, status.detail);
        if let Some(model) = config.judge_model {
            println!("Judge model route: {model}");
        }
    }
    Ok(0)
}

fn result(args: &[String]) -> Result<u8> {
    let mut run_id = None;
    let mut json_output = false;
    for arg in args {
        match arg.as_str() {
            "--json" if !json_output => json_output = true,
            value if !value.starts_with('-') && run_id.is_none() => run_id = Some(value.to_owned()),
            value => {
                return Err(anyhow::anyhow!(
                    "invalid or duplicate result argument {value:?}"
                ))
            }
        }
    }
    let state_root = std::env::current_dir()?.join(".a3s/bench");
    let run_id = match run_id {
        Some(value) => value,
        None => crate::result_record::LocalResultRecord::latest_run_id(&state_root)?,
    };
    crate::run_journal::validate_run_id(&run_id)?;
    match crate::result_record::LocalResultRecord::load(&state_root, &run_id)? {
        Some(record) => print_completed_result(&record, json_output)?,
        None => {
            let journal = crate::run_journal::RunJournal::load(&state_root, &run_id)?;
            anyhow::ensure!(
                journal.stage != crate::run_journal::RunStage::Completed,
                "completed run result is missing"
            );
            let projection = journal.public_projection();
            if json_output {
                crate::output::print_success("result", projection)?;
            } else {
                println!(
                    "{}  task={}",
                    journal.stage.to_string().to_ascii_uppercase(),
                    journal.task_reference
                );
                println!("run:    {run_id}");
            }
        }
    }
    Ok(0)
}

fn print_completed_result(
    record: &crate::result_record::LocalResultRecord,
    json_output: bool,
) -> Result<()> {
    if json_output {
        crate::output::print_success("result", record.public_projection())?;
    } else {
        println!("COMPLETED  score={}  task={}", record.score, record.task_id);
        if let Some(timeout_sec) = record
            .candidate_execution
            .as_ref()
            .filter(|execution| execution.is_timed_out())
            .and_then(|execution| execution.timeout_sec)
        {
            println!("candidate: timed_out ({timeout_sec}s)");
        }
        println!("run:    {}", record.run_id);
    }
    Ok(())
}

fn parse_flags(args: &[String], allowed: &[&str]) -> Result<(bool, bool)> {
    let mut all = false;
    let mut json = false;
    for arg in args {
        anyhow::ensure!(allowed.contains(&arg.as_str()), "unknown option {arg:?}");
        match arg.as_str() {
            "--all" if !all => all = true,
            "--json" if !json => json = true,
            _ => return Err(anyhow::anyhow!("duplicate option {arg:?}")),
        }
    }
    Ok((all, json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_matches_cli_contract() {
        let value = component_info();
        assert_eq!(value["component"], "bench");
        assert_eq!(value["cli_protocol"], "a3s-bench-cli/v1");
    }

    #[test]
    fn usage_names_the_product_neutral_candidate() {
        assert!(USAGE.contains("--agent <candidate>"));
        assert!(USAGE.contains("advanced task lock <source> --out <file>"));
        assert!(USAGE.contains("advanced candidate lock <candidate>"));
        assert!(!USAGE.contains("--agent <agent>"));
        assert!(!USAGE.contains("--agent <asset>"));
    }

    #[test]
    fn duplicate_flags_fail() {
        assert!(parse_flags(&["--json".into(), "--json".into()], &["--json"]).is_err());
    }
}
