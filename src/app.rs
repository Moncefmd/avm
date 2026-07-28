use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command as ProcessCommand;

use clap::CommandFactory;
use directories::BaseDirs;
use serde::Serialize;
use tempfile::NamedTempFile;

use crate::cli::{Cli, Command, Shell};
use crate::error::{AvmError, Result};
use crate::github::{GitHubClient, Release, sanitized_url};
use crate::platform::Platform;
use crate::resolver::{
    Resolution, ResolutionSource, VERSION_ENV, remove_project_pin, resolve_environment_override,
    resolve_project_pin, resolve_version, write_project_pin,
};
use crate::shim;
use crate::store::{InstallMetadata, Paths, RELEASE_CACHE_TTL, Store, make_executable};
use crate::version::{self, Selector};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppOutcome {
    Success,
    ChildExit(i32),
}

const PROFILE_BLOCK_START: &str = "# >>> avm setup >>>";
const PROFILE_BLOCK_END: &str = "# <<< avm setup <<<";

pub fn run(cli: Cli) -> Result<AppOutcome> {
    let Cli { avm_home, command } = cli;
    let Some(command) = command else {
        let mut command = Cli::command();
        command
            .print_help()
            .map_err(|error| AvmError::io("print help", error))?;
        println!();
        return Ok(AppOutcome::Success);
    };
    let platform = Platform::current()?;
    let store = Store::new(Paths::resolve(avm_home)?, platform);

    let outcome = match command {
        Command::Install {
            selector,
            force,
            allow_unverified,
        } => {
            install(&store, selector.as_deref(), force, allow_unverified)?;
            AppOutcome::Success
        }
        Command::Setup { shell, dry_run } => {
            setup(&store, shell.unwrap_or_else(detected_shell), dry_run)?;
            AppOutcome::Success
        }
        Command::Default {
            selector,
            unset,
            force,
            allow_unverified,
        } => {
            if unset {
                unset_default(&store)?;
            } else {
                set_default(
                    &store,
                    selector.as_deref().ok_or_else(|| {
                        AvmError::Message("default requires a selector".to_owned())
                    })?,
                    force,
                    allow_unverified,
                )?;
            }
            AppOutcome::Success
        }
        Command::Pin {
            selector,
            unset,
            force,
            allow_unverified,
        } => {
            if unset {
                unset_pin()?;
            } else {
                pin(
                    &store,
                    selector
                        .as_deref()
                        .ok_or_else(|| AvmError::Message("pin requires a selector".to_owned()))?,
                    force,
                    allow_unverified,
                )?;
            }
            AppOutcome::Success
        }
        Command::Exec {
            selector,
            allow_unverified,
            argocd_args,
        } => AppOutcome::ChildExit(exec_argocd(
            &store,
            &selector,
            allow_unverified,
            argocd_args,
        )?),
        Command::List { json } => {
            list(&store, json)?;
            AppOutcome::Success
        }
        Command::Uninstall { version } => {
            uninstall(&store, &version)?;
            AppOutcome::Success
        }
        Command::Status { json } => {
            status(&store, json)?;
            AppOutcome::Success
        }
        Command::Available {
            query,
            prerelease,
            refresh,
            json,
        } => {
            available(&store, query.as_deref(), prerelease, refresh, json)?;
            AppOutcome::Success
        }
        Command::Info {
            selector,
            refresh,
            json,
        } => {
            info(&store, &selector, refresh, json)?;
            AppOutcome::Success
        }
        Command::Completion { shell, install } => {
            completion(shell, install)?;
            AppOutcome::Success
        }
        Command::Doctor { json } => {
            doctor(&store, json)?;
            AppOutcome::Success
        }
    };
    Ok(outcome)
}

fn install(
    store: &Store,
    selector: Option<&str>,
    force: bool,
    allow_unverified: bool,
) -> Result<()> {
    let requested = match selector {
        Some(selector) => selector.to_owned(),
        None => {
            let environment = std::env::var_os(VERSION_ENV);
            let cwd = current_directory()?;
            resolve_version(store, environment.as_deref(), &cwd)?
                .map(|resolution| resolution.version)
                .unwrap_or_else(|| version::STABLE.to_owned())
        }
    };
    let version = ensure_installed(store, &requested, force, allow_unverified, true)?;
    println!("Argo CD {version} is installed.");
    Ok(())
}

