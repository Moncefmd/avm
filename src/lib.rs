mod app;
mod catalog;
mod cli;
mod error;
mod github;
mod installer;
pub mod platform;
mod release;
mod resolver;
mod shell;
pub mod shim;
mod store;
mod version;

pub use app::{AppOutcome, run};
pub use cli::Cli;
pub use error::{AvmError, Result};
