use std::fs;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command as ProcessCommand;

use directories::BaseDirs;
use tempfile::NamedTempFile;

use crate::cli::Shell;
use crate::error::{AvmError, Result};
use crate::store::Store;

const PROFILE_BLOCK_START: &str = "# >>> avm setup >>>";
const PROFILE_BLOCK_END: &str = "# <<< avm setup <<<";
const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

pub(crate) fn setup(store: &Store, shell: Shell, dry_run: bool) -> Result<()> {
    let line = init_line(shell, &store.paths.bin);

    #[cfg(not(windows))]
    if shell == Shell::Powershell {
        let instruction = format!(
            "automatic PowerShell profile selection is ambiguous on this platform; \
             add this line to $PROFILE:\n{line}"
        );
        if dry_run {
            println!("AVM setup preview for {shell}:");
            println!("{instruction}");
            println!("No files were changed.");
            return Ok(());
        }
        return Err(AvmError::Message(instruction));
    }

    if dry_run {
        println!("AVM setup preview for {shell}:");
        println!("Dispatcher: {}", store.shim_path().display());
        if shell == Shell::Powershell {
            #[cfg(windows)]
            println!("User PATH: add {}", store.paths.bin.display());
        } else {
            for profile in shell_profiles(shell)? {
                println!("Profile: {}", profile.display());
                println!("{PROFILE_BLOCK_START}");
                println!("{line}");
                println!("{PROFILE_BLOCK_END}");
            }
        }
        println!("Run `avm setup --shell {shell}` to apply the change.");
        println!("No files were changed.");
        return Ok(());
    }

    store.ensure_dispatcher()?;
    if shell == Shell::Powershell {
        #[cfg(windows)]
        {
            apply_windows_user_path(&store.paths.bin)?;
            println!(
                "Added {} to the user PATH. Open a new terminal to use `argocd`.",
                store.paths.bin.display()
            );
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            unreachable!("non-Windows PowerShell setup is rejected before mutation");
        }
    }

    let profiles = shell_profiles(shell)?;
    update_shell_profiles(&profiles, &line)?;
    let configured = profiles
        .iter()
        .map(|profile| profile.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!("Configured {shell} in {configured}.");
    Ok(())
}

#[cfg(windows)]
fn apply_windows_user_path(bin: &Path) -> Result<()> {
    const SCRIPT: &str = r#"
$target = $env:AVM_SETUP_BIN
$current = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @()
if (-not [string]::IsNullOrWhiteSpace($current)) {
    $entries = $current -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
}
if ($entries | Where-Object { $_.TrimEnd('\', '/') -ieq $target.TrimEnd('\', '/') }) {
    exit 0
}
$next = if ([string]::IsNullOrWhiteSpace($current)) { $target } else { "$target;$current" }
[Environment]::SetEnvironmentVariable('Path', $next, 'User')
"#;
    let status = ProcessCommand::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .env("AVM_SETUP_BIN", bin)
        .status()
        .map_err(|error| AvmError::io("start PowerShell to update the user PATH", error))?;
    if !status.success() {
        return Err(AvmError::Message(format!(
            "PowerShell could not update the user PATH (exit {})",
            status.code().unwrap_or(1)
        )));
    }
    Ok(())
}

fn shell_profiles(shell: Shell) -> Result<Vec<PathBuf>> {
    let home = BaseDirs::new()
        .ok_or_else(|| AvmError::Message("could not determine the home directory".to_owned()))?
        .home_dir()
        .to_path_buf();
    shell_profiles_in(&home, shell)
}

fn shell_profiles_in(home: &Path, shell: Shell) -> Result<Vec<PathBuf>> {
    match shell {
        Shell::Bash => {
            let login = bash_login_profile(home)?;
            Ok(vec![home.join(".bashrc"), login])
        }
        Shell::Zsh => Ok(vec![home.join(".zshrc")]),
        Shell::Fish => Ok(vec![home.join(".config").join("fish").join("config.fish")]),
        Shell::Powershell => Err(AvmError::CompletionInstallUnsupported(
            "automatic PowerShell profile selection".to_owned(),
        )),
    }
}

fn bash_login_profile(home: &Path) -> Result<PathBuf> {
    for name in [".bash_profile", ".bash_login", ".profile"] {
        let candidate = home.join(name);
        match fs::symlink_metadata(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(AvmError::io(
                    format!("inspect {}", candidate.display()),
                    error,
                ));
            }
        }
    }
    Ok(home.join(".bash_profile"))
}

struct ProfileUpdate {
    path: PathBuf,
    contents: Vec<u8>,
    permissions: Option<fs::Permissions>,
}

fn update_shell_profiles(paths: &[PathBuf], command: &str) -> Result<()> {
    let updates = paths
        .iter()
        .map(|path| plan_shell_profile_update(path, command))
        .collect::<Result<Vec<_>>>()?;
    for update in updates.into_iter().flatten() {
        apply_shell_profile_update(update)?;
    }
    Ok(())
}

fn plan_shell_profile_update(path: &Path, command: &str) -> Result<Option<ProfileUpdate>> {
    let (existing, permissions) = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(AvmError::UnsafePath(path.to_path_buf()));
        }
        Ok(metadata) if metadata.len() > MAX_PROFILE_BYTES => {
            return Err(AvmError::Message(format!(
                "refusing to edit oversized shell profile {}",
                path.display()
            )));
        }
        Ok(metadata) => (
            fs::read(path)
                .map_err(|error| AvmError::io(format!("read {}", path.display()), error))?,
            Some(metadata.permissions()),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
        Err(error) => return Err(AvmError::io(format!("inspect {}", path.display()), error)),
    };
    if existing.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(AvmError::Message(format!(
            "refusing to edit UTF-8 BOM shell profile {}",
            path.display()
        )));
    }
    let existing = String::from_utf8(existing).map_err(|_| {
        AvmError::Message(format!(
            "refusing to edit non-UTF-8 shell profile {}",
            path.display()
        ))
    })?;
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let block =
        format!("{PROFILE_BLOCK_START}{newline}{command}{newline}{PROFILE_BLOCK_END}{newline}");
    let starts = existing
        .match_indices(PROFILE_BLOCK_START)
        .collect::<Vec<_>>();
    let ends = existing
        .match_indices(PROFILE_BLOCK_END)
        .collect::<Vec<_>>();
    let updated = match (starts.as_slice(), ends.as_slice()) {
        ([], []) => {
            let mut updated = existing.clone();
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push_str(newline);
            }
            updated.push_str(&block);
            updated
        }
        ([(start, _)], [(end, _)]) if start < end => {
            let suffix_start = end + PROFILE_BLOCK_END.len();
            let suffix = existing[suffix_start..]
                .strip_prefix("\r\n")
                .or_else(|| existing[suffix_start..].strip_prefix('\n'))
                .unwrap_or(&existing[suffix_start..]);
            format!("{}{block}{suffix}", &existing[..*start])
        }
        _ => {
            return Err(AvmError::Message(format!(
                "shell profile {} contains malformed or duplicate AVM markers",
                path.display()
            )));
        }
    };
    if updated == existing {
        return Ok(None);
    }
    Ok(Some(ProfileUpdate {
        path: path.to_path_buf(),
        contents: updated.into_bytes(),
        permissions,
    }))
}

