use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{Builder, NamedTempFile, TempDir};

use crate::error::{AvmError, Result};
use crate::github::{MAX_BINARY_BYTES, Release};
use crate::platform::Platform;
use crate::version;

pub const RELEASE_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);
const METADATA_FILE: &str = "install.json";
const MAX_INSTALL_METADATA_BYTES: u64 = 64 * 1024;
const CACHE_SCHEMA: u32 = 1;
const INSTALL_SCHEMA: u32 = 1;
const SHIM_SCHEMA: u32 = 1;

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
    pub versions: PathBuf,
    pub bin: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub locks: PathBuf,
}

impl Paths {
    pub fn resolve(override_root: Option<PathBuf>) -> Result<Self> {
        let root = match override_root {
            Some(root) => root,
            None => match std::env::var_os("AVM_HOME") {
                Some(root) => PathBuf::from(root),
                None => BaseDirs::new()
                    .ok_or_else(|| {
                        AvmError::Message("could not determine the home directory".to_owned())
                    })?
                    .home_dir()
                    .join(".avm"),
            },
        };
        if root.as_os_str().is_empty() {
            return Err(AvmError::UnsafePath(root));
        }
        let root = std::path::absolute(&root)
            .map_err(|error| AvmError::io(format!("resolve {}", root.display()), error))?;
        if root.parent().is_none()
            || BaseDirs::new()
                .map(|directories| paths_equal(&root, directories.home_dir()))
                .unwrap_or(false)
        {
            return Err(AvmError::UnsafePath(root));
        }
        Ok(Self::new(root))
    }

