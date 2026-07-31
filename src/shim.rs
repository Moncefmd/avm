use std::ffi::OsString;
use std::process::Command;

use crate::error::{AvmError, Result};
use crate::resolver::{VERSION_ENV, resolve_environment_override, resolve_required};
use crate::store::Store;

pub fn dispatch(store: &Store, arguments: Vec<OsString>) -> Result<i32> {
    let environment = std::env::var_os(VERSION_ENV);
    let resolution = match resolve_environment_override(environment.as_deref())? {
        Some(resolution) => resolution,
        None => {
            let cwd = std::env::current_dir()
                .map_err(|error| AvmError::io("determine the current directory", error))?;
            resolve_required(store, None, &cwd)?
        }
    };
    execute_with_guard(store, &resolution.version, arguments, None)
}

pub fn execute<I>(store: &Store, version: &str, arguments: I) -> Result<i32>
where
    I: IntoIterator<Item = OsString>,
{
    execute_with_guard(store, version, arguments, None)
}

fn execute_with_guard<I>(
    store: &Store,
    version: &str,
    arguments: I,
    dispatcher: Option<&std::path::Path>,
) -> Result<i32>
where
    I: IntoIterator<Item = OsString>,
{
    let (binary, _version_lock) = store.binary_for(version)?;
    if dispatcher.is_some_and(|dispatcher| same_file_best_effort(dispatcher, &binary)) {
        return Err(AvmError::Message(
            "selected Argo CD binary resolves back to the AVM dispatcher".to_owned(),
        ));
    }

    let mut command = Command::new(&binary);
    command.args(arguments);
    let status = command
        .status()
        .map_err(|error| AvmError::io(format!("execute {}", binary.display()), error))?;
    if let Some(code) = status.code() {
        return Ok(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Ok(128 + signal);
        }
    }
    Ok(1)
}

fn same_file_best_effort(left: &std::path::Path, right: &std::path::Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}