fn ensure_installed(
    store: &Store,
    requested: &str,
    force: bool,
    allow_unverified: bool,
    prefer_local_exact: bool,
) -> Result<String> {
    let selector = Selector::parse(requested)?;
    if prefer_local_exact
        && !force
        && let Selector::Exact(version) = &selector
    {
        let _version_lock = store.lock_version(version, true)?;
        if store.is_installed(version)? {
            let metadata = store
                .install_metadata(version)?
                .ok_or_else(|| AvmError::CorruptInstall(version.clone()))?;
            let actual = store.installed_sha256(version)?;
            if !actual.eq_ignore_ascii_case(&metadata.sha256) {
                return Err(AvmError::ChecksumMismatch {
                    asset: metadata.asset,
                    expected: metadata.sha256,
                    actual,
                });
            }
            eprintln!("Argo CD {version} is already installed; using the healthy local install.");
            return Ok(version.clone());
        }
    }

    let remote = GitHubClient::from_env()?;
    eprintln!("Resolving Argo CD {requested}...");
    let release = release_for_selector(store, &remote, &selector, false)?;
    let version = version::normalize(&release.tag_name)?;
    if !matches!(selector, Selector::Exact(_)) {
        eprintln!("Resolved {requested} to {version}.");
    }

    let asset_name = store.platform.asset_name();
    let asset = release
        .asset(&asset_name)
        .ok_or_else(|| AvmError::AssetNotFound {
            version: version.clone(),
            asset: asset_name.clone(),
        })?;
    let expected = remote.expected_checksum(&release, &asset_name)?;
    if expected.is_none() && !allow_unverified {
        return Err(AvmError::ChecksumMissing {
            version,
            asset: asset_name,
        });
    }

    let _version_lock = store.lock_version(&version, false)?;
    let recorded_metadata = match store.install_metadata(&version) {
        Ok(metadata) => metadata,
        Err(AvmError::CorruptInstall(_)) if force => None,
        Err(error) => return Err(error),
    };
    if let (Some(expected), Some(recorded)) = (expected.as_deref(), recorded_metadata.as_ref())
        && !recorded.sha256.eq_ignore_ascii_case(expected)
    {
        return Err(AvmError::ChecksumChanged {
            version,
            recorded: recorded.sha256.clone(),
            published: expected.to_owned(),
        });
    }
    let installed = store.is_installed(&version)?;
    if installed && !force {
        let recorded = recorded_metadata
            .as_ref()
            .ok_or_else(|| AvmError::CorruptInstall(version.clone()))?;
        let actual = store.installed_sha256(&version)?;
        if !actual.eq_ignore_ascii_case(&recorded.sha256) {
            return Err(AvmError::ChecksumMismatch {
                asset: asset_name,
                expected: recorded.sha256.clone(),
                actual,
            });
        }
        if expected.is_none() {
            eprintln!(
                "warning: the local digest for {version} matches its install record, but the \
                 publisher provides no checksum; `--allow-unverified` was supplied"
            );
        }
        eprintln!("Argo CD {version} is already installed; reusing it.");
        return Ok(version);
    }
    if !force && store.version_entry_exists(&version)? {
        return Err(AvmError::CorruptInstall(version));
    }

    let staging = store.staging_dir()?;
    let staged_binary = staging.path().join(store.platform.binary_name());
    eprintln!("Downloading {asset_name}...");
    let download = remote.download_to(&asset.browser_download_url, &staged_binary)?;

    if let Some(expected) = expected.as_deref()
        && !download.sha256.eq_ignore_ascii_case(expected)
    {
        return Err(AvmError::ChecksumMismatch {
            asset: asset_name,
            expected: expected.to_owned(),
            actual: download.sha256,
        });
    }
    if let Some(recorded) = recorded_metadata.as_ref()
        && !download.sha256.eq_ignore_ascii_case(&recorded.sha256)
    {
        return Err(AvmError::ChecksumChanged {
            version,
            recorded: recorded.sha256.clone(),
            published: download.sha256,
        });
    }
    make_executable(&staged_binary)?;

    let metadata = InstallMetadata::new(
        version.clone(),
        asset_name,
        download.sha256.clone(),
        expected.is_some(),
        sanitized_url(&asset.browser_download_url),
    );
    store.commit_install(&staging, &version, &metadata, force)?;
    eprintln!(
        "Installed Argo CD {version} ({} MiB, SHA-256 {}).",
        download.bytes / (1024 * 1024),
        download.sha256
    );
    Ok(version)
}

fn set_default(store: &Store, selector: &str, force: bool, allow_unverified: bool) -> Result<()> {
    let version = ensure_installed(store, selector, force, allow_unverified, true)?;
    store.set_default(&version)?;
    println!("Default Argo CD version is now {version}.");
    warn_if_path_missing(store);
    Ok(())
}