    pub fn new(root: PathBuf) -> Self {
        Self {
            versions: root.join("versions"),
            bin: root.join("bin"),
            state: root.join("state"),
            cache: root.join("cache"),
            locks: root.join("locks"),
            root,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    pub paths: Paths,
    pub platform: Platform,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InstallMetadata {
    pub schema: u32,
    pub version: String,
    pub asset: String,
    pub sha256: String,
    pub verified: bool,
    pub source_url: String,
    pub installed_at: u64,
}

impl InstallMetadata {
    pub fn new(
        version: String,
        asset: String,
        sha256: String,
        verified: bool,
        source_url: String,
    ) -> Self {
        Self {
            schema: INSTALL_SCHEMA,
            version,
            asset,
            sha256,
            verified,
            source_url,
            installed_at: unix_timestamp(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct InstalledVersion {
    pub version: String,
    pub is_default: bool,
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<InstallMetadata>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReleaseCache {
    schema: u32,
    source: String,
    fetched_at: u64,
    releases: Vec<Release>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ShimMetadata {
    schema: u32,
    avm_version: String,
    #[serde(default)]
    pending: bool,
}

pub struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl Store {
    pub fn new(paths: Paths, platform: Platform) -> Self {
        Self { paths, platform }
    }

    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(&self.paths.root).map_err(|error| {
            AvmError::io(format!("create {}", self.paths.root.display()), error)
        })?;
        ensure_managed_directory(&self.paths.root)?;
        for path in [
            &self.paths.versions,
            &self.paths.bin,
            &self.paths.state,
            &self.paths.cache,
            &self.paths.locks,
        ] {
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(AvmError::io(format!("create {}", path.display()), error));
                }
            }
            ensure_managed_directory(path)?;
        }
        Ok(())
    }

    fn validate_existing_layout(&self) -> Result<()> {
        for path in [
            &self.paths.root,
            &self.paths.versions,
            &self.paths.bin,
            &self.paths.state,
            &self.paths.cache,
            &self.paths.locks,
        ] {
            match fs::symlink_metadata(path) {
                Ok(_) => ensure_managed_directory(path)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(AvmError::io(format!("inspect {}", path.display()), error));
                }
            }
        }
        Ok(())
    }

    pub fn lock_version(&self, version: &str, shared: bool) -> Result<FileLock> {
        ensure_canonical_tag(version)?;
        self.ensure_layout()?;
        let path = self.paths.locks.join(format!("{version}.lock"));
        acquire_lock(&path, shared, format!("version {version}"))
    }

    pub fn lock_state(&self, shared: bool) -> Result<FileLock> {
        self.ensure_layout()?;
        acquire_lock(
            &self.paths.locks.join("state.lock"),
            shared,
            "the default version".to_owned(),
        )
    }

    pub fn version_dir(&self, version: &str) -> Result<PathBuf> {
        ensure_canonical_tag(version)?;
        let path = self.paths.versions.join(version);
        ensure_direct_child(&self.paths.versions, &path)?;
        Ok(path)
    }

    pub fn version_binary(&self, version: &str) -> Result<PathBuf> {
        Ok(self.version_dir(version)?.join(self.platform.binary_name()))
    }

    pub fn shim_path(&self) -> PathBuf {
        self.paths.bin.join(self.platform.binary_name())
    }

    pub fn ensure_dispatcher(&self) -> Result<()> {
        let _state_lock = self.lock_state(false)?;
        self.ensure_shim()
    }

    pub fn is_installed(&self, version: &str) -> Result<bool> {
        self.validate_existing_layout()?;
        let directory = self.version_dir(version)?;
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if is_link_like(&metadata) => {
                return Err(AvmError::UnsafePath(directory));
            }
            Ok(metadata) if !metadata.is_dir() => return Ok(false),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(AvmError::io(
                    format!("inspect {}", directory.display()),
                    error,
                ));
            }
        }
        let binary = self.version_binary(version)?;
        let binary_metadata = match fs::symlink_metadata(&binary) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(AvmError::io(format!("inspect {}", binary.display()), error));
            }
        };
        if !binary_metadata.file_type().is_file()
            || is_link_like(&binary_metadata)
            || binary_metadata.len() == 0
            || binary_metadata.len() > MAX_BINARY_BYTES
        {
            return Ok(false);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if binary_metadata.permissions().mode() & 0o111 == 0 {
                return Ok(false);
            }
        }

        let install_metadata = match self.read_install_metadata(version) {
            Ok(metadata) => metadata,
            Err(AvmError::CorruptInstall(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        match install_metadata {
            Some(metadata) if self.install_metadata_is_valid(version, &metadata) => Ok(true),
            Some(_) | None => Ok(false),
        }
    }

    pub fn version_entry_exists(&self, version: &str) -> Result<bool> {
        self.validate_existing_layout()?;
        let directory = self.version_dir(version)?;
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if is_link_like(&metadata) || !metadata.is_dir() => {
                Err(AvmError::UnsafePath(directory))
            }
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(AvmError::io(
                format!("inspect {}", directory.display()),
                error,
            )),
        }
    }

    pub fn installed_sha256(&self, version: &str) -> Result<String> {
        if !self.is_installed(version)? {
            return Err(AvmError::CorruptInstall(version.to_owned()));
        }
        let path = self.version_binary(version)?;
        let mut file = File::open(&path)
            .map_err(|error| AvmError::io(format!("open {}", path.display()), error))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 128 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| AvmError::io(format!("read {}", path.display()), error))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex_digest(&hasher.finalize()))
    }

    pub fn install_metadata(&self, version: &str) -> Result<Option<InstallMetadata>> {
        if !self.version_entry_exists(version)? {
            return Ok(None);
        }
        match self.read_install_metadata(version)? {
            Some(metadata) if self.install_metadata_is_valid(version, &metadata) => {
                Ok(Some(metadata))
            }
            Some(_) => Err(AvmError::CorruptInstall(version.to_owned())),
            None => Ok(None),
        }
    }

    pub fn staging_dir(&self) -> Result<TempDir> {
        self.ensure_layout()?;
        Builder::new()
            .prefix(".staging-")
            .tempdir_in(&self.paths.versions)
            .map_err(|error| AvmError::io("create install staging directory", error))
    }

    pub fn commit_install(
        &self,
        staging: &TempDir,
        version: &str,
        metadata: &InstallMetadata,
        force: bool,
    ) -> Result<()> {
        ensure_canonical_tag(version)?;
        if metadata.version != version {
            return Err(AvmError::UnsafePath(staging.path().to_path_buf()));
        }

        write_json_file(&staging.path().join(METADATA_FILE), metadata)?;
        let final_dir = self.version_dir(version)?;
        let backup = self
            .paths
            .versions
            .join(format!(".replaced-{version}-{}", std::process::id()));
        let mut had_backup = false;

        if let Ok(existing) = fs::symlink_metadata(&final_dir) {
            if is_link_like(&existing) || !existing.is_dir() {
                return Err(AvmError::UnsafePath(final_dir));
            }
            if !force {
                return if self.is_installed(version)? {
                    Err(AvmError::AlreadyInstalled(version.to_owned()))
                } else {
                    Err(AvmError::CorruptInstall(version.to_owned()))
                };
            }
            if fs::symlink_metadata(&backup).is_ok() {
                return Err(AvmError::Message(format!(
                    "stale install backup exists at {}; remove it after inspection",
                    backup.display()
                )));
            }
            fs::rename(&final_dir, &backup).map_err(|error| {
                AvmError::io(
                    format!("move existing installation {}", final_dir.display()),
                    error,
                )
            })?;
            had_backup = true;
        }

        if let Err(error) = fs::rename(staging.path(), &final_dir) {
            if had_backup {
                let _ = fs::rename(&backup, &final_dir);
            }
            return Err(AvmError::io(
                format!("commit installation {}", final_dir.display()),
                error,
            ));
        }

        if had_backup {
            fs::remove_dir_all(&backup).map_err(|error| {
                AvmError::io(format!("remove install backup {}", backup.display()), error)
            })?;
        }
        Ok(())
    }

    pub fn installed_versions(&self) -> Result<Vec<InstalledVersion>> {
        let default = match self.default_version() {
            Ok(default) => default,
            Err(AvmError::CorruptState(_)) => None,
            Err(error) => return Err(error),
        };
        let entries = match fs::read_dir(&self.paths.versions) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(AvmError::io(
                    format!("read {}", self.paths.versions.display()),
                    error,
                ));
            }
        };

