mod acl_schema;
mod asset;
mod bench_run;
mod catalog;
mod cli;
mod config;
mod game_judge;
mod judge_source;
mod legacy_judge;
mod lock;
mod lock_identity;
mod model_candidate;
mod oci_asset;
mod output;
mod result_identity;
mod result_record;
mod run_input;
mod run_journal;
mod runtime;
mod runtime_profile;
mod state_fs;
mod submission;
mod task;
mod task_snapshot;
mod workspace;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json_output = args.iter().any(|argument| argument == "--json");
    match cli::run(args.clone()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if json_output {
                output::print_error(&output::command_name(&args), &format!("{error:#}"));
            } else {
                eprintln!("a3s bench: {error:#}");
            }
            ExitCode::from(2)
        }
    }
}