fn unset_default(store: &Store) -> Result<()> {
    let Some(version) = store.default_version()? else {
        println!("No default Argo CD version is set.");
        return Ok(());
    };
    if store.clear_default(&version)? {
        println!("Removed the default Argo CD version.");
        Ok(())
    } else {
        Err(AvmError::Message(
            "the default changed concurrently; run `avm default --unset` again".to_owned(),
        ))
    }
}

fn pin(store: &Store, selector: &str, force: bool, allow_unverified: bool) -> Result<()> {
    let version = ensure_installed(store, selector, force, allow_unverified, true)?;
    let _version_lock = store.lock_version(&version, true)?;
    if !store.is_installed(&version)? {
        return Err(AvmError::NotInstalled(version));
    }
    store.ensure_dispatcher()?;
    let cwd = current_directory()?;
    let path = write_project_pin(&cwd, &version)?;
    println!("Pinned Argo CD {version} in {}.", path.display());
    warn_if_path_missing(store);
    Ok(())
}

fn unset_pin() -> Result<()> {
    let cwd = current_directory()?;
    match remove_project_pin(&cwd)? {
        Some(path) => println!("Removed the project pin at {}.", path.display()),
        None => println!("No project pin is set in {}.", cwd.display()),
    }
    Ok(())
}

fn exec_argocd(
    store: &Store,
    selector: &str,
    allow_unverified: bool,
    arguments: Vec<OsString>,
) -> Result<i32> {
    let version = ensure_installed(store, selector, false, allow_unverified, true)?;
    shim::execute(store, &version, arguments)
}

fn current_directory() -> Result<PathBuf> {
    std::env::current_dir().map_err(|error| AvmError::io("determine the current directory", error))
}