        let mut installed = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|error| AvmError::io("read installed version entry", error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if version::parse_tag(&name).is_none() {
                continue;
            }
            let file_type = entry.file_type().map_err(|error| {
                AvmError::io(format!("inspect {}", entry.path().display()), error)
            })?;
            if file_type.is_symlink() || !file_type.is_dir() {
                installed.push(InstalledVersion {
                    is_default: default.as_deref() == Some(name.as_str()),
                    metadata: None,
                    healthy: false,
                    version: name,
                });
                continue;
            }
            let healthy = self.is_installed(&name)?;
            installed.push(InstalledVersion {
                is_default: default.as_deref() == Some(name.as_str()),
                metadata: if healthy {
                    self.read_install_metadata(&name)?
                } else {
                    None
                },
                healthy,
                version: name,
            });
        }

        installed.sort_by(|a, b| {
            let a = version::parse_tag(&a.version);
            let b = version::parse_tag(&b.version);
            b.cmp(&a)
        });
        Ok(installed)
    }

    pub fn default_version(&self) -> Result<Option<String>> {
        self.validate_existing_layout()?;
        let default = self.paths.state.join("default");
        let metadata = match fs::symlink_metadata(&default) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AvmError::io(
                    format!("inspect {}", default.display()),
                    error,
                ));
            }
        };
        if is_link_like(&metadata) || !metadata.is_file() {
            return Err(AvmError::UnsafePath(default));
        }
        if metadata.len() > 128 {
            return Err(AvmError::CorruptState("<oversized>".to_owned()));
        }
        let contents = fs::read_to_string(&default)
            .map_err(|error| AvmError::io(format!("read {}", default.display()), error))?;
        let value = contents.trim();
        let normalized =
            version::normalize(value).map_err(|_| AvmError::CorruptState(value.to_owned()))?;
        if normalized != value {
            return Err(AvmError::CorruptState(value.to_owned()));
        }
        Ok(Some(normalized))
    }

    pub fn set_default(&self, version: &str) -> Result<()> {
        ensure_canonical_tag(version)?;
        let _version_lock = self.lock_version(version, true)?;
        if !self.is_installed(version)? {
            return Err(AvmError::NotInstalled(version.to_owned()));
        }
        let _state_lock = self.lock_state(false)?;
        self.ensure_shim()?;
        write_atomic(
            &self.paths.state.join("default"),
            format!("{version}\n").as_bytes(),
        )
    }

    pub fn clear_default(&self, expected: &str) -> Result<bool> {
        ensure_canonical_tag(expected)?;
        let _state_lock = self.lock_state(false)?;
        match self.default_version() {
            Ok(Some(default)) if default == expected => {}
            Ok(_) | Err(AvmError::CorruptState(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        let default = self.paths.state.join("default");
        match fs::remove_file(&default) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(AvmError::io(format!("remove {}", default.display()), error)),
        }
    }

    pub fn remove_version(&self, version: &str) -> Result<()> {
        ensure_canonical_tag(version)?;
        let directory = self.version_dir(version)?;
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|error| AvmError::io(format!("inspect {}", directory.display()), error))?;
        if is_link_like(&metadata) || !metadata.is_dir() {
            return Err(AvmError::UnsafePath(directory));
        }

        let removing = self
            .paths
            .versions
            .join(format!(".removing-{version}-{}", std::process::id()));
        fs::rename(&directory, &removing).map_err(|error| {
            AvmError::io(format!("stage removal {}", directory.display()), error)
        })?;
        fs::remove_dir_all(&removing)
            .map_err(|error| AvmError::io(format!("remove {}", removing.display()), error))
    }

    pub fn selected_binary(&self) -> Result<(String, PathBuf, FileLock)> {
        let version = self.default_version()?.ok_or(AvmError::NoVersionSelected)?;
        let (binary, lock) = self.binary_for(&version)?;
        Ok((version, binary, lock))
    }

    pub fn binary_for(&self, version: &str) -> Result<(PathBuf, FileLock)> {
        ensure_canonical_tag(version)?;
        self.validate_existing_layout()?;
        let lock_path = self.paths.locks.join(format!("{version}.lock"));
        let Some(lock) = acquire_existing_lock(&lock_path, true, format!("version {version}"))?
        else {
            return if self.version_entry_exists(version)? {
                Err(AvmError::ExecutionLockMissing(version.to_owned()))
            } else {
                Err(AvmError::NotInstalled(version.to_owned()))
            };
        };
        let binary = self.version_binary(version)?;
        if !self.is_installed(version)? {
            return if self.version_entry_exists(version)? {
                Err(AvmError::CorruptInstall(version.to_owned()))
            } else {
                Err(AvmError::NotInstalled(version.to_owned()))
            };
        }
        Ok((binary, lock))
    }

    pub fn is_bin_on_path(&self) -> bool {
        std::env::var_os("PATH")
            .map(|value| {
                std::env::split_paths(&value).any(|entry| paths_equal(&entry, &self.paths.bin))
            })
            .unwrap_or(false)
    }

    pub fn shim_is_healthy(&self) -> Result<bool> {
        self.validate_existing_layout()?;
        let shim = self.shim_path();
        let shim_metadata = match fs::symlink_metadata(&shim) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(AvmError::io(format!("inspect {}", shim.display()), error));
            }
        };
        if is_link_like(&shim_metadata) || !shim_metadata.is_file() || shim_metadata.len() == 0 {
            return Ok(false);
        }

        let marker = self.paths.state.join("dispatcher.json");
        let marker_metadata = match fs::symlink_metadata(&marker) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(AvmError::io(format!("inspect {}", marker.display()), error));
            }
        };
        if is_link_like(&marker_metadata) || !marker_metadata.is_file() {
            return Ok(false);
        }
        let owned = fs::read(&marker)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ShimMetadata>(&bytes).ok());
        let marker_is_healthy = owned.is_some_and(|metadata| {
            metadata.schema == SHIM_SCHEMA
                && !metadata.pending
                && metadata.avm_version == env!("CARGO_PKG_VERSION")
        });
        if !marker_is_healthy {
            return Ok(false);
        }
        let current = std::env::current_exe()
            .map_err(|error| AvmError::io("locate the AVM executable", error))?;
        files_have_same_contents(&shim, &current)
    }

    pub fn load_release_cache(
        &self,
        source: &str,
        max_age: Option<Duration>,
    ) -> Result<Option<Vec<Release>>> {
        self.validate_existing_layout()?;
        let path = self.paths.cache.join("releases.json");
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => Err(AvmError::io(format!("read {}", path.display()), error))?,
        };
        let cache: ReleaseCache = serde_json::from_slice(&contents)
            .map_err(|error| AvmError::InvalidCache(error.to_string()))?;
        if cache.schema != CACHE_SCHEMA || cache.source != source {
            return Ok(None);
        }
        if let Some(max_age) = max_age {
            let age = unix_timestamp().saturating_sub(cache.fetched_at);
            if age > max_age.as_secs() {
                return Ok(None);
            }
        }
        Ok(Some(cache.releases))
    }

    pub fn write_release_cache(&self, source: &str, releases: &[Release]) -> Result<()> {
        self.ensure_layout()?;
        let cache = ReleaseCache {
            schema: CACHE_SCHEMA,
            source: source.to_owned(),
            fetched_at: unix_timestamp(),
            releases: releases.to_vec(),
        };
        write_json_atomic(&self.paths.cache.join("releases.json"), &cache)
    }

    fn read_install_metadata(&self, version: &str) -> Result<Option<InstallMetadata>> {
        let path = self.version_dir(version)?.join(METADATA_FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AvmError::io(format!("inspect {}", path.display()), error));
            }
        };
        if is_link_like(&metadata)
            || !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_INSTALL_METADATA_BYTES
        {
            return Err(AvmError::CorruptInstall(version.to_owned()));
        }
        let mut file = File::open(&path)
            .map_err(|error| AvmError::io(format!("open {}", path.display()), error))?;
        let opened = file
            .metadata()
            .map_err(|error| AvmError::io(format!("inspect open {}", path.display()), error))?;
        if !opened.is_file() || opened.len() == 0 || opened.len() > MAX_INSTALL_METADATA_BYTES {
            return Err(AvmError::CorruptInstall(version.to_owned()));
        }
        let mut contents = Vec::with_capacity(opened.len() as usize);
        file.read_to_end(&mut contents)
            .map_err(|error| AvmError::io(format!("read {}", path.display()), error))?;
        if contents.len() as u64 > MAX_INSTALL_METADATA_BYTES {
            return Err(AvmError::CorruptInstall(version.to_owned()));
        }
        serde_json::from_slice(&contents)
            .map(Some)
            .map_err(|_| AvmError::CorruptInstall(version.to_owned()))
    }

    fn install_metadata_is_valid(&self, version: &str, metadata: &InstallMetadata) -> bool {
        metadata.schema == INSTALL_SCHEMA
            && metadata.version == version
            && metadata.asset == self.platform.asset_name()
            && valid_sha256(&metadata.sha256)
    }

    fn ensure_shim(&self) -> Result<()> {
        self.ensure_layout()?;
        let shim = self.shim_path();
        let marker = self.paths.state.join("dispatcher.json");
        let mut replace = true;

        match fs::symlink_metadata(&shim) {
            Ok(metadata) if is_link_like(&metadata) => {
                return Err(AvmError::UnmanagedShim { path: shim });
            }
            Ok(metadata) if metadata.is_file() => {
                let owned = fs::read(&marker)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<ShimMetadata>(&bytes).ok())
                    .filter(|metadata| metadata.schema == SHIM_SCHEMA);
                let Some(owned) = owned else {
                    return Err(AvmError::UnmanagedShim { path: shim });
                };
                let current = std::env::current_exe()
                    .map_err(|error| AvmError::io("locate the AVM executable", error))?;
                replace = owned.pending
                    || owned.avm_version != env!("CARGO_PKG_VERSION")
                    || !files_have_same_contents(&shim, &current)?;
            }
            Ok(_) => return Err(AvmError::UnmanagedShim { path: shim }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AvmError::io(format!("inspect {}", shim.display()), error)),
        }

        if replace {
            let pending = ShimMetadata {
                schema: SHIM_SCHEMA,
                avm_version: env!("CARGO_PKG_VERSION").to_owned(),
                pending: true,
            };
            write_json_atomic(&marker, &pending)?;
            copy_current_executable_atomic(&shim)?;
            let complete = ShimMetadata {
                pending: false,
                ..pending
            };
            write_json_atomic(&marker, &complete)?;
        }
        Ok(())
    }
}

