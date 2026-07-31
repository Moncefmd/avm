mod app;
mod atomic_file;
mod catalog;
mod cli;
mod completion;
mod error;
mod github;
mod installer;
pub mod launcher;
mod onboarding;
pub mod platform;
mod profile;
mod release;
mod resolver;
mod shell;
pub mod shim;
mod store;
mod version;
#[cfg(windows)]
mod windows;

pub use app::{AppOutcome, run};
pub use cli::Cli;
pub use error::{AvmError, Result};

#[doc(hidden)]
pub fn complete_env() {
    completion::complete_env();
}