fn list(store: &Store, json: bool) -> Result<()> {
    let installed = store.installed_versions()?;
    let environment = std::env::var_os(VERSION_ENV);
    let cwd = current_directory()?;
    let effective =
        resolve_version(store, environment.as_deref(), &cwd)?.map(|resolution| resolution.version);
    if json {
        let output = serde_json::json!({
            "schema": 1,
            "effective_version": effective,
            "versions": installed,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .map_err(|error| AvmError::Message(error.to_string()))?
        );
        return Ok(());
    }
    if installed.is_empty() {
        println!("No versions installed yet.");
        return Ok(());
    }

    println!("Installed versions:");
    for item in installed {
        let mut labels = Vec::new();
        if effective.as_deref() == Some(item.version.as_str()) {
            labels.push("effective");
        }
        if item.is_default {
            labels.push("default");
        }
        if !item.healthy {
            labels.push("corrupt");
        }
        let marker = if effective.as_deref() == Some(item.version.as_str()) {
            ">"
        } else {
            " "
        };
        if labels.is_empty() {
            println!("{marker} {}", item.version);
        } else {
            println!("{marker} {} ({})", item.version, labels.join(", "));
        }
    }
    Ok(())
}

fn release_for_selector(
    store: &Store,
    remote: &GitHubClient,
    selector: &Selector,
    refresh: bool,
) -> Result<Release> {
    match selector {
        Selector::LatestStable => available_releases(store, remote, false, refresh)?
            .into_iter()
            .max_by_key(Release::parsed_version)
            .ok_or_else(|| AvmError::ReleaseNotFound(version::STABLE.to_owned())),
        Selector::Exact(version) => remote.release(version),
        Selector::Major { major } => available_releases(store, remote, false, refresh)?
            .into_iter()
            .filter(|release| {
                release
                    .parsed_version()
                    .is_some_and(|version| version.major == *major)
            })
            .max_by_key(Release::parsed_version)
            .ok_or_else(|| AvmError::ReleaseNotFound(format!("v{major}.x"))),
        Selector::Minor { major, minor } => available_releases(store, remote, false, refresh)?
            .into_iter()
            .filter(|release| {
                release
                    .parsed_version()
                    .is_some_and(|version| version.major == *major && version.minor == *minor)
            })
            .max_by_key(Release::parsed_version)
            .ok_or_else(|| AvmError::ReleaseNotFound(format!("v{major}.{minor}.x"))),
    }
}

fn available_releases(
    store: &Store,
    remote: &GitHubClient,
    include_prerelease: bool,
    refresh: bool,
) -> Result<Vec<Release>> {
    let mut cache_is_invalid = false;
    let cached = if refresh {
        None
    } else {
        match store.load_release_cache(remote.source(), Some(RELEASE_CACHE_TTL)) {
            Ok(cached) => cached,
            Err(AvmError::InvalidCache(reason)) => {
                eprintln!("warning: ignoring invalid release cache: {reason}");
                cache_is_invalid = true;
                None
            }
            Err(error) => return Err(error),
        }
    };
    let mut releases = match cached {
        Some(releases) => releases,
        None => match remote.releases() {
            Ok(releases) => {
                if let Err(error) = store.write_release_cache(remote.source(), &releases) {
                    eprintln!("warning: could not update release cache: {error}");
                }
                releases
            }
            Err(error) => {
                let stale = if cache_is_invalid {
                    None
                } else {
                    match store.load_release_cache(remote.source(), None) {
                        Ok(stale) => stale,
                        Err(AvmError::InvalidCache(reason)) => {
                            eprintln!("warning: ignoring invalid release cache: {reason}");
                            None
                        }
                        Err(cache_error) => return Err(cache_error),
                    }
                };
                if let Some(stale) = stale {
                    eprintln!("warning: {error}; using stale cached release metadata");
                    stale
                } else {
                    return Err(error);
                }
            }
        },
    };
    releases.retain(|release| {
        !release.draft
            && (include_prerelease || !release.prerelease)
            && release.parsed_version().is_some()
    });
    releases.sort_by_key(|release| std::cmp::Reverse(release.parsed_version()));
    Ok(releases)
}

fn uninstall(store: &Store, requested: &str) -> Result<()> {
    let version = canonical_version(requested)?;
    let _version_lock = store.lock_version(&version, false)?;
    if !store.version_entry_exists(&version)? {
        return Err(AvmError::NotInstalled(version));
    }
    if store.default_version()?.as_deref() == Some(version.as_str()) {
        return Err(AvmError::Message(format!(
            "{version} is the default; choose another default or run `avm default --unset` first"
        )));
    }
    let cwd = current_directory()?;
    if let Some(pin) = resolve_project_pin(&cwd)?
        && pin.version == version
    {
        let path = match pin.source {
            ResolutionSource::ProjectPin { path } => path,
            _ => unreachable!("project pin resolution must contain its path"),
        };
        return Err(AvmError::Message(format!(
            "{version} is pinned by {}; change or remove that pin first",
            path.display()
        )));
    }
    store.remove_version(&version)?;
    println!("Uninstalled Argo CD {version}.");
    Ok(())
}

fn status(store: &Store, json: bool) -> Result<()> {
    let environment = std::env::var_os(VERSION_ENV);
    let cwd = current_directory()?;
    let (default_version, default_state_error) = match store.default_version() {
        Ok(version) => (version, None),
        Err(AvmError::CorruptState(value)) => (None, Some(format!("invalid default {value:?}"))),
        Err(error) => return Err(error),
    };
    let resolution = match resolve_environment_override(environment.as_deref())? {
        Some(resolution) => Some(resolution),
        None => match resolve_project_pin(&cwd)? {
            Some(resolution) => Some(resolution),
            None => default_version.clone().map(|version| Resolution {
                version,
                source: ResolutionSource::GlobalDefault,
            }),
        },
    };

    let mut report = StatusReport {
        schema: 1,
        effective_version: None,
        effective_source: None,
        source_path: None,
        default_version,
        default_state_error,
        binary_path: None,
        installed: false,
        healthy: false,
        verified: false,
    };
    if let Some(resolution) = resolution {
        let (source, source_path) = status_source(&resolution);
        let version = resolution.version;
        let binary = store.version_binary(&version)?;
        let installed = match store.version_entry_exists(&version) {
            Ok(installed) => installed,
            Err(AvmError::UnsafePath(_)) => true,
            Err(error) => return Err(error),
        };
        let audit = audit_installed_version(store, &version)?;
        report.effective_version = Some(version);
        report.effective_source = Some(source);
        report.source_path = source_path;
        report.binary_path = Some(binary.display().to_string());
        report.installed = installed;
        report.healthy = audit.healthy;
        report.verified = audit.verified;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| AvmError::Message(error.to_string()))?
        );
        return Ok(());
    }

    println!(
        "Version:        {}",
        report.effective_version.as_deref().unwrap_or("none")
    );
    println!(
        "Source:         {}",
        report.effective_source.as_deref().unwrap_or("none")
    );
    if let Some(path) = report.source_path.as_deref() {
        println!("Source path:    {path}");
    }
    println!(
        "Default:        {}",
        report.default_version.as_deref().unwrap_or("none")
    );
    if let Some(error) = report.default_state_error.as_deref() {
        println!("Default error:  {error}");
    }
    println!(
        "Binary:         {}",
        report.binary_path.as_deref().unwrap_or("n/a")
    );
    println!("Installed:      {}", yes_no(report.installed));
    println!("Healthy:        {}", yes_no(report.healthy));
    println!("Verified:       {}", yes_no(report.verified));
    Ok(())
}