fn apply_shell_profile_update(update: ProfileUpdate) -> Result<()> {
    let parent = update
        .path
        .parent()
        .ok_or_else(|| AvmError::UnsafePath(update.path.clone()))?;
    fs::create_dir_all(parent)
        .map_err(|error| AvmError::io(format!("create {}", parent.display()), error))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
        AvmError::io(
            format!("create temporary file in {}", parent.display()),
            error,
        )
    })?;
    if let Some(permissions) = update.permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| {
                AvmError::io(
                    format!("set permissions for {}", update.path.display()),
                    error,
                )
            })?;
    }
    std::io::Write::write_all(&mut temporary, &update.contents).map_err(|error| {
        AvmError::io(format!("write temporary {}", update.path.display()), error)
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        AvmError::io(format!("sync temporary {}", update.path.display()), error)
    })?;
    temporary
        .persist(&update.path)
        .map_err(|error| AvmError::io(format!("replace {}", update.path.display()), error.error))?;
    Ok(())
}

pub(crate) fn detected_shell() -> Shell {
    let shell = std::env::var("SHELL")
        .unwrap_or_default()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if shell.ends_with("/fish") || shell == "fish" {
        Shell::Fish
    } else if shell.ends_with("/zsh") || shell == "zsh" {
        Shell::Zsh
    } else if shell.ends_with("/bash") || shell == "bash" {
        Shell::Bash
    } else if cfg!(windows) {
        Shell::Powershell
    } else {
        Shell::Bash
    }
}

pub(crate) fn init_line(shell: Shell, bin: &Path) -> String {
    let native_bin = bin.to_string_lossy();
    match shell {
        Shell::Bash | Shell::Zsh => {
            let bin = posix_shell_path(bin);
            format!(
                "case \":$PATH:\" in *:{}:*) ;; *) export PATH={}:\"$PATH\" ;; esac",
                posix_single_quote(&bin),
                posix_single_quote(&bin)
            )
        }
        Shell::Fish => {
            let bin = posix_shell_path(bin);
            format!("fish_add_path -- {}", fish_single_quote(&bin))
        }
        Shell::Powershell => format!(
            "$env:Path = '{};' + $env:Path",
            native_bin.replace('\'', "''")
        ),
    }
}

