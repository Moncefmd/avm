use std::ffi::OsString;
use std::path::PathBuf;

use clap::CommandFactory;
use serde::Serialize;

use crate::catalog;
use crate::cli::{Cli, Command};
use crate::completion;
use crate::error::{AvmError, Result};
use crate::github::GitHubClient;
use crate::installer;
use crate::onboarding;
use crate::platform::Platform;
use crate::resolver::{
    Resolution, ResolutionSource, VERSION_ENV, remove_project_pin, resolve_environment_override,
    resolve_project_pin, resolve_version, write_project_pin,
};
use crate::shell;
use crate::shim;
use crate::store::{Paths, Store};
use crate::version::{self, Selector};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppOutcome {
    Success,
    ChildExit(i32),
}

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
            installer::install(&store, selector.as_deref(), force, allow_unverified)?;
            AppOutcome::Success
        }
        Command::Init {
            shell,
            dry_run,
            no_completion,
        } => {
            onboarding::init(
                &store,
                shell.unwrap_or_else(shell::detected_shell),
                !no_completion,
                dry_run,
            )?;
            AppOutcome::Success
        }
        Command::Uninit {
            dry_run,
            remove_path,
        } => {
            onboarding::uninit(&store, dry_run, remove_path)?;
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
        Command::Completion {
            shell,
            install,
            dry_run,
        } => {
            completion::command(&store, shell, install, dry_run)?;
            AppOutcome::Success
        }
        Command::Doctor { json } => {
            doctor(&store, json)?;
            AppOutcome::Success
        }
        Command::DispatchV1 { argocd_args } => {
            AppOutcome::ChildExit(shim::dispatch(&store, argocd_args)?)
        }
    };
    Ok(outcome)
}

fn set_default(store: &Store, selector: &str, force: bool, allow_unverified: bool) -> Result<()> {
    let version = installer::ensure_installed(store, selector, force, allow_unverified, true)?;
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
    let version = installer::ensure_installed(store, selector, force, allow_unverified, true)?;
    let _version_lock = store.lock_version(&version, true)?;
    if !store.is_installed(&version)? {
        return Err(AvmError::NotInstalled(version));
    }
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
    let version = installer::ensure_installed(store, selector, false, allow_unverified, true)?;
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
    let mut releases = catalog::available_releases(store, &remote, include_prerelease, refresh)?;
    if let Some(query) = query {
        releases.retain(|release| catalog::release_matches_query(release, query));
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

fn info(store: &Store, requested: &str, refresh: bool, json: bool) -> Result<()> {
    let selector = Selector::parse(requested)?;
    let remote = GitHubClient::from_env()?;
    let release = catalog::release_for_selector(store, &remote, &selector, refresh)?;
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
            shell::init_line(shell::detected_shell(), &store.paths.bin)
        );
    }
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
