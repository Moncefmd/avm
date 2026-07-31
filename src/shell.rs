use std::fs;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command as ProcessCommand;

use crate::cli::Shell;
use crate::error::{AvmError, Result};
use serde::{Deserialize, Serialize};

use crate::atomic_file::{self, FileSnapshot, PlannedDelete, PlannedWrite};
use crate::profile::{self, BlockRemovalRequest, BlockRequest, ProfileFormat};
use crate::store::Store;

const PROFILE_BLOCK_START: &str = "# >>> avm init >>>";
const PROFILE_BLOCK_END: &str = "# <<< avm init <<<";
const LEGACY_PROFILE_BLOCK_START: &str = "# >>> avm setup >>>";
const LEGACY_PROFILE_BLOCK_END: &str = "# <<< avm setup <<<";
const INTEGRATION_SCHEMA: u32 = 1;
const MAX_INTEGRATION_METADATA_BYTES: u64 = 16 * 1024;

#[cfg(windows)]
const WINDOWS_PATH_ADD_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
function Test-EquivalentPath([string] $Entry, [string] $Target) {
    $raw = $Entry.TrimEnd('\', '/')
    $expanded = [Environment]::ExpandEnvironmentVariables($Entry).TrimEnd('\', '/')
    return $raw -ieq $Target.TrimEnd('\', '/') -or $expanded -ieq $Target.TrimEnd('\', '/')
}
function Publish-EnvironmentChange {
    if ($env:AVM_BROADCAST_ENVIRONMENT -ne '1') { return }
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class AvmEnvironmentBroadcast {
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr SendMessageTimeout(
        IntPtr hWnd, uint message, UIntPtr wParam, string lParam,
        uint flags, uint timeout, out UIntPtr result);
}
'@
    [UIntPtr] $result = [UIntPtr]::Zero
    [AvmEnvironmentBroadcast]::SendMessageTimeout(
        [IntPtr] 0xffff, 0x1a, [UIntPtr]::Zero, 'Environment', 0x2, 5000, [ref] $result
    ) | Out-Null
}
$target = $env:AVM_INIT_BIN
$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($env:AVM_ENVIRONMENT_KEY, $true)
if ($null -eq $key) { throw 'the current-user Environment registry key is unavailable' }
try {
    $options = [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
    $value = $key.GetValue('Path', $null, $options)
    if ($null -eq $value) {
        $current = ''
        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
    } else {
        if (-not ($value -is [string])) { throw 'the current-user Path value is not a string' }
        $current = [string] $value
        $kind = $key.GetValueKind('Path')
        if ($kind -notin @(
            [Microsoft.Win32.RegistryValueKind]::String,
            [Microsoft.Win32.RegistryValueKind]::ExpandString
        )) { throw "the current-user Path value has unsupported registry kind $kind" }
    }
    foreach ($entry in $current.Split([char] ';')) {
        if (Test-EquivalentPath $entry $target) {
            [Console]::Out.Write('present')
            exit 0
        }
    }
    $next = if ([string]::IsNullOrEmpty($current)) { $target } else { "$target;$current" }
    $key.SetValue('Path', $next, $kind)
} finally {
    $key.Dispose()
}
Publish-EnvironmentChange
[Console]::Out.Write('added')
"#;

#[cfg(windows)]
const WINDOWS_PATH_CONTAINS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
function Test-EquivalentPath([string] $Entry, [string] $Target) {
    $raw = $Entry.TrimEnd('\', '/')
    $expanded = [Environment]::ExpandEnvironmentVariables($Entry).TrimEnd('\', '/')
    return $raw -ieq $Target.TrimEnd('\', '/') -or $expanded -ieq $Target.TrimEnd('\', '/')
}
$target = $env:AVM_INIT_BIN
$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($env:AVM_ENVIRONMENT_KEY, $false)
if ($null -eq $key) { [Console]::Out.Write('absent'); exit 0 }
try {
    $options = [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
    $value = $key.GetValue('Path', $null, $options)
    if ($null -eq $value) { [Console]::Out.Write('absent'); exit 0 }
    if (-not ($value -is [string])) { throw 'the current-user Path value is not a string' }
    foreach ($entry in ([string] $value).Split([char] ';')) {
        if (Test-EquivalentPath $entry $target) {
            [Console]::Out.Write('present')
            exit 0
        }
    }
    [Console]::Out.Write('absent')
} finally {
    $key.Dispose()
}
"#;

#[cfg(windows)]
const WINDOWS_PATH_REMOVE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
function Test-RemovablePath([string] $Entry, [string] $Target) {
    if ($Entry.TrimEnd('\', '/') -ieq $Target.TrimEnd('\', '/')) { return $true }
    if ($env:AVM_REMOVE_EQUIVALENT_PATH -ne '1') { return $false }
    return [Environment]::ExpandEnvironmentVariables($Entry).TrimEnd('\', '/') -ieq $Target.TrimEnd('\', '/')
}
function Publish-EnvironmentChange {
    if ($env:AVM_BROADCAST_ENVIRONMENT -ne '1') { return }
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class AvmEnvironmentBroadcast {
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr SendMessageTimeout(
        IntPtr hWnd, uint message, UIntPtr wParam, string lParam,
        uint flags, uint timeout, out UIntPtr result);
}
'@
    [UIntPtr] $result = [UIntPtr]::Zero
    [AvmEnvironmentBroadcast]::SendMessageTimeout(
        [IntPtr] 0xffff, 0x1a, [UIntPtr]::Zero, 'Environment', 0x2, 5000, [ref] $result
    ) | Out-Null
}
$target = $env:AVM_INIT_BIN
$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($env:AVM_ENVIRONMENT_KEY, $true)
if ($null -eq $key) { [Console]::Out.Write('absent'); exit 0 }
$removed = $false
try {
    $options = [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
    $value = $key.GetValue('Path', $null, $options)
    if ($null -eq $value) { [Console]::Out.Write('absent'); exit 0 }
    if (-not ($value -is [string])) { throw 'the current-user Path value is not a string' }
    $kind = $key.GetValueKind('Path')
    if ($kind -notin @(
        [Microsoft.Win32.RegistryValueKind]::String,
        [Microsoft.Win32.RegistryValueKind]::ExpandString
    )) { throw "the current-user Path value has unsupported registry kind $kind" }
    $next = @(
        foreach ($entry in ([string] $value).Split([char] ';')) {
            if (-not $removed -and (Test-RemovablePath $entry $target)) {
                $removed = $true
            } else {
                $entry
            }
        }
    )
    if ($removed) { $key.SetValue('Path', ($next -join ';'), $kind) }
} finally {
    $key.Dispose()
}
if ($removed) {
    Publish-EnvironmentChange
    [Console]::Out.Write('removed')
} else {
    [Console]::Out.Write('absent')
}
"#;

#[derive(Debug, Deserialize, Serialize)]
struct IntegrationMetadata {
    schema: u32,
    windows_user_path_added: bool,
    #[serde(default)]
    windows_user_path_update_pending: bool,
}

#[derive(Debug)]
#[allow(dead_code)] // Cleanup variants are constructed on different target operating systems.
pub(crate) enum WindowsPathCleanup {
    NotApplicable,
    PreserveUnowned {
        receipt: Option<PlannedDelete>,
    },
    Remove {
        receipt: Option<PlannedDelete>,
        equivalent: bool,
    },
    RemoveReceiptOnly {
        receipt: PlannedDelete,
    },
}

pub(crate) struct IntegrationPlan {
    pub(crate) dispatcher: PathBuf,
    pub(crate) bin: PathBuf,
    pub(crate) profile_requests: Vec<BlockRequest>,
    windows_user_path: bool,
}

pub(crate) fn plan_integration(store: &Store, shell: Shell) -> Result<IntegrationPlan> {
    let line = init_line(shell, &store.paths.bin);
    let home = profile::home_dir()?;
    let windows_user_path = cfg!(windows) && shell == Shell::Powershell;
    let profiles = if windows_user_path {
        Vec::new()
    } else {
        shell_profiles_in(&home, shell)?
    };
    let profile_requests = profiles
        .into_iter()
        .map(|path| BlockRequest {
            path,
            start: PROFILE_BLOCK_START,
            end: PROFILE_BLOCK_END,
            legacy_markers: vec![(LEGACY_PROFILE_BLOCK_START, LEGACY_PROFILE_BLOCK_END)],
            command: line.clone(),
            format: if shell == Shell::Powershell {
                ProfileFormat::PowerShell
            } else {
                ProfileFormat::Utf8NoBom
            },
        })
        .collect();
    Ok(IntegrationPlan {
        dispatcher: store.shim_path(),
        bin: store.paths.bin.clone(),
        profile_requests,
        windows_user_path,
    })
}

pub(crate) fn preview(plan: &IntegrationPlan) {
    println!("Managed argocd command: {}", plan.dispatcher.display());
    if plan.windows_user_path {
        println!("User PATH: add {}", plan.bin.display());
    }
    for request in &plan.profile_requests {
        println!("PATH profile: {}", request.path.display());
        println!("{}", request.start);
        println!("{}", request.command);
        println!("{}", request.end);
    }
}

pub(crate) fn apply_integration_locked(store: &Store, plan: &IntegrationPlan) -> Result<()> {
    store.ensure_dispatcher_locked()?;
    if plan.windows_user_path {
        #[cfg(windows)]
        apply_windows_user_path_with_receipt(store, &plan.bin)?;
    }
    Ok(())
}

#[cfg(windows)]
fn apply_windows_user_path_with_receipt(store: &Store, bin: &Path) -> Result<()> {
    let previous = load_integration_metadata(store)?;
    let previously_owned = previous
        .as_ref()
        .is_some_and(IntegrationMetadata::owns_path);

    if windows_user_path_contains(bin)? {
        if previous
            .as_ref()
            .is_some_and(|metadata| metadata.windows_user_path_update_pending)
        {
            write_integration_metadata(store, &IntegrationMetadata::settled(previously_owned))?;
        }
        return Ok(());
    }

    // Persist intent before touching the process-external user PATH. If the process exits after
    // the PATH mutation but before finalization, cleanup still has a conservative ownership
    // receipt and can remove the entry safely.
    write_integration_metadata(
        store,
        &IntegrationMetadata {
            schema: INTEGRATION_SCHEMA,
            windows_user_path_added: previously_owned,
            windows_user_path_update_pending: true,
        },
    )?;

    match apply_windows_user_path(bin) {
        Ok(WindowsPathUpdate::Added) => {
            write_integration_metadata(store, &IntegrationMetadata::settled(true))
        }
        Ok(WindowsPathUpdate::Present) => {
            // Another actor added the same entry after our inspection. Do not claim it unless an
            // earlier AVM receipt already did.
            write_integration_metadata(store, &IntegrationMetadata::settled(previously_owned))
        }
        Err(error) => {
            recover_failed_windows_path_update(store, bin, previous.as_ref());
            Err(error)
        }
    }
}

impl IntegrationMetadata {
    fn settled(windows_user_path_added: bool) -> Self {
        Self {
            schema: INTEGRATION_SCHEMA,
            windows_user_path_added,
            windows_user_path_update_pending: false,
        }
    }

    fn owns_path(&self) -> bool {
        self.windows_user_path_added || self.windows_user_path_update_pending
    }
}

#[cfg(windows)]
fn recover_failed_windows_path_update(
    store: &Store,
    bin: &Path,
    previous: Option<&IntegrationMetadata>,
) {
    match windows_user_path_contains(bin) {
        Ok(true) => {
            // The external command may have committed the PATH change before reporting failure.
            // Finalize ownership when possible; otherwise leave the pending journal intact.
            if let Err(error) =
                write_integration_metadata(store, &IntegrationMetadata::settled(true))
            {
                eprintln!(
                    "warning: could not finalize the Windows PATH ownership receipt: {error}"
                );
            }
        }
        Ok(false) => {
            if let Err(error) = restore_integration_metadata(store, previous) {
                eprintln!(
                    "warning: could not roll back the pending Windows PATH ownership receipt: {error}"
                );
            }
        }
        Err(error) => eprintln!(
            "warning: could not verify the Windows user PATH after a failed update; the pending ownership receipt was preserved: {error}"
        ),
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowsPathUpdate {
    Added,
    Present,
}

#[cfg(windows)]
fn apply_windows_user_path(bin: &Path) -> Result<WindowsPathUpdate> {
    match powershell_user_path_command(bin, WINDOWS_PATH_ADD_SCRIPT, "update the user PATH")?
        .as_str()
    {
        "added" => Ok(WindowsPathUpdate::Added),
        "present" => Ok(WindowsPathUpdate::Present),
        other => Err(AvmError::Message(format!(
            "PowerShell returned an unexpected user PATH update result: {other:?}"
        ))),
    }
}

fn integration_metadata_path(store: &Store) -> PathBuf {
    store.paths.state.join("integration.json")
}

fn load_integration_metadata(store: &Store) -> Result<Option<IntegrationMetadata>> {
    let path = integration_metadata_path(store);
    let snapshot = atomic_file::read_snapshot(
        &path,
        MAX_INTEGRATION_METADATA_BYTES,
        "integration metadata",
    )?;
    parse_integration_metadata(&path, &snapshot)
}

fn parse_integration_metadata(
    path: &Path,
    snapshot: &FileSnapshot,
) -> Result<Option<IntegrationMetadata>> {
    let Some(bytes) = snapshot.bytes() else {
        return Ok(None);
    };
    let metadata: IntegrationMetadata = serde_json::from_slice(bytes).map_err(|_| {
        AvmError::Message(format!(
            "AVM integration metadata is invalid: {}",
            path.display()
        ))
    })?;
    if metadata.schema != INTEGRATION_SCHEMA {
        return Err(AvmError::Message(format!(
            "AVM integration metadata uses an unsupported schema: {}",
            path.display()
        )));
    }
    Ok(Some(metadata))
}

fn write_integration_metadata(store: &Store, metadata: &IntegrationMetadata) -> Result<()> {
    let path = integration_metadata_path(store);
    let snapshot = atomic_file::read_snapshot(
        &path,
        MAX_INTEGRATION_METADATA_BYTES,
        "integration metadata",
    )?;
    let _ = parse_integration_metadata(&path, &snapshot)?;
    let mut desired = serde_json::to_vec_pretty(metadata)
        .map_err(|error| AvmError::Message(format!("serialize integration metadata: {error}")))?;
    desired.push(b'\n');
    if snapshot.bytes() == Some(desired.as_slice()) {
        return Ok(());
    }
    atomic_file::apply(PlannedWrite::new(
        path,
        snapshot,
        desired,
        MAX_INTEGRATION_METADATA_BYTES,
        "integration metadata",
    ))
}

#[cfg(windows)]
fn restore_integration_metadata(
    store: &Store,
    previous: Option<&IntegrationMetadata>,
) -> Result<()> {
    if let Some(previous) = previous {
        return write_integration_metadata(store, previous);
    }

    let path = integration_metadata_path(store);
    let snapshot = atomic_file::read_snapshot(
        &path,
        MAX_INTEGRATION_METADATA_BYTES,
        "integration metadata",
    )?;
    let _ = parse_integration_metadata(&path, &snapshot)?;
    if matches!(snapshot, FileSnapshot::Missing) {
        return Ok(());
    }
    atomic_file::apply_delete(PlannedDelete::new(
        path,
        snapshot,
        MAX_INTEGRATION_METADATA_BYTES,
        "integration metadata",
    ))?;
    Ok(())
}

#[cfg(windows)]
fn windows_user_path_contains(bin: &Path) -> Result<bool> {
    let output =
        powershell_user_path_command(bin, WINDOWS_PATH_CONTAINS_SCRIPT, "inspect the user PATH")?;
    match output.as_str() {
        "present" => Ok(true),
        "absent" => Ok(false),
        _ => Err(AvmError::Message(format!(
            "PowerShell returned an unexpected user PATH inspection result: {output:?}"
        ))),
    }
}

#[cfg(windows)]
fn remove_windows_user_path(bin: &Path, equivalent: bool) -> Result<()> {
    let output = powershell_path_command_with_key(
        bin,
        WINDOWS_PATH_REMOVE_SCRIPT,
        "remove AVM from the user PATH",
        "Environment",
        true,
        equivalent,
    )?;
    if matches!(output.as_str(), "removed" | "absent") {
        Ok(())
    } else {
        Err(AvmError::Message(format!(
            "PowerShell returned an unexpected user PATH removal result: {output:?}"
        )))
    }
}

#[cfg(windows)]
fn powershell_user_path_command(bin: &Path, script: &str, action: &str) -> Result<String> {
    powershell_path_command_with_key(bin, script, action, "Environment", true, false)
}

#[cfg(windows)]
fn powershell_path_command_with_key(
    bin: &Path,
    script: &str,
    action: &str,
    environment_key: &str,
    broadcast: bool,
    remove_equivalent: bool,
) -> Result<String> {
    let output = ProcessCommand::new(crate::windows::trusted_powershell()?)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("AVM_INIT_BIN", bin)
        .env("AVM_ENVIRONMENT_KEY", environment_key)
        .env(
            "AVM_BROADCAST_ENVIRONMENT",
            if broadcast { "1" } else { "0" },
        )
        .env(
            "AVM_REMOVE_EQUIVALENT_PATH",
            if remove_equivalent { "1" } else { "0" },
        )
        .output()
        .map_err(|error| AvmError::io(format!("start PowerShell to {action}"), error))?;
    if !output.status.success() {
        return Err(AvmError::Message(format!(
            "PowerShell could not {action} (exit {}): {}",
            output.status.code().unwrap_or(1),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(crate) fn cleanup_profile_requests() -> Result<Vec<BlockRemovalRequest>> {
    let home = profile::home_dir()?;
    let powershell_profiles = profile::powershell_profiles()?;
    Ok(cleanup_profile_requests_in(&home, &powershell_profiles))
}

fn cleanup_profile_requests_in(
    home: &Path,
    powershell_profiles: &[PathBuf],
) -> Vec<BlockRemovalRequest> {
    let mut targets = vec![
        (home.join(".bashrc"), ProfileFormat::Utf8NoBom),
        (home.join(".bash_profile"), ProfileFormat::Utf8NoBom),
        (home.join(".bash_login"), ProfileFormat::Utf8NoBom),
        (home.join(".profile"), ProfileFormat::Utf8NoBom),
        (home.join(".zshrc"), ProfileFormat::Utf8NoBom),
        (
            home.join(".config").join("fish").join("config.fish"),
            ProfileFormat::Utf8NoBom,
        ),
    ];
    targets.extend(
        powershell_profiles
            .iter()
            .cloned()
            .map(|path| (path, ProfileFormat::PowerShell)),
    );
    targets.sort_by(|left, right| left.0.cmp(&right.0));
    targets.dedup_by(|left, right| left.0 == right.0);
    targets
        .into_iter()
        .map(|(path, format)| BlockRemovalRequest {
            path,
            markers: vec![
                (PROFILE_BLOCK_START, PROFILE_BLOCK_END),
                (LEGACY_PROFILE_BLOCK_START, LEGACY_PROFILE_BLOCK_END),
            ],
            format,
        })
        .collect()
}

pub(crate) fn plan_windows_path_cleanup(
    store: &Store,
    remove_without_receipt: bool,
) -> Result<WindowsPathCleanup> {
    #[cfg(not(windows))]
    {
        let _ = (store, remove_without_receipt);
        Ok(WindowsPathCleanup::NotApplicable)
    }
    #[cfg(windows)]
    {
        let receipt_path = integration_metadata_path(store);
        let receipt_snapshot = atomic_file::read_snapshot(
            &receipt_path,
            MAX_INTEGRATION_METADATA_BYTES,
            "integration metadata",
        )?;
        let metadata = parse_integration_metadata(&receipt_path, &receipt_snapshot)?;
        let owned = metadata
            .as_ref()
            .is_some_and(IntegrationMetadata::owns_path);
        let contains = windows_user_path_contains(&store.paths.bin)?;
        let receipt = metadata.map(|_| {
            PlannedDelete::new(
                receipt_path,
                receipt_snapshot,
                MAX_INTEGRATION_METADATA_BYTES,
                "integration metadata",
            )
        });
        if !contains {
            return Ok(match receipt {
                Some(receipt) => WindowsPathCleanup::RemoveReceiptOnly { receipt },
                None => WindowsPathCleanup::PreserveUnowned { receipt: None },
            });
        }
        if owned || remove_without_receipt {
            Ok(WindowsPathCleanup::Remove {
                receipt,
                equivalent: remove_without_receipt,
            })
        } else {
            Ok(WindowsPathCleanup::PreserveUnowned { receipt })
        }
    }
}

pub(crate) fn preview_windows_path_cleanup(plan: &WindowsPathCleanup, bin: &Path) {
    match plan {
        WindowsPathCleanup::NotApplicable => {}
        WindowsPathCleanup::PreserveUnowned { .. } => println!(
            "Windows user PATH: preserve {} (no AVM ownership receipt; pass --remove-path to remove one matching entry)",
            bin.display()
        ),
        WindowsPathCleanup::Remove { equivalent, .. } => {
            if *equivalent {
                println!(
                    "Windows user PATH: remove one entry equivalent to {}",
                    bin.display()
                )
            } else {
                println!("Windows user PATH: remove one {} entry", bin.display())
            }
        }
        WindowsPathCleanup::RemoveReceiptOnly { .. } => {
            println!("Windows user PATH: already clean; remove stale AVM ownership receipt")
        }
    }
}

pub(crate) fn preflight_windows_path_cleanup(plan: &WindowsPathCleanup) -> Result<()> {
    match plan {
        WindowsPathCleanup::Remove {
            receipt: Some(receipt),
            ..
        }
        | WindowsPathCleanup::PreserveUnowned {
            receipt: Some(receipt),
        }
        | WindowsPathCleanup::RemoveReceiptOnly { receipt } => {
            atomic_file::preflight_delete(receipt)
        }
        _ => Ok(()),
    }
}

pub(crate) fn apply_windows_path_cleanup(store: &Store, plan: WindowsPathCleanup) -> Result<()> {
    match plan {
        WindowsPathCleanup::NotApplicable => Ok(()),
        WindowsPathCleanup::PreserveUnowned { receipt } => {
            if let Some(receipt) = receipt {
                atomic_file::apply_delete(receipt)?;
            }
            Ok(())
        }
        WindowsPathCleanup::Remove {
            receipt,
            equivalent,
        } => {
            #[cfg(windows)]
            remove_windows_user_path(&store.paths.bin, equivalent)?;
            if let Some(receipt) = receipt {
                atomic_file::apply_delete(receipt)?;
            }
            Ok(())
        }
        WindowsPathCleanup::RemoveReceiptOnly { receipt } => {
            atomic_file::apply_delete(receipt)?;
            Ok(())
        }
    }
}

fn shell_profiles_in(home: &Path, shell: Shell) -> Result<Vec<PathBuf>> {
    match shell {
        Shell::Bash => {
            let login = bash_login_profile(home)?;
            Ok(vec![home.join(".bashrc"), login])
        }
        Shell::Zsh => Ok(vec![home.join(".zshrc")]),
        Shell::Fish => Ok(vec![home.join(".config").join("fish").join("config.fish")]),
        Shell::Powershell => profile::powershell_profiles(),
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
            let bin = fish_single_quote(&bin);
            format!(
                "if not contains -- {bin} $PATH\n\
                 set -gx PATH {bin} $PATH\n\
                 end"
            )
        }
        Shell::Powershell => {
            let bin = native_bin.replace('\'', "''");
            format!(
                "$__avmBin = '{bin}'\n\
                 try {{\n\
                     if (($env:Path -split [IO.Path]::PathSeparator) -notcontains $__avmBin) {{\n\
                         if ([string]::IsNullOrEmpty($env:Path)) {{\n\
                             $env:Path = $__avmBin\n\
                         }} else {{\n\
                             $env:Path = $__avmBin + [IO.Path]::PathSeparator + $env:Path\n\
                         }}\n\
                     }}\n\
                 }} finally {{\n\
                     Remove-Variable -Name __avmBin -ErrorAction SilentlyContinue\n\
                 }}"
            )
        }
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
            "if not contains -- '/tmp/example' $PATH\n\
             set -gx PATH '/tmp/example' $PATH\n\
             end"
        );
        let fish = init_line(Shell::Fish, path);
        assert!(!fish.contains("fish_add_path"));
        assert!(!fish.contains("fish_user_paths"));
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
            "if not contains -- '/tmp/it\\'s $(not-code)' $PATH\n\
             set -gx PATH '/tmp/it\\'s $(not-code)' $PATH\n\
             end"
        );
        let powershell = init_line(Shell::Powershell, path);
        assert!(powershell.contains("$__avmBin = '/tmp/it''s $(not-code)'"));
        assert!(powershell.contains("[IO.Path]::PathSeparator"));
        assert!(powershell.contains("-notcontains $__avmBin"));
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
        let powershell = init_line(Shell::Powershell, path);
        assert!(powershell.contains(r"$__avmBin = 'C:\Users\Example User\.avm\bin'"));
        assert!(powershell.contains("[IO.Path]::PathSeparator"));
    }

    #[test]
    fn bash_init_targets_interactive_and_login_profiles() {
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
    fn cleanup_scans_every_bash_login_candidate_and_powershell_family() {
        let home = Path::new("/home/example");
        let powershell = vec![
            PathBuf::from("C:/Profiles/WindowsPowerShell/profile.ps1"),
            PathBuf::from("C:/Profiles/PowerShell/profile.ps1"),
        ];
        let requests = cleanup_profile_requests_in(home, &powershell);
        let paths = requests
            .iter()
            .map(|request| request.path.clone())
            .collect::<Vec<_>>();

        for expected in [
            home.join(".bashrc"),
            home.join(".bash_profile"),
            home.join(".bash_login"),
            home.join(".profile"),
            home.join(".zshrc"),
            home.join(".config/fish/config.fish"),
            powershell[0].clone(),
            powershell[1].clone(),
        ] {
            assert!(paths.contains(&expected), "missing {}", expected.display());
        }
    }

    #[test]
    fn pending_windows_path_receipts_are_treated_as_owned() {
        let metadata = IntegrationMetadata {
            schema: INTEGRATION_SCHEMA,
            windows_user_path_added: false,
            windows_user_path_update_pending: true,
        };
        assert!(metadata.owns_path());
        assert!(!IntegrationMetadata::settled(false).owns_path());
        assert!(IntegrationMetadata::settled(true).owns_path());
    }

    #[test]
    fn legacy_windows_path_receipts_default_to_not_pending() {
        let snapshot =
            FileSnapshot::Present(br#"{"schema":1,"windows_user_path_added":true}"#.to_vec());
        let metadata = parse_integration_metadata(Path::new("integration.json"), &snapshot)
            .unwrap()
            .unwrap();
        assert!(metadata.windows_user_path_added);
        assert!(!metadata.windows_user_path_update_pending);
        assert!(metadata.owns_path());
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_updates_preserve_raw_expandable_values_and_registry_kind() {
        use std::ffi::OsString;
        use std::time::{SystemTime, UNIX_EPOCH};

        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, REG_EXPAND_SZ};
        use winreg::types::{FromRegValue, ToRegValue};

        let root = RegKey::predef(HKEY_CURRENT_USER);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let key_path = format!(r"Software\AVM-Path-Test-{}-{unique}", std::process::id());
        let original = r"%USERPROFILE%\avm-path-test;C:\Other";
        let target = Path::new(r"C:\AVM-Test\bin");
        let equivalent_target =
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap()).join("avm-path-test");

        let result = (|| -> std::result::Result<_, Box<dyn std::error::Error>> {
            let (key, _) = root.create_subkey(&key_path)?;
            let mut raw = original.to_reg_value();
            raw.vtype = REG_EXPAND_SZ;
            key.set_raw_value("Path", &raw)?;
            drop(key);

            let added = powershell_path_command_with_key(
                target,
                WINDOWS_PATH_ADD_SCRIPT,
                "test adding a raw user PATH entry",
                &key_path,
                false,
                false,
            )?;
            let key = root.open_subkey(&key_path)?;
            let after_add = key.get_raw_value("Path")?;
            let after_add_text = OsString::from_reg_value(&after_add)?;
            drop(key);

            let removed = powershell_path_command_with_key(
                target,
                WINDOWS_PATH_REMOVE_SCRIPT,
                "test removing a raw user PATH entry",
                &key_path,
                false,
                false,
            )?;
            let key = root.open_subkey(&key_path)?;
            let after_remove = key.get_raw_value("Path")?;
            let after_remove_text = OsString::from_reg_value(&after_remove)?;
            drop(key);

            let removed_equivalent = powershell_path_command_with_key(
                &equivalent_target,
                WINDOWS_PATH_REMOVE_SCRIPT,
                "test removing an explicitly requested equivalent user PATH entry",
                &key_path,
                false,
                true,
            )?;
            let key = root.open_subkey(&key_path)?;
            let after_equivalent_remove = key.get_raw_value("Path")?;
            let after_equivalent_remove_text = OsString::from_reg_value(&after_equivalent_remove)?;
            drop(key);

            Ok((
                added,
                after_add.vtype,
                after_add_text,
                removed,
                after_remove.vtype,
                after_remove_text,
                removed_equivalent,
                after_equivalent_remove.vtype,
                after_equivalent_remove_text,
            ))
        })();
        let _ = root.delete_subkey_all(&key_path);

        let (
            added,
            after_add_kind,
            after_add,
            removed,
            after_remove_kind,
            after_remove,
            removed_equivalent,
            after_equivalent_kind,
            after_equivalent_remove,
        ) = result.unwrap();
        assert_eq!(added, "added");
        assert_eq!(after_add_kind, REG_EXPAND_SZ);
        assert_eq!(
            after_add,
            OsString::from(format!(r"{};{original}", target.display()))
        );
        assert_eq!(removed, "removed");
        assert_eq!(after_remove_kind, REG_EXPAND_SZ);
        assert_eq!(after_remove, OsString::from(original));
        assert_eq!(removed_equivalent, "removed");
        assert_eq!(after_equivalent_kind, REG_EXPAND_SZ);
        assert_eq!(after_equivalent_remove, OsString::from(r"C:\Other"));
    }
}
