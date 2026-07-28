use std::process::{self, ExitCode};

use clap::{CommandFactory, Parser};

fn main() -> ExitCode {
    if avm::shim::is_argocd_invocation() {
        return match avm::shim::dispatch() {
            Ok(code) => process::exit(code),
            Err(error) => {
                eprintln!("avm dispatcher error: {error}");
                ExitCode::from(error.exit_code())
            }
        };
    }

    clap_complete::CompleteEnv::with_factory(avm::Cli::command).complete();

    match avm::run(avm::Cli::parse()) {
        Ok(avm::AppOutcome::Success) => ExitCode::SUCCESS,
        Ok(avm::AppOutcome::ChildExit(code)) => process::exit(code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
