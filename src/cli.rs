use crate::{catalog, config, lock, runtime, task, workspace};
use anyhow::Result;
use serde_json::json;
use std::path::Path;

const USAGE: &str = "a3s bench\n\nUsage:\n  a3s bench list [--all] [--json]\n  a3s bench info <task> [--all] [--json]\n  a3s bench run <task> --agent <candidate> [--model <provider/model>] [--locked] [--json]\n  a3s bench result [run-id] [--json]\n  a3s bench advanced check <./task>\n  a3s bench advanced doctor [--json]\n  a3s bench advanced task lock <source> --out <file>\n  a3s bench advanced candidate lock <candidate> [--model <provider/model>] --out <file>\n";

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
        Some("advanced") => advanced(&args[1..]),
        Some("run") => crate::bench_run::execute(&args[1..]),
        Some(command) => Err(anyhow::anyhow!("unknown command {command:?}\n\n{USAGE}")),
    }
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
        .filter(|task| all || task.admission == "admitted")
        .collect();
    if json_output {
        crate::output::print_success("list", json!({"tasks":tasks}))?;
    } else {
        for task in tasks {
            println!("{:<40} {:<12} {}", task.id, task.admission, task.name);
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
        all || entry.admission == "admitted",
        "built-in Task {reference:?} is not admitted; use --all to inspect it"
    );
    if json_output {
        crate::output::print_success("info", json!({"task":entry}))?;
    } else {
        println!(
            "{}\n  admission: {}\n  reason: {}",
            entry.id, entry.admission, entry.admission_reason
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
    let value = lock::create_task(Path::new(&args[0]), &state_root, Path::new(&args[2]))?;
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
            json!({"config":config.path,"runtime":status}),
        )?;
    } else {
        println!("Runtime {} is ready ({})", status.provider, status.detail);
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