fn posix_shell_path(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        let normalized = normalized.strip_prefix("//?/").unwrap_or(&normalized);
        let bytes = normalized.as_bytes();
        if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' {
            let drive = (bytes[0] as char).to_ascii_lowercase();
            return format!("/{drive}/{}", &normalized[3..]);
        }
        normalized.to_owned()
    }
    #[cfg(not(windows))]
    {
        normalized
    }
}

fn posix_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

fn fish_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_idempotent_shell_specific_path_commands() {
        let path = Path::new("/tmp/example");
        assert_eq!(
            init_line(Shell::Bash, path),
            "case \":$PATH:\" in *:'/tmp/example':*) ;; *) \
             export PATH='/tmp/example':\"$PATH\" ;; esac"
        );
        assert_eq!(
            init_line(Shell::Fish, path),
            "fish_add_path -- '/tmp/example'"
        );
    }

    #[test]
    fn quotes_shell_metacharacters_in_path_commands() {
        let path = Path::new("/tmp/it's $(not-code)");
        assert_eq!(
            init_line(Shell::Bash, path),
            "case \":$PATH:\" in *:'/tmp/it'\"'\"'s $(not-code)':*) ;; *) \
             export PATH='/tmp/it'\"'\"'s $(not-code)':\"$PATH\" ;; esac"
        );
        assert_eq!(
            init_line(Shell::Fish, path),
            r"fish_add_path -- '/tmp/it\'s $(not-code)'"
        );
        assert_eq!(
            init_line(Shell::Powershell, path),
            "$env:Path = '/tmp/it''s $(not-code);' + $env:Path"
        );
    }

    #[cfg(windows)]
    #[test]
    fn converts_windows_paths_for_posix_shells() {
        let path = Path::new(r"C:\Users\Example User\.avm\bin");
        assert_eq!(
            init_line(Shell::Bash, path),
            "case \":$PATH:\" in *:'/c/Users/Example User/.avm/bin':*) ;; *) \
             export PATH='/c/Users/Example User/.avm/bin':\"$PATH\" ;; esac"
        );
        assert_eq!(
            init_line(Shell::Powershell, path),
            r"$env:Path = 'C:\Users\Example User\.avm\bin;' + $env:Path"
        );
    }

    #[test]
    fn bash_setup_targets_interactive_and_login_profiles() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path();

        assert_eq!(
            shell_profiles_in(home, Shell::Bash).unwrap(),
            vec![home.join(".bashrc"), home.join(".bash_profile")]
        );

        fs::write(home.join(".profile"), "# existing profile\n").unwrap();
        assert_eq!(
            shell_profiles_in(home, Shell::Bash).unwrap(),
            vec![home.join(".bashrc"), home.join(".profile")]
        );

        fs::write(home.join(".bash_login"), "# bash login\n").unwrap();
        assert_eq!(
            shell_profiles_in(home, Shell::Bash).unwrap(),
            vec![home.join(".bashrc"), home.join(".bash_login")]
        );
    }

    #[test]
    fn shell_profile_update_is_idempotent_and_preserves_surrounding_text() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join(".bashrc");
        fs::write(&profile, "# before\nexport EXAMPLE=1\n").unwrap();

        update_shell_profiles(
            std::slice::from_ref(&profile),
            "export PATH='/first':\"$PATH\"",
        )
        .unwrap();
        update_shell_profiles(
            std::slice::from_ref(&profile),
            "export PATH='/second':\"$PATH\"",
        )
        .unwrap();

        let contents = fs::read_to_string(profile).unwrap();
        assert_eq!(contents.matches("# >>> avm setup >>>").count(), 1);
        assert_eq!(contents.matches("# <<< avm setup <<<").count(), 1);
        assert!(contents.starts_with("# before\nexport EXAMPLE=1\n"));
        assert!(!contents.contains("/first"));
        assert!(contents.contains("/second"));
    }

    #[test]
    fn shell_profile_update_refuses_duplicate_markers() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join(".zshrc");
        fs::write(
            &profile,
            "# >>> avm setup >>>\n# >>> avm setup >>>\n# <<< avm setup <<<\n",
        )
        .unwrap();

        let error = update_shell_profiles(
            std::slice::from_ref(&profile),
            "export PATH='/safe':\"$PATH\"",
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicate AVM markers"));
    }

    #[test]
    fn profile_edits_are_preflighted_before_any_write() {
        let temporary = tempfile::tempdir().unwrap();
        let safe = temporary.path().join(".bashrc");
        let unsafe_profile = temporary.path().join(".bash_profile");
        fs::write(&safe, "# unchanged\n").unwrap();
        fs::create_dir(&unsafe_profile).unwrap();

        let error = update_shell_profiles(
            &[safe.clone(), unsafe_profile],
            "export PATH='/safe':\"$PATH\"",
        )
        .unwrap_err();
        assert!(matches!(error, AvmError::UnsafePath(_)));
        assert_eq!(fs::read_to_string(safe).unwrap(), "# unchanged\n");
    }
}
