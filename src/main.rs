use std::process::{self, ExitCode};

use clap::Parser;

fn main() -> ExitCode {
    if avm::launcher::is_argocd_invocation() {
        return match avm::launcher::run() {
            Ok(code) => process::exit(code),
            Err(error) => {
                eprintln!("avm dispatcher launcher error: {error}");
                ExitCode::from(error.exit_code())
            }
        };
    }

    avm::complete_env();

    match avm::run(avm::Cli::parse()) {
        Ok(avm::AppOutcome::Success) => ExitCode::SUCCESS,
        Ok(avm::AppOutcome::ChildExit(code)) => process::exit(code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
