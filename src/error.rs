use std::io;
use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, AvmError>;

#[derive(Debug, Error)]
pub enum AvmError {
    #[error("invalid Argo CD version {input:?}: expected `vX.Y.Z` or `X.Y.Z`")]
    InvalidVersion { input: String },

    #[error(
        "invalid Argo CD selector {input:?}: expected `stable`, `X`, `X.Y`, `vX.Y.Z`, or `X.Y.Z`"
    )]
    InvalidSelector { input: String },

    #[error("Argo CD does not publish a CLI binary for {os}/{arch}")]
    UnsupportedPlatform { os: String, arch: String },

    #[error("Argo CD release {0} was not found")]
    ReleaseNotFound(String),

    #[error("release {version} has no asset named {asset}")]
    AssetNotFound { version: String, asset: String },

    #[error(
        "release {version} does not publish a usable checksum for {asset}; \
         pass `--allow-unverified` only if you accept that risk"
    )]
    ChecksumMissing { version: String, asset: String },

    #[error("release {version} publishes conflicting checksums for {asset}")]
    ChecksumConflict { version: String, asset: String },

    #[error("malformed checksum metadata in release {0}")]
    MalformedChecksum(String),

    #[error("checksum mismatch for {asset}: expected {expected}, found {actual}")]
    ChecksumMismatch {
        asset: String,
        expected: String,
        actual: String,
    },

    #[error(
        "release {version} now publishes checksum {published}, but AVM recorded {recorded}; \
         refusing a mutable-tag change (uninstall the version first to accept it explicitly)"
    )]
    ChecksumChanged {
        version: String,
        recorded: String,
        published: String,
    },

    #[error("download exceeded the 1 GiB safety limit")]
    DownloadTooLarge,

    #[error("GitHub API rate limit exhausted{reset}")]
    RateLimited { reset: String },

    #[error("GitHub API request failed ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("network request failed: {0}")]
    Http(reqwest::Error),

    #[error("network response failed while {context}: {source}")]
    NetworkIo {
        context: String,
        #[source]
        source: io::Error,
    },

    #[error("version {0} is not installed; run `avm install {0}` first")]
    NotInstalled(String),

    #[error(
        "version {0} is incomplete because its execution lock is missing; \
         run `avm install {0} --force` to repair it"
    )]
    ExecutionLockMissing(String),

    #[error("version {0} is already installed; use `--force` to replace it")]
    AlreadyInstalled(String),

    #[error("installed version {0} is incomplete or corrupt; use `avm install {0} --force`")]
    CorruptInstall(String),

    #[error("no Argo CD version is selected; run `avm default stable` or `avm pin <selector>`")]
    NoVersionSelected,

    #[error("one or more AVM diagnostic checks failed")]
    HealthCheckFailed,

    #[error("default-version state is invalid: {0:?}; run `avm default <selector>` to replace it")]
    CorruptState(String),

    #[error("refusing to overwrite {path}: it is not an AVM-managed dispatcher")]
    UnmanagedShim { path: PathBuf },

    #[error("refusing unsafe filesystem operation outside the AVM home: {0}")]
    UnsafePath(PathBuf),

    #[error("another AVM process is operating on {0}; try again shortly")]
    LockTimeout(String),

    #[error("cached release metadata is invalid: {0}")]
    InvalidCache(String),

    #[error("completion installation is not supported for {0}; generate the script instead")]
    CompletionInstallUnsupported(String),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },

    #[error("{0}")]
    Message(String),
}

impl AvmError {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    pub fn http(source: reqwest::Error) -> Self {
        Self::Http(source.without_url())
    }

    pub fn network_io(context: impl Into<String>, source: io::Error) -> Self {
        Self::NetworkIo {
            context: context.into(),
            source,
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidVersion { .. } | Self::InvalidSelector { .. } => 2,
            Self::Http(_)
            | Self::NetworkIo { .. }
            | Self::Api { .. }
            | Self::RateLimited { .. }
            | Self::ReleaseNotFound(_)
            | Self::AssetNotFound { .. } => 3,
            Self::ChecksumMissing { .. }
            | Self::ChecksumConflict { .. }
            | Self::MalformedChecksum(_)
            | Self::ChecksumMismatch { .. }
            | Self::ChecksumChanged { .. }
            | Self::DownloadTooLarge => 4,
            _ => 5,
        }
    }
}