fn status_source(resolution: &Resolution) -> (String, Option<String>) {
    match &resolution.source {
        ResolutionSource::Environment => ("environment".to_owned(), None),
        ResolutionSource::ProjectPin { path } => {
            ("project".to_owned(), Some(path.display().to_string()))
        }
        ResolutionSource::GlobalDefault => ("default".to_owned(), None),
    }
}

fn available(
    store: &Store,
    query: Option<&str>,
    include_prerelease: bool,
    refresh: bool,
    json: bool,
) -> Result<()> {
    let remote = GitHubClient::from_env()?;
    let mut releases = available_releases(store, &remote, include_prerelease, refresh)?;
    if let Some(query) = query {
        releases.retain(|release| release_matches_query(release, query));
    }

    if json {
        let output = serde_json::json!({
            "schema": 1,
            "query": query,
            "releases": releases,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .map_err(|error| AvmError::Message(error.to_string()))?
        );
    } else {
        println!("Matching Argo CD releases:");
        for release in releases {
            println!("{}", release.tag_name);
        }
    }
    Ok(())
}

fn release_matches_query(release: &Release, query: &str) -> bool {
    match Selector::parse(query) {
        Ok(Selector::LatestStable) => !release.prerelease,
        Ok(Selector::Major { major }) => release
            .parsed_version()
            .is_some_and(|version| version.major == major),
        Ok(Selector::Minor { major, minor }) => release
            .parsed_version()
            .is_some_and(|version| version.major == major && version.minor == minor),
        Ok(Selector::Exact(version)) => release.tag_name == version,
        Err(_) => release
            .tag_name
            .to_ascii_lowercase()
            .contains(&query.to_ascii_lowercase()),
    }
}

fn info(store: &Store, requested: &str, refresh: bool, json: bool) -> Result<()> {
    let selector = Selector::parse(requested)?;
    let remote = GitHubClient::from_env()?;
    let release = release_for_selector(store, &remote, &selector, refresh)?;
    if json {
        let output = serde_json::json!({
            "schema": 1,
            "selector": requested,
            "release": release,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .map_err(|error| AvmError::Message(error.to_string()))?
        );
    } else {
        println!("Argo CD release {}:", release.tag_name);
        for asset in release.assets {
            println!("  {}", asset.name);
        }
    }
    Ok(())
}

fn setup(store: &Store, shell: Shell, dry_run: bool) -> Result<()> {
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
            let profile = shell_profile(shell)?;
            println!("Profile: {}", profile.display());
            println!("{PROFILE_BLOCK_START}");
            println!("{line}");
            println!("{PROFILE_BLOCK_END}");
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

    let profile = shell_profile(shell)?;
    update_shell_profile(&profile, &line)?;
    println!("Configured {shell} in {}.", profile.display());
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

fn shell_profile(shell: Shell) -> Result<PathBuf> {
    let home = BaseDirs::new()
        .ok_or_else(|| AvmError::Message("could not determine the home directory".to_owned()))?
        .home_dir()
        .to_path_buf();
    match shell {
        Shell::Bash => Ok(home.join(".bashrc")),
        Shell::Zsh => Ok(home.join(".zshrc")),
        Shell::Fish => Ok(home.join(".config").join("fish").join("config.fish")),
        Shell::Powershell => Err(AvmError::CompletionInstallUnsupported(
            "automatic PowerShell profile selection".to_owned(),
        )),
    }
}

fn update_shell_profile(path: &Path, command: &str) -> Result<()> {
    const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

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
        return Ok(());
    }

    let parent = path
        .parent()
        .ok_or_else(|| AvmError::UnsafePath(path.to_path_buf()))?;
    fs::create_dir_all(parent)
        .map_err(|error| AvmError::io(format!("create {}", parent.display()), error))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
        AvmError::io(
            format!("create temporary file in {}", parent.display()),
            error,
        )
    })?;
    if let Some(permissions) = permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| {
                AvmError::io(format!("set permissions for {}", path.display()), error)
            })?;
    }
    std::io::Write::write_all(&mut temporary, updated.as_bytes())
        .map_err(|error| AvmError::io(format!("write temporary {}", path.display()), error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| AvmError::io(format!("sync temporary {}", path.display()), error))?;
    temporary
        .persist(path)
        .map_err(|error| AvmError::io(format!("replace {}", path.display()), error.error))?;
    Ok(())
}

