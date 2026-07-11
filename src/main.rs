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
mod result_record;
mod run_journal;
mod runtime;
mod state_fs;
mod submission;
mod task;
mod task_snapshot;
mod workspace;

use std::process::ExitCode;

fn main() -> ExitCode {
    match cli::run(std::env::args().skip(1).collect()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("a3s bench: {error:#}");
            ExitCode::from(2)
        }
    }
}
