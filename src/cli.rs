use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::{ArgValueCandidates, CompletionCandidate};

use crate::github::DEFAULT_API_URL;
use crate::platform::Platform;
use crate::store::{Paths, Store};

#[derive(Clone, Debug, Parser)]
#[command(
    name = "avm",
    version,
    about = "A fast, safe version manager for the Argo CD CLI",
    long_about = "AVM installs and selects Argo CD CLI versions while keeping one consistent \
                   `argocd` command in your PATH."
)]
pub struct Cli {
    /// Override the AVM data directory (default: ~/.avm)
    #[arg(long, global = true, env = "AVM_HOME", value_name = "DIR")]
    pub avm_home: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Install an Argo CD CLI release
    Install {
        /// `stable`, `X`, `X.Y`, `vX.Y.Z`, or `X.Y.Z`; omit for the selected version or `stable`
        #[arg(add = ArgValueCandidates::new(install_version_candidates))]
        selector: Option<String>,

        /// Replace an existing or incomplete installation
        #[arg(long)]
        force: bool,

        /// Permit releases that publish neither an API digest nor a checksum manifest
        #[arg(long)]
        allow_unverified: bool,
    },

    /// Create the managed `argocd` command, add it to PATH, and enable tab completion
    #[command(
        long_about = "Initialize AVM for a shell. This creates or repairs the managed `argocd` \
                            dispatcher in AVM_HOME/bin, configures that directory in PATH, and \
                            installs AVM tab completion. It does not download or select an Argo CD \
                            CLI version."
    )]
    Init {
        /// Shell to configure (auto-detected when omitted)
        #[arg(long)]
        shell: Option<Shell>,

        /// Preview the resulting configuration without applying it
        #[arg(long)]
        dry_run: bool,

        /// Configure the managed `argocd` command and PATH without installing tab completion
        #[arg(long)]
        no_completion: bool,
    },

    /// Remove AVM-managed shell integration while preserving installed versions
    Uninit {
        /// Preview the cleanup without applying it
        #[arg(long)]
        dry_run: bool,

        /// Remove AVM_HOME/bin from the Windows user PATH even without an ownership receipt
        #[arg(long)]
        remove_path: bool,
    },

    /// Set the user-wide default Argo CD CLI version
    Default {
        /// Release selector such as `stable`, `X`, `X.Y`, or `vX.Y.Z`
        #[arg(
            add = ArgValueCandidates::new(install_version_candidates),
            required_unless_present = "unset",
            conflicts_with = "unset"
        )]
        selector: Option<String>,

        /// Remove the user-wide default
        #[arg(long)]
        unset: bool,

        /// Replace an existing or incomplete installation
        #[arg(long, conflicts_with = "unset")]
        force: bool,

        /// Permit releases that publish neither an API digest nor a checksum manifest
        #[arg(long, conflicts_with = "unset")]
        allow_unverified: bool,
    },

    /// Pin an Argo CD CLI version for the current project
    Pin {
        /// Release selector such as `stable`, `X`, `X.Y`, or `vX.Y.Z`
        #[arg(
            add = ArgValueCandidates::new(install_version_candidates),
            required_unless_present = "unset",
            conflicts_with = "unset"
        )]
        selector: Option<String>,

        /// Remove the pin in the current directory
        #[arg(long)]
        unset: bool,

        /// Replace an existing or incomplete installation
        #[arg(long, conflicts_with = "unset")]
        force: bool,

        /// Permit releases that publish neither an API digest nor a checksum manifest
        #[arg(long, conflicts_with = "unset")]
        allow_unverified: bool,
    },

    /// Run one Argo CD CLI version without changing persistent selection
    Exec {
        /// Release selector (`stable`, `X`, `X.Y`, or `vX.Y.Z`)
        #[arg(add = ArgValueCandidates::new(install_version_candidates))]
        selector: String,

        /// Permit releases that publish neither an API digest nor a checksum manifest
        #[arg(long)]
        allow_unverified: bool,

        /// Arguments passed unchanged to `argocd` (place them after `--`)
        #[arg(last = true, num_args = 0.., value_name = "ARG")]
        argocd_args: Vec<OsString>,
    },

    /// List installed Argo CD CLI versions
    List {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Remove an installed Argo CD CLI version
    Uninstall {
        /// `vX.Y.Z` or `X.Y.Z`
        #[arg(add = ArgValueCandidates::new(installed_version_candidates))]
        version: String,
    },

    /// Show the resolved version and where the selection came from
    Status {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// List downloadable Argo CD CLI releases
    Available {
        /// A version selector, or text to match case-insensitively in release tags
        query: Option<String>,

        /// Include release candidates and other prereleases
        #[arg(long)]
        prerelease: bool,

        /// Ignore cached release metadata
        #[arg(long)]
        refresh: bool,

        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Show metadata and assets for one Argo CD CLI release
    Info {
        /// Release selector such as `stable`, `X`, `X.Y`, or `vX.Y.Z`
        #[arg(add = ArgValueCandidates::new(install_version_candidates))]
        selector: String,

        /// Ignore cached release metadata
        #[arg(long)]
        refresh: bool,

        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Generate or install shell completion
    Completion {
        shell: Shell,

        /// Install and activate completion for the selected shell
        #[arg(long)]
        install: bool,

        /// Preview the resulting installation without applying it
        #[arg(long, requires = "install")]
        dry_run: bool,
    },

    /// Inspect the local AVM installation
    Doctor {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    #[command(name = "__dispatch-v1", hide = true)]
    DispatchV1 {
        #[arg(last = true, allow_hyphen_values = true, num_args = 0.., value_name = "ARG")]
        argocd_args: Vec<OsString>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl From<Shell> for clap_complete::Shell {
    fn from(value: Shell) -> Self {
        match value {
            Shell::Bash => Self::Bash,
            Shell::Zsh => Self::Zsh,
            Shell::Fish => Self::Fish,
            Shell::Powershell => Self::PowerShell,
        }
    }
}

impl std::fmt::Display for Shell {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Powershell => "powershell",
        };
        formatter.write_str(name)
    }
}

fn install_version_candidates() -> Vec<CompletionCandidate> {
    let mut candidates = vec![CompletionCandidate::new("stable")];
    candidates.extend(cached_version_candidates());
    candidates
}

fn installed_version_candidates() -> Vec<CompletionCandidate> {
    completion_store()
        .and_then(|store| store.installed_versions().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|installed| CompletionCandidate::new(installed.version))
        .collect()
}

fn cached_version_candidates() -> Vec<CompletionCandidate> {
    let Some(store) = completion_store() else {
        return Vec::new();
    };
    let source = std::env::var("AVM_GITHUB_API_URL")
        .unwrap_or_else(|_| DEFAULT_API_URL.to_owned())
        .trim_end_matches('/')
        .to_owned();
    let mut releases = store
        .load_release_cache(&source, None)
        .ok()
        .flatten()
        .unwrap_or_default();
    releases.retain(|release| !release.draft && release.parsed_version().is_some());
    releases.sort_by_key(|release| std::cmp::Reverse(release.parsed_version()));
    releases
        .into_iter()
        .map(|release| CompletionCandidate::new(release.tag_name))
        .collect()
}

fn completion_store() -> Option<Store> {
    let platform = Platform::current().ok()?;
    let paths = Paths::resolve(completion_home_override()).ok()?;
    Some(Store::new(paths, platform))
}

fn completion_home_override() -> Option<PathBuf> {
    let mut args = std::env::args_os();
    while let Some(argument) = args.next() {
        if argument == "--avm-home" {
            return args.next().map(PathBuf::from);
        }
        if let Some(value) = split_long_option(&argument, "--avm-home=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

fn split_long_option(argument: &OsStr, prefix: &str) -> Option<OsString> {
    let argument = argument.to_string_lossy();
    argument.strip_prefix(prefix).map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn completes_channels_without_network_access() {
        let mut command = Cli::command();
        let candidates = clap_complete::engine::complete(
            &mut command,
            ["avm", "install", ""].map(OsString::from).to_vec(),
            2,
            None,
        )
        .unwrap();
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.get_value() == "stable")
        );
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.get_value() == "latest")
        );
    }

    #[test]
    fn install_accepts_an_optional_selector() {
        assert!(Cli::try_parse_from(["avm", "install"]).is_ok());
        assert!(Cli::try_parse_from(["avm", "install", "stable"]).is_ok());
    }

    #[test]
    fn persistent_selection_requires_a_selector_or_unset() {
        assert!(Cli::try_parse_from(["avm", "default"]).is_err());
        assert!(Cli::try_parse_from(["avm", "default", "stable"]).is_ok());
        assert!(Cli::try_parse_from(["avm", "default", "--unset"]).is_ok());
        assert!(Cli::try_parse_from(["avm", "default", "stable", "--unset"]).is_err());
        assert!(Cli::try_parse_from(["avm", "pin"]).is_err());
        assert!(Cli::try_parse_from(["avm", "pin", "--unset"]).is_ok());
    }

    #[test]
    fn exec_forwards_trailing_hyphenated_arguments() {
        let cli = Cli::try_parse_from([
            "avm",
            "exec",
            "v3.4.5",
            "--allow-unverified",
            "--",
            "app",
            "sync",
            "--prune",
        ])
        .unwrap();
        let Some(Command::Exec {
            selector,
            allow_unverified,
            argocd_args,
        }) = cli.command
        else {
            panic!("expected exec command");
        };

        assert_eq!(selector, "v3.4.5");
        assert!(allow_unverified);
        assert_eq!(argocd_args, ["app", "sync", "--prune"].map(OsString::from));
    }

    #[test]
    fn exec_requires_a_selector() {
        assert!(Cli::try_parse_from(["avm", "exec", "--", "version"]).is_err());
    }

    #[test]
    fn uninit_is_global_and_supports_safe_preview_and_explicit_path_cleanup() {
        assert!(Cli::try_parse_from(["avm", "uninit"]).is_ok());
        assert!(Cli::try_parse_from(["avm", "uninit", "--dry-run"]).is_ok());
        assert!(Cli::try_parse_from(["avm", "uninit", "--remove-path"]).is_ok());
        assert!(Cli::try_parse_from(["avm", "uninit", "--shell", "bash"]).is_err());
    }
}