fn ensure_canonical_tag(tag: &str) -> Result<()> {
    let normalized = version::normalize(tag)?;
    if normalized != tag {
        return Err(AvmError::InvalidVersion {
            input: tag.to_owned(),
        });
    }
    Ok(())
}

fn ensure_direct_child(parent: &Path, child: &Path) -> Result<()> {
    if child.parent() == Some(parent) {
        Ok(())
    } else {
        Err(AvmError::UnsafePath(child.to_path_buf()))
    }
}

fn ensure_managed_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AvmError::io(format!("inspect {}", path.display()), error))?;
    if is_link_like(&metadata) || !metadata.is_dir() {
        return Err(AvmError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

fn acquire_lock(path: &Path, shared: bool, label: String) -> Result<FileLock> {
    let file = open_lock_file(path, true)?.ok_or_else(|| {
        AvmError::Message(format!("could not create managed lock {}", path.display()))
    })?;
    acquire_open_lock(file, path, shared, label)
}

fn acquire_existing_lock(path: &Path, shared: bool, label: String) -> Result<Option<FileLock>> {
    let Some(file) = open_lock_file(path, false)? else {
        return Ok(None);
    };
    acquire_open_lock(file, path, shared, label).map(Some)
}

fn open_lock_file(path: &Path, create: bool) -> Result<Option<File>> {
    for _ in 0..3 {
        match fs::symlink_metadata(path) {
            Ok(metadata) if is_link_like(&metadata) || !metadata.is_file() => {
                return Err(AvmError::UnsafePath(path.to_path_buf()));
            }
            Ok(_) => match OpenOptions::new().read(true).write(create).open(path) {
                Ok(file) => {
                    if !file
                        .metadata()
                        .map_err(|error| {
                            AvmError::io(format!("inspect open lock {}", path.display()), error)
                        })?
                        .is_file()
                    {
                        return Err(AvmError::UnsafePath(path.to_path_buf()));
                    }
                    return Ok(Some(file));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(AvmError::io(format!("open lock {}", path.display()), error));
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => {
                return Ok(None);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(path)
                {
                    Ok(file) => return Ok(Some(file)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => {
                        return Err(AvmError::io(
                            format!("create lock {}", path.display()),
                            error,
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(AvmError::io(
                    format!("inspect lock {}", path.display()),
                    error,
                ));
            }
        }
    }
    Err(AvmError::Message(format!(
        "lock path changed repeatedly while opening {}",
        path.display()
    )))
}

fn acquire_open_lock(file: File, path: &Path, shared: bool, label: String) -> Result<FileLock> {
    let started = std::time::Instant::now();

    loop {
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match result {
            Ok(()) => return Ok(FileLock { file }),
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < LOCK_TIMEOUT => {
                thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(AvmError::LockTimeout(label));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(AvmError::io(
                    format!("acquire lock {}", path.display()),
                    error,
                ));
            }
        }
    }
}

fn files_have_same_contents(left: &Path, right: &Path) -> Result<bool> {
    let left_metadata = fs::symlink_metadata(left)
        .map_err(|error| AvmError::io(format!("inspect {}", left.display()), error))?;
    let right_metadata = fs::symlink_metadata(right)
        .map_err(|error| AvmError::io(format!("inspect {}", right.display()), error))?;
    if is_link_like(&left_metadata)
        || is_link_like(&right_metadata)
        || !left_metadata.is_file()
        || !right_metadata.is_file()
        || left_metadata.len() != right_metadata.len()
    {
        return Ok(false);
    }

    let mut left_file = File::open(left)
        .map_err(|error| AvmError::io(format!("open {}", left.display()), error))?;
    let mut right_file = File::open(right)
        .map_err(|error| AvmError::io(format!("open {}", right.display()), error))?;
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    loop {
        let left_read = left_file
            .read(&mut left_buffer)
            .map_err(|error| AvmError::io(format!("read {}", left.display()), error))?;
        let right_read = right_file
            .read(&mut right_buffer)
            .map_err(|error| AvmError::io(format!("read {}", right.display()), error))?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn copy_current_executable_atomic(destination: &Path) -> Result<()> {
    let source = std::env::current_exe()
        .map_err(|error| AvmError::io("locate the AVM executable", error))?;
    let parent = destination
        .parent()
        .ok_or_else(|| AvmError::UnsafePath(destination.to_path_buf()))?;
    fs::create_dir_all(parent)
        .map_err(|error| AvmError::io(format!("create {}", parent.display()), error))?;

    let mut input = File::open(&source)
        .map_err(|error| AvmError::io(format!("open {}", source.display()), error))?;
    let mut temporary = Builder::new()
        .prefix(".argocd-dispatcher-")
        .suffix(if cfg!(windows) { ".exe" } else { "" })
        .tempfile_in(parent)
        .map_err(|error| AvmError::io("create temporary AVM dispatcher", error))?;
    std::io::copy(&mut input, temporary.as_file_mut())
        .map_err(|error| AvmError::io("copy AVM dispatcher", error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| AvmError::io("sync AVM dispatcher", error))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&source)
            .map_err(|error| AvmError::io(format!("inspect {}", source.display()), error))?
            .permissions()
            .mode();
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|error| AvmError::io("set AVM dispatcher permissions", error))?;
    }

    temporary
        .persist(destination)
        .map_err(|error| AvmError::io(format!("replace {}", destination.display()), error.error))?;
    Ok(())
}

pub fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|error| AvmError::io(format!("chmod {}", path.display()), error))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn write_json_file(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| AvmError::Message(format!("serialize metadata: {error}")))?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| AvmError::io(format!("create {}", path.display()), error))?;
    file.write_all(&bytes)
        .map_err(|error| AvmError::io(format!("write {}", path.display()), error))?;
    file.sync_all()
        .map_err(|error| AvmError::io(format!("sync {}", path.display()), error))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| AvmError::Message(format!("serialize metadata: {error}")))?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
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
    temporary
        .write_all(bytes)
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

fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .eq_ignore_ascii_case(right.to_string_lossy().trim_end_matches(['\\', '/']))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let platform = Platform::from_target(
            if cfg!(windows) { "windows" } else { "linux" },
            "x86_64",
            true,
        )
        .unwrap();
        let store = Store::new(Paths::new(temp.path().join(".avm")), platform);
        (temp, store)
    }

    fn install_dummy(store: &Store, version: &str) {
        let staging = store.staging_dir().unwrap();
        let binary = staging.path().join(store.platform.binary_name());
        fs::write(&binary, b"dummy").unwrap();
        make_executable(&binary).unwrap();
        let metadata = InstallMetadata::new(
            version.to_owned(),
            store.platform.asset_name(),
            "a".repeat(64),
            true,
            "https://example.invalid/argocd".to_owned(),
        );
        store
            .commit_install(&staging, version, &metadata, false)
            .unwrap();
    }

    #[test]
    fn reports_complete_and_corrupt_version_entries() {
        let (_temp, store) = test_store();
        install_dummy(&store, "v3.4.5");
        fs::create_dir_all(store.paths.versions.join("v3.4.4")).unwrap();
        fs::write(store.paths.versions.join("v3.4.3"), b"not a directory").unwrap();
        let versions = store.installed_versions().unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].version, "v3.4.5");
        assert!(versions[0].healthy);
        assert!(versions[0].metadata.as_ref().unwrap().verified);
        assert_eq!(versions[1].version, "v3.4.4");
        assert!(!versions[1].healthy);
        assert!(versions[1].metadata.is_none());
        assert_eq!(versions[2].version, "v3.4.3");
        assert!(!versions[2].healthy);
        assert!(versions[2].metadata.is_none());
    }

    #[test]
    fn rejects_an_install_without_metadata() {
        let (_temp, store) = test_store();
        let directory = store.version_dir("v3.4.5").unwrap();
        fs::create_dir_all(&directory).unwrap();
        let binary = directory.join(store.platform.binary_name());
        fs::write(&binary, b"unmanaged binary").unwrap();
        make_executable(&binary).unwrap();
        assert!(!store.is_installed("v3.4.5").unwrap());
    }

    #[test]
    fn rejects_oversized_install_metadata() {
        let (_temp, store) = test_store();
        install_dummy(&store, "v3.4.5");
        fs::write(
            store.version_dir("v3.4.5").unwrap().join(METADATA_FILE),
            vec![b' '; MAX_INSTALL_METADATA_BYTES as usize + 1],
        )
        .unwrap();

        assert!(!store.is_installed("v3.4.5").unwrap());
        assert!(matches!(
            store.install_metadata("v3.4.5"),
            Err(AvmError::CorruptInstall(version)) if version == "v3.4.5"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_install_metadata() {
        let (temp, store) = test_store();
        install_dummy(&store, "v3.4.5");
        let metadata = store.version_dir("v3.4.5").unwrap().join(METADATA_FILE);
        fs::remove_file(&metadata).unwrap();
        let outside = temp.path().join("outside.json");
        fs::write(&outside, b"{}").unwrap();
        std::os::unix::fs::symlink(&outside, &metadata).unwrap();

        assert!(!store.is_installed("v3.4.5").unwrap());
        assert!(matches!(
            store.install_metadata("v3.4.5"),
            Err(AvmError::CorruptInstall(version)) if version == "v3.4.5"
        ));
    }

    #[test]
    fn refuses_traversal_before_touching_disk() {
        let (temp, store) = test_store();
        let sentinel = temp.path().join("sentinel");
        fs::write(&sentinel, b"keep").unwrap();
        assert!(store.remove_version("../../").is_err());
        assert!(sentinel.exists());
    }

    #[test]
    fn runtime_lookup_does_not_create_a_missing_store() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("missing-avm-home");
        let store = Store::new(
            Paths::new(root.clone()),
            Platform::from_target(
                if cfg!(windows) { "windows" } else { "linux" },
                "x86_64",
                true,
            )
            .unwrap(),
        );

        assert!(matches!(
            store.binary_for("v3.4.5"),
            Err(AvmError::NotInstalled(version)) if version == "v3.4.5"
        ));
        assert!(!root.exists(), "runtime resolution must remain read-only");
    }

    #[test]
    fn runtime_lookup_does_not_create_a_missing_execution_lock() {
        let (_temp, store) = test_store();
        install_dummy(&store, "v3.4.5");
        let lock = store.paths.locks.join("v3.4.5.lock");
        assert!(!lock.exists());

        assert!(matches!(
            store.binary_for("v3.4.5"),
            Err(AvmError::ExecutionLockMissing(version)) if version == "v3.4.5"
        ));
        assert!(
            !lock.exists(),
            "runtime resolution must not create lock files"
        );
    }

    #[test]
    #[cfg(unix)]
    fn refuses_a_symlinked_version_lock() {
        let (temp, store) = test_store();
        store.ensure_layout().unwrap();
        let outside = temp.path().join("outside.lock");
        fs::write(&outside, []).unwrap();
        std::os::unix::fs::symlink(&outside, store.paths.locks.join("v3.4.5.lock")).unwrap();

        assert!(matches!(
            store.lock_version("v3.4.5", true),
            Err(AvmError::UnsafePath(_))
        ));
    }

    #[test]
    fn release_cache_honors_source_and_freshness() {
        let (_temp, store) = test_store();
        let release = Release {
            tag_name: "v3.4.5".to_owned(),
            draft: false,
            prerelease: false,
            assets: Vec::new(),
        };
        store
            .write_release_cache("https://api.example/releases", &[release])
            .unwrap();
        assert_eq!(
            store
                .load_release_cache(
                    "https://api.example/releases",
                    Some(Duration::from_secs(60))
                )
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .load_release_cache("https://other.example/releases", None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn refuses_to_replace_an_unmanaged_argocd_command() {
        let (_temp, store) = test_store();
        install_dummy(&store, "v3.4.5");
        fs::create_dir_all(&store.paths.bin).unwrap();
        let shim = store.shim_path();
        fs::write(&shim, b"user-owned").unwrap();

        let error = store.set_default("v3.4.5").unwrap_err();
        assert!(matches!(error, AvmError::UnmanagedShim { .. }));
        assert_eq!(fs::read(&shim).unwrap(), b"user-owned");
    }

    #[test]
    fn switches_the_default_between_installed_versions() {
        let (_temp, store) = test_store();
        install_dummy(&store, "v3.4.4");
        install_dummy(&store, "v3.4.5");

        store.set_default("v3.4.4").unwrap();
        assert_eq!(store.default_version().unwrap().as_deref(), Some("v3.4.4"));
        store.set_default("v3.4.5").unwrap();
        assert_eq!(store.default_version().unwrap().as_deref(), Some("v3.4.5"));
    }

    #[test]
    fn detects_and_repairs_a_modified_managed_shim() {
        let (_temp, store) = test_store();
        install_dummy(&store, "v3.4.5");
        store.set_default("v3.4.5").unwrap();
        assert!(store.shim_is_healthy().unwrap());

        fs::write(store.shim_path(), b"modified").unwrap();
        assert!(!store.shim_is_healthy().unwrap());
        store.set_default("v3.4.5").unwrap();
        assert!(store.shim_is_healthy().unwrap());
    }

    #[test]
    #[cfg(unix)]
    fn dispatcher_refuses_a_preexisting_symlink_without_creating_a_default() {
        let (_temp, store) = test_store();
        install_dummy(&store, "v3.4.5");
        fs::create_dir_all(&store.paths.bin).unwrap();
        std::os::unix::fs::symlink(store.version_binary("v3.4.5").unwrap(), store.shim_path())
            .unwrap();

        let error = store.ensure_dispatcher().unwrap_err();

        assert!(matches!(error, AvmError::UnmanagedShim { .. }));
        assert!(!store.paths.state.join("default").exists());
        assert!(
            fs::symlink_metadata(store.shim_path())
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn corrupt_default_state_does_not_hide_version_entries() {
        let (_temp, store) = test_store();
        install_dummy(&store, "v3.4.5");
        fs::write(store.paths.state.join("default"), b"not-a-version\n").unwrap();

        assert!(matches!(
            store.default_version(),
            Err(AvmError::CorruptState(_))
        ));
        let versions = store.installed_versions().unwrap();
        assert_eq!(versions.len(), 1);
        assert!(!versions[0].is_default);
        assert!(!store.clear_default("v3.4.5").unwrap());
        store.set_default("v3.4.5").unwrap();
        assert_eq!(store.default_version().unwrap().as_deref(), Some("v3.4.5"));
    }

    #[test]
    #[cfg(unix)]
    fn refuses_a_symlinked_version_directory() {
        let (temp, store) = test_store();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join(store.platform.binary_name()), b"external").unwrap();
        store.ensure_layout().unwrap();
        std::os::unix::fs::symlink(&outside, store.paths.versions.join("v3.4.5")).unwrap();
        assert!(matches!(
            store.is_installed("v3.4.5"),
            Err(AvmError::UnsafePath(_))
        ));
    }

    #[test]
    #[cfg(unix)]
    fn refuses_a_symlinked_managed_directory() {
        let (temp, store) = test_store();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&store.paths.root).unwrap();
        std::os::unix::fs::symlink(&outside, &store.paths.versions).unwrap();
        assert!(matches!(
            store.ensure_layout(),
            Err(AvmError::UnsafePath(_))
        ));
    }

    #[test]
    #[cfg(windows)]
    fn refuses_a_junction_at_a_managed_directory() {
        let (temp, store) = test_store();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&store.paths.root).unwrap();
        let output = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&store.paths.versions)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "could not create test junction: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(matches!(
            store.ensure_layout(),
            Err(AvmError::UnsafePath(_))
        ));
    }
}