fn completion(shell: Shell, install: bool) -> Result<()> {
    if !install {
        write_dynamic_completion(shell, &mut io::stdout())?;
        return Ok(());
    }

    if shell == Shell::Powershell {
        println!(
            "PowerShell completion installation is shell-profile specific.\n\
             Run `avm completion powershell > avm.ps1` and dot-source it from `$PROFILE`."
        );
        return Ok(());
    }

    let home = BaseDirs::new()
        .ok_or_else(|| AvmError::Message("could not determine the home directory".to_owned()))?
        .home_dir()
        .to_path_buf();
    let destination = completion_path(shell, &home)?;
    let mut bytes = Vec::new();
    write_dynamic_completion(shell, &mut bytes)?;
    write_completion_atomic(&destination, &bytes)?;
    println!("Installed {shell} completion at {}.", destination.display());
    if shell == Shell::Zsh {
        println!("Ensure ~/.zsh/completions is present in your zsh fpath.");
    }
    Ok(())
}

fn write_dynamic_completion(shell: Shell, output: &mut dyn io::Write) -> Result<()> {
    use clap_complete::env::{Bash, EnvCompleter, Fish, Powershell, Zsh};

    let completer: &dyn EnvCompleter = match shell {
        Shell::Bash => &Bash,
        Shell::Zsh => &Zsh,
        Shell::Fish => &Fish,
        Shell::Powershell => &Powershell,
    };
    completer
        .write_registration("COMPLETE", "avm", "avm", "avm", output)
        .map_err(|error| AvmError::io(format!("generate {shell} completion"), error))
}

