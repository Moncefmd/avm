use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{AvmError, Result};

const DISPATCH_PROTOCOL: &str = "__dispatch-v1";

pub fn is_argocd_invocation() -> bool {
    std::env::args_os()
        .next()
        .as_deref()
        .and_then(|argument| Path::new(argument).file_stem())
        .is_some_and(|name| command_name_matches(name, "argocd"))
}

pub fn run() -> Result<i32> {
    let launcher = std::env::current_exe()
        .map_err(|error| AvmError::io("locate the AVM dispatcher launcher", error))?;
    let avm_home = avm_home_from_launcher(&launcher)?;
    let avm = find_avm(&launcher)?;
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    run_with(&avm, &avm_home, arguments)
}

fn avm_home_from_launcher(launcher: &Path) -> Result<PathBuf> {
    let bin = launcher.parent().ok_or_else(|| {
        AvmError::Message("the AVM dispatcher launcher has no parent directory".to_owned())
    })?;
    if !bin
        .file_name()
        .is_some_and(|name| command_name_matches(name, "bin"))
    {
        return Err(AvmError::Message(format!(
            "the AVM dispatcher launcher must be installed under an AVM `bin` directory: {}",
            launcher.display()
        )));
    }
    bin.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AvmError::Message("the AVM dispatcher cannot determine AVM_HOME".to_owned()))
}

fn find_avm(launcher: &Path) -> Result<PathBuf> {
    find_avm_in(launcher, std::env::var_os("PATH").as_deref())
}

fn find_avm_in(launcher: &Path, path: Option<&OsStr>) -> Result<PathBuf> {
    let binary_name = if cfg!(windows) { "avm.exe" } else { "avm" };
    let mut candidates = Vec::new();
    let adjacent = launcher.parent().map(|parent| parent.join(binary_name));
    if let Some(path) = path {
        candidates.extend(
            std::env::split_paths(&path)
                .filter(|entry| entry.is_absolute())
                .map(|entry| entry.join(binary_name)),
        );
    }
    // Preserve ordinary PATH precedence. A standalone installer places `avm`
    // beside this launcher, so use that only when it was not already represented
    // by an absolute PATH entry.
    if let Some(adjacent) = adjacent
        && !candidates
            .iter()
            .any(|candidate| same_file_best_effort(candidate, &adjacent))
    {
        candidates.push(adjacent);
    }

    for candidate in candidates {
        if !candidate.is_absolute()
            || same_file_best_effort(&candidate, launcher)
            || !is_executable_file(&candidate)
        {
            continue;
        }
        return Ok(candidate);
    }

    Err(AvmError::Message(format!(
        "could not find the current `{binary_name}` executable in an absolute PATH directory; reinstall AVM or run `avm init`"
    )))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn run_with(avm: &Path, avm_home: &Path, arguments: Vec<OsString>) -> Result<i32> {
    let mut command = Command::new(avm);
    command
        .arg("--avm-home")
        .arg(avm_home)
        .arg(DISPATCH_PROTOCOL)
        .arg("--")
        .args(arguments);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        Err(AvmError::io(
            format!("start current AVM executable {}", avm.display()),
            error,
        ))
    }
    #[cfg(not(unix))]
    {
        let status = command.status().map_err(|error| {
            AvmError::io(
                format!("start current AVM executable {}", avm.display()),
                error,
            )
        })?;
        Ok(status.code().unwrap_or(1))
    }
}

fn command_name_matches(value: &OsStr, expected: &str) -> bool {
    value
        .to_str()
        .map(|value| {
            if cfg!(windows) {
                value.eq_ignore_ascii_case(expected)
            } else {
                value == expected
            }
        })
        .unwrap_or(false)
}

fn same_file_best_effort(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_home_only_from_a_bin_launcher() {
        let launcher = Path::new("/tmp/example/bin/argocd");
        assert_eq!(
            avm_home_from_launcher(launcher).unwrap(),
            Path::new("/tmp/example")
        );
        assert!(avm_home_from_launcher(Path::new("/tmp/example/argocd")).is_err());
    }

    #[test]
    fn relative_path_entries_are_never_launcher_candidates() {
        let temporary = tempfile::tempdir().unwrap();
        let launcher = temporary.path().join(if cfg!(windows) {
            "bin/argocd.exe"
        } else {
            "bin/argocd"
        });
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        fs::write(&launcher, b"launcher").unwrap();

        let path = std::env::join_paths([Path::new("relative")]).unwrap();
        let result = find_avm_in(&launcher, Some(&path));

        assert!(result.is_err());
    }

    #[test]
    fn absolute_path_order_is_preserved() {
        let temporary = tempfile::tempdir().unwrap();
        let bin = temporary.path().join("home/bin");
        let managed = temporary.path().join("package/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&managed).unwrap();
        let launcher = bin.join(if cfg!(windows) {
            "argocd.exe"
        } else {
            "argocd"
        });
        let binary_name = if cfg!(windows) { "avm.exe" } else { "avm" };
        let adjacent = bin.join(binary_name);
        let packaged = managed.join(binary_name);
        for path in [&launcher, &adjacent, &packaged] {
            fs::write(path, b"executable").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let path = std::env::join_paths([managed, bin]).unwrap();

        assert_eq!(find_avm_in(&launcher, Some(&path)).unwrap(), packaged);
    }

    #[test]
    fn adjacent_path_entry_wins_over_a_later_candidate() {
        let temporary = tempfile::tempdir().unwrap();
        let bin = temporary.path().join("home/bin");
        let later = temporary.path().join("later/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&later).unwrap();
        let launcher = bin.join(if cfg!(windows) {
            "argocd.exe"
        } else {
            "argocd"
        });
        let binary_name = if cfg!(windows) { "avm.exe" } else { "avm" };
        let adjacent = bin.join(binary_name);
        let later_avm = later.join(binary_name);
        for path in [&launcher, &adjacent, &later_avm] {
            fs::write(path, b"executable").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let path = std::env::join_paths([bin, later]).unwrap();

        assert_eq!(find_avm_in(&launcher, Some(&path)).unwrap(), adjacent);
    }

    #[test]
    fn adjacent_standalone_install_remains_a_fallback() {
        let temporary = tempfile::tempdir().unwrap();
        let bin = temporary.path().join("home/bin");
        fs::create_dir_all(&bin).unwrap();
        let launcher = bin.join(if cfg!(windows) {
            "argocd.exe"
        } else {
            "argocd"
        });
        let adjacent = bin.join(if cfg!(windows) { "avm.exe" } else { "avm" });
        for path in [&launcher, &adjacent] {
            fs::write(path, b"executable").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }

        assert_eq!(find_avm_in(&launcher, None).unwrap(), adjacent);
    }
}
