use crate::catalog;
use crate::error::{AvmError, Result};
use crate::github::{GitHubClient, sanitized_url};
use crate::resolver::{VERSION_ENV, resolve_version};
use crate::store::{InstallMetadata, Store, make_executable};
use crate::version::{self, Selector};

pub(crate) fn install(
    store: &Store,
    selector: Option<&str>,
    force: bool,
    allow_unverified: bool,
) -> Result<()> {
    let requested = match selector {
        Some(selector) => selector.to_owned(),
        None => {
            let environment = std::env::var_os(VERSION_ENV);
            let cwd = std::env::current_dir()
                .map_err(|error| AvmError::io("determine current directory", error))?;
            resolve_version(store, environment.as_deref(), &cwd)?
                .map(|resolution| resolution.version)
                .unwrap_or_else(|| version::STABLE.to_owned())
        }
    };
    let version = ensure_installed(store, &requested, force, allow_unverified, true)?;
    println!("Argo CD {version} is installed.");
    Ok(())
}

pub(crate) fn ensure_installed(
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
    let release = catalog::release_for_selector(store, &remote, &selector, false)?;
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
    if download.bytes == 0 {
        return Err(AvmError::EmptyDownload(asset_name));
    }
    if let Some(expected_size) = asset.size
        && download.bytes != expected_size
    {
        return Err(AvmError::DownloadSizeMismatch {
            asset: asset_name,
            expected: expected_size,
            actual: download.bytes,
        });
    }

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