fn doctor(store: &Store, json: bool) -> Result<()> {
    let installed = store.installed_versions()?;
    let mut install_audits = Vec::with_capacity(installed.len());
    for item in &installed {
        install_audits.push((
            item.version.clone(),
            audit_installed_version(store, &item.version)?,
        ));
    }
    let (default_version, default_state_error) = match store.default_version() {
        Ok(default_version) => (default_version, None),
        Err(AvmError::CorruptState(value)) => (
            None,
            Some(format!("invalid default-version value {value:?}")),
        ),
        Err(error) => return Err(error),
    };
    let cwd = current_directory()?;
    let environment = std::env::var_os(VERSION_ENV);
    let (environment_resolution, environment_error) =
        match resolve_environment_override(environment.as_deref()) {
            Ok(resolution) => (resolution, None),
            Err(error) => (None, Some(error.to_string())),
        };
    let (project_resolution, project_error) = match resolve_project_pin(&cwd) {
        Ok(resolution) => (resolution, None),
        Err(error) => (None, Some(error.to_string())),
    };
    let (resolution, selection_error) = if let Some(error) = environment_error.as_ref() {
        (None, Some(error.clone()))
    } else if let Some(resolution) = environment_resolution {
        (Some(resolution), None)
    } else if let Some(error) = project_error.as_ref() {
        (None, Some(error.clone()))
    } else if let Some(resolution) = project_resolution.clone() {
        (Some(resolution), None)
    } else {
        (
            default_version.clone().map(|version| Resolution {
                version,
                source: ResolutionSource::GlobalDefault,
            }),
            default_state_error.clone(),
        )
    };

    let on_path = store.is_bin_on_path();
    let (effective_version, effective_source, source_path) = match resolution.as_ref() {
        Some(resolution) => {
            let (source, path) = status_source(resolution);
            (Some(resolution.version.clone()), Some(source), path)
        }
        None => (None, None, None),
    };
    let (project_version, project_path) = match project_resolution.as_ref() {
        Some(resolution) => {
            let path = match &resolution.source {
                ResolutionSource::ProjectPin { path } => Some(path.display().to_string()),
                _ => None,
            };
            (Some(resolution.version.clone()), path)
        }
        None => (None, None),
    };
    let (effective_healthy, effective_verified) = match effective_version.as_deref() {
        None => (None, None),
        Some(version) => {
            let audit = audit_from_catalog(store, &install_audits, version)?;
            (Some(audit.healthy), Some(audit.verified))
        }
    };
    let (project_healthy, project_verified) = match project_version.as_deref() {
        None => (None, None),
        Some(version) => {
            let audit = audit_from_catalog(store, &install_audits, version)?;
            (Some(audit.healthy), Some(audit.verified))
        }
    };
    let (default_healthy, default_verified) = match default_version.as_deref() {
        None => (None, None),
        Some(version) => {
            let audit = audit_from_catalog(store, &install_audits, version)?;
            (Some(audit.healthy), Some(audit.verified))
        }
    };
    let healthy_versions = install_audits
        .iter()
        .filter(|(_, audit)| audit.healthy)
        .count();
    let corrupt_versions = installed.len() - healthy_versions;
    let report = DoctorReport {
        schema: 1,
        avm_home: store.paths.root.display().to_string(),
        bin_directory: store.paths.bin.display().to_string(),
        bin_on_path: on_path,
        platform: format!("{}/{}", store.platform.os, store.platform.arch),
        release_asset: store.platform.asset_name(),
        effective_version,
        effective_source,
        source_path,
        selection_error,
        effective_healthy,
        effective_verified,
        project_version,
        project_path,
        project_error,
        project_healthy,
        project_verified,
        default_version,
        default_state_error,
        default_healthy,
        default_verified,
        dispatcher_healthy: store.shim_is_healthy()?,
        installed_versions: healthy_versions,
        corrupt_versions,
    };
    let check_failed = report.selection_error.is_some()
        || report.effective_version.is_none()
        || report.effective_healthy != Some(true)
        || report.project_error.is_some()
        || report.project_version.is_some() && report.project_healthy != Some(true)
        || report.default_state_error.is_some()
        || report.default_version.is_some() && report.default_healthy != Some(true)
        || !report.dispatcher_healthy
        || report.corrupt_versions > 0
        || !report.bin_on_path;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| AvmError::Message(error.to_string()))?
        );
    } else {
        println!("AVM home:        {}", report.avm_home);
        println!("Platform:        {}", report.platform);
        println!("Release asset:   {}", report.release_asset);
        println!("Installed:       {}", report.installed_versions);
        println!("Corrupt entries: {}", report.corrupt_versions);
        println!(
            "Effective:       {}",
            report.effective_version.as_deref().unwrap_or("none")
        );
        println!(
            "Source:          {}",
            report.effective_source.as_deref().unwrap_or("none")
        );
        if let Some(path) = report.source_path.as_deref() {
            println!("Source path:     {path}");
        }
        if let Some(error) = report.selection_error.as_deref() {
            println!("Selection error: {error}");
        }
        println!(
            "Project pin:     {}",
            report.project_version.as_deref().unwrap_or("none")
        );
        if let Some(path) = report.project_path.as_deref() {
            println!("Project path:    {path}");
        }
        if let Some(error) = report.project_error.as_deref() {
            println!("Project error:   {error}");
        }
        println!(
            "Project good:    {}",
            optional_yes_no(report.project_healthy)
        );
        println!(
            "Project verified: {}",
            optional_yes_no(report.project_verified)
        );
        println!(
            "Default:         {}",
            report.default_version.as_deref().unwrap_or("none")
        );
        if let Some(error) = report.default_state_error.as_deref() {
            println!("Default error:   {error}");
        }
        println!(
            "Default good:    {}",
            optional_yes_no(report.default_healthy)
        );
        println!(
            "Default verified: {}",
            optional_yes_no(report.default_verified)
        );
        println!(
            "Effective good:  {}",
            optional_yes_no(report.effective_healthy)
        );
        println!(
            "Verified:        {}",
            optional_yes_no(report.effective_verified)
        );
        println!("Dispatcher good:  {}", yes_no(report.dispatcher_healthy));
        println!("Bin directory:   {}", report.bin_directory);
        println!("Bin is on PATH:  {}", yes_no(report.bin_on_path));
    }
    if check_failed {
        return Err(AvmError::HealthCheckFailed);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct InstallAudit {
    healthy: bool,
    verified: bool,
}

fn audit_installed_version(store: &Store, version: &str) -> Result<InstallAudit> {
    let (_binary, _lock) = match store.binary_for(version) {
        Ok(guarded) => guarded,
        Err(
            AvmError::NotInstalled(_)
            | AvmError::ExecutionLockMissing(_)
            | AvmError::CorruptInstall(_)
            | AvmError::UnsafePath(_),
        ) => {
            return Ok(InstallAudit {
                healthy: false,
                verified: false,
            });
        }
        Err(error) => return Err(error),
    };
    let Some(metadata) = store.install_metadata(version)? else {
        return Ok(InstallAudit {
            healthy: false,
            verified: false,
        });
    };
    let actual = store.installed_sha256(version)?;
    let digest_matches = actual.eq_ignore_ascii_case(&metadata.sha256);
    Ok(InstallAudit {
        healthy: digest_matches,
        verified: metadata.verified && digest_matches,
    })
}

fn audit_from_catalog(
    store: &Store,
    audits: &[(String, InstallAudit)],
    version: &str,
) -> Result<InstallAudit> {
    match audits.iter().find(|(installed, _)| installed == version) {
        Some((_, audit)) => Ok(*audit),
        None => audit_installed_version(store, version),
    }
}

fn canonical_version(requested: &str) -> Result<String> {
    version::normalize(requested)
}

fn warn_if_path_missing(store: &Store) {
    if !store.is_bin_on_path() {
        eprintln!(
            "warning: {} is not in PATH\n         {}",
            store.paths.bin.display(),
            init_line(detected_shell(), &store.paths.bin)
        );
    }
}

fn detected_shell() -> Shell {
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

fn init_line(shell: Shell, bin: &Path) -> String {
    let bin = bin.to_string_lossy();
    match shell {
        Shell::Bash | Shell::Zsh => {
            format!("export PATH={}:\"$PATH\"", posix_single_quote(&bin))
        }
        Shell::Fish => format!("fish_add_path -- {}", fish_single_quote(&bin)),
        Shell::Powershell => format!("$env:Path = '{};' + $env:Path", bin.replace('\'', "''")),
    }
}

fn posix_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

fn fish_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'"))
}

fn completion_path(shell: Shell, home: &Path) -> Result<PathBuf> {
    match shell {
        Shell::Bash => Ok(home
            .join(".local")
            .join("share")
            .join("bash-completion")
            .join("completions")
            .join("avm")),
        Shell::Zsh => Ok(home.join(".zsh").join("completions").join("_avm")),
        Shell::Fish => Ok(home
            .join(".config")
            .join("fish")
            .join("completions")
            .join("avm.fish")),
        Shell::Powershell => Err(AvmError::CompletionInstallUnsupported(
            "powershell".to_owned(),
        )),
    }
}

fn write_completion_atomic(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| AvmError::UnsafePath(destination.to_path_buf()))?;
    fs::create_dir_all(parent)
        .map_err(|error| AvmError::io(format!("create {}", parent.display()), error))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| AvmError::io("create temporary completion file", error))?;
    std::io::Write::write_all(&mut temporary, bytes)
        .map_err(|error| AvmError::io("write completion file", error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| AvmError::io("sync completion file", error))?;
    temporary.persist(destination).map_err(|error| {
        AvmError::io(
            format!("replace completion {}", destination.display()),
            error.error,
        )
    })?;
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn optional_yes_no(value: Option<bool>) -> &'static str {
    value.map(yes_no).unwrap_or("n/a")
}

