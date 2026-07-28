use std::ffi::{OsStr, OsString};
use std::process::Command;

use crate::error::{AvmError, Result};
use crate::platform::Platform;
use crate::resolver::{VERSION_ENV, resolve_environment_override, resolve_required};
use crate::store::{Paths, Store};

pub fn is_argocd_invocation() -> bool {
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(std::path::Path::file_stem)
        .map(|name| os_str_eq_ignore_ascii_case(name, "argocd"))
        .unwrap_or(false)
}

pub fn dispatch() -> Result<i32> {
    let current = std::env::current_exe()
        .map_err(|error| AvmError::io("locate the AVM dispatcher", error))?;
    let bin = current.parent().ok_or_else(|| {
        AvmError::Message("the AVM dispatcher has no parent directory".to_owned())
    })?;
    if !bin
        .file_name()
        .is_some_and(|name| os_str_eq_ignore_ascii_case(name, "bin"))
    {
        return Err(AvmError::Message(format!(
            "the AVM dispatcher must be installed under an AVM `bin` directory: {}",
            current.display()
        )));
    }
    let root = bin.parent().ok_or_else(|| {
        AvmError::Message("the AVM dispatcher cannot determine its AVM home".to_owned())
    })?;
    let store = Store::new(Paths::new(root.to_path_buf()), Platform::current()?);
    let environment = std::env::var_os(VERSION_ENV);
    let resolution = match resolve_environment_override(environment.as_deref())? {
        Some(resolution) => resolution,
        None => {
            let cwd = std::env::current_dir()
                .map_err(|error| AvmError::io("determine the current directory", error))?;
            resolve_required(&store, None, &cwd)?
        }
    };
    execute_with_guard(
        &store,
        &resolution.version,
        std::env::args_os().skip(1),
        Some(&current),
    )
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

fn os_str_eq_ignore_ascii_case(value: &OsStr, expected: &str) -> bool {
    value
        .to_str()
        .map(|value| value.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

fn same_file_best_effort(left: &std::path::Path, right: &std::path::Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}