#[derive(Serialize)]
struct StatusReport {
    schema: u8,
    effective_version: Option<String>,
    effective_source: Option<String>,
    source_path: Option<String>,
    default_version: Option<String>,
    default_state_error: Option<String>,
    binary_path: Option<String>,
    installed: bool,
    healthy: bool,
    verified: bool,
}

#[derive(Serialize)]
struct DoctorReport {
    schema: u8,
    avm_home: String,
    bin_directory: String,
    bin_on_path: bool,
    platform: String,
    release_asset: String,
    effective_version: Option<String>,
    effective_source: Option<String>,
    source_path: Option<String>,
    selection_error: Option<String>,
    effective_healthy: Option<bool>,
    effective_verified: Option<bool>,
    project_version: Option<String>,
    project_path: Option<String>,
    project_error: Option<String>,
    project_healthy: Option<bool>,
    project_verified: Option<bool>,
    default_version: Option<String>,
    default_state_error: Option<String>,
    default_healthy: Option<bool>,
    default_verified: Option<bool>,
    dispatcher_healthy: bool,
    installed_versions: usize,
    corrupt_versions: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_shell_specific_path_commands() {
        let path = Path::new("/tmp/example");
        assert_eq!(
            init_line(Shell::Bash, path),
            "export PATH='/tmp/example':\"$PATH\""
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
            "export PATH='/tmp/it'\"'\"'s $(not-code)':\"$PATH\""
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

    #[test]
    fn preserves_existing_completion_locations() {
        let home = Path::new("/home/user");
        assert_eq!(
            completion_path(Shell::Zsh, home).unwrap(),
            home.join(".zsh/completions/_avm")
        );
        assert_eq!(
            completion_path(Shell::Fish, home).unwrap(),
            home.join(".config/fish/completions/avm.fish")
        );
    }

    #[test]
    fn shell_profile_update_is_idempotent_and_preserves_surrounding_text() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join(".bashrc");
        fs::write(&profile, "# before\nexport EXAMPLE=1\n").unwrap();

        update_shell_profile(&profile, "export PATH='/first':\"$PATH\"").unwrap();
        update_shell_profile(&profile, "export PATH='/second':\"$PATH\"").unwrap();

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

        let error = update_shell_profile(&profile, "export PATH='/safe':\"$PATH\"").unwrap_err();
        assert!(error.to_string().contains("duplicate AVM markers"));
    }
}
