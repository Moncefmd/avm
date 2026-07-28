use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use crate::error::{AvmError, Result};
use crate::store::Store;
use crate::version;

pub const VERSION_ENV: &str = "AVM_ARGOCD_VERSION";
pub const PROJECT_VERSION_FILE: &str = ".argocd-version";

const MAX_PROJECT_VERSION_BYTES: u64 = 256;

/// The input that selected an Argo CD version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolutionSource {
    Environment,
    ProjectPin { path: PathBuf },
    GlobalDefault,
}

impl fmt::Display for ResolutionSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment => formatter.write_str(VERSION_ENV),
            Self::ProjectPin { path } => {
                write!(formatter, "project pin {}", path.display())
            }
            Self::GlobalDefault => formatter.write_str("AVM default"),
        }
    }
}

/// An exact canonical version together with the reason it was selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolution {
    pub version: String,
    pub source: ResolutionSource,
}

impl Resolution {
    pub fn explain(&self) -> String {
        format!("{} selected by {}", self.version, self.source)
    }
}

/// Resolve without network access or filesystem mutation.
///
/// Precedence is:
///
/// 1. the injected `AVM_ARGOCD_VERSION` value
/// 2. the nearest `.argocd-version` at `cwd` or one of its ancestors
/// 3. the store's existing default
///
/// Callers should inject an absolute current working directory. An absent result
/// means that none of the four sources selected a version.
pub fn resolve_version(
    store: &Store,
    environment: Option<&OsStr>,
    cwd: &Path,
) -> Result<Option<Resolution>> {
    if let Some(resolution) = resolve_environment_override(environment)? {
        return Ok(Some(resolution));
    }

    if let Some(resolution) = resolve_project_pin(cwd)? {
        return Ok(Some(resolution));
    }

    Ok(store.default_version()?.map(|version| Resolution {
        version,
        source: ResolutionSource::GlobalDefault,
    }))
}

/// Resolve only the process-level override, without consulting the current directory or store.
pub fn resolve_environment_override(environment: Option<&OsStr>) -> Result<Option<Resolution>> {
    let Some(value) = environment else {
        return Ok(None);
    };
    let value = value.to_str().ok_or_else(|| {
        AvmError::Message(format!(
            "{VERSION_ENV} is not valid UTF-8; set it to one exact version such as v3.4.5"
        ))
    })?;
    Ok(Some(Resolution {
        version: normalize_sourced(value, VERSION_ENV, false)?,
        source: ResolutionSource::Environment,
    }))
}

/// Resolve a version or report that no selection exists.
pub fn resolve_required(
    store: &Store,
    environment: Option<&OsStr>,
    cwd: &Path,
) -> Result<Resolution> {
    resolve_version(store, environment, cwd)?.ok_or(AvmError::NoVersionSelected)
}

pub fn resolve_project_pin(cwd: &Path) -> Result<Option<Resolution>> {
    ensure_absolute_directory_path(cwd)?;

    for directory in cwd.ancestors() {
        let path = directory.join(PROJECT_VERSION_FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(AvmError::io(format!("inspect {}", path.display()), error));
            }
        };

        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AvmError::Message(format!(
                "{} must be a regular file containing one exact version such as v3.4.5",
                path.display()
            )));
        }
        if metadata.len() > MAX_PROJECT_VERSION_BYTES {
            return Err(AvmError::Message(format!(
                "{} is too large; it must contain only one exact version such as v3.4.5",
                path.display()
            )));
        }

        let file = fs::File::open(&path)
            .map_err(|error| AvmError::io(format!("open {}", path.display()), error))?;
        let opened_metadata = file
            .metadata()
            .map_err(|error| AvmError::io(format!("inspect open {}", path.display()), error))?;
        if !opened_metadata.is_file() {
            return Err(AvmError::Message(format!(
                "{} must be a regular file containing one exact version such as v3.4.5",
                path.display()
            )));
        }
        if opened_metadata.len() > MAX_PROJECT_VERSION_BYTES {
            return Err(AvmError::Message(format!(
                "{} is too large; it must contain only one exact version such as v3.4.5",
                path.display()
            )));
        }
        let mut bytes = Vec::with_capacity((MAX_PROJECT_VERSION_BYTES + 1) as usize);
        let mut limited = file.take(MAX_PROJECT_VERSION_BYTES + 1);
        limited
            .read_to_end(&mut bytes)
            .map_err(|error| AvmError::io(format!("read {}", path.display()), error))?;
        if bytes.len() as u64 > MAX_PROJECT_VERSION_BYTES {
            return Err(AvmError::Message(format!(
                "{} is too large; it must contain only one exact version such as v3.4.5",
                path.display()
            )));
        }
        let contents = String::from_utf8(bytes).map_err(|_| {
            AvmError::Message(format!(
                "{} is not valid UTF-8; write one exact version such as v3.4.5",
                path.display()
            ))
        })?;
        let label = path.display().to_string();
        let version = normalize_sourced(&contents, &label, true)?;
        return Ok(Some(Resolution {
            version,
            source: ResolutionSource::ProjectPin { path },
        }));
    }

    Ok(None)
}

pub fn write_project_pin(cwd: &Path, requested: &str) -> Result<PathBuf> {
    let version = normalize_explicit(requested)?;
    ensure_absolute_directory_path(cwd)?;
    let directory = fs::symlink_metadata(cwd)
        .map_err(|error| AvmError::io(format!("inspect {}", cwd.display()), error))?;
    if directory.file_type().is_symlink() || !directory.is_dir() {
        return Err(AvmError::UnsafePath(cwd.to_path_buf()));
    }

    let path = cwd.join(PROJECT_VERSION_FILE);
    let existing_permissions = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(AvmError::UnsafePath(path));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(AvmError::io(format!("inspect {}", path.display()), error));
        }
    };

    let mut temporary = NamedTempFile::new_in(cwd).map_err(|error| {
        AvmError::io(format!("create temporary file in {}", cwd.display()), error)
    })?;
    if let Some(permissions) = existing_permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| {
                AvmError::io(format!("set permissions for {}", path.display()), error)
            })?;
    }
    temporary
        .write_all(format!("{version}\n").as_bytes())
        .map_err(|error| AvmError::io(format!("write temporary {}", path.display()), error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| AvmError::io(format!("sync temporary {}", path.display()), error))?;
    temporary
        .persist(&path)
        .map_err(|error| AvmError::io(format!("replace {}", path.display()), error.error))?;
    Ok(path)
}

pub fn remove_project_pin(cwd: &Path) -> Result<Option<PathBuf>> {
    ensure_absolute_directory_path(cwd)?;
    let directory = fs::symlink_metadata(cwd)
        .map_err(|error| AvmError::io(format!("inspect {}", cwd.display()), error))?;
    if directory.file_type().is_symlink() || !directory.is_dir() {
        return Err(AvmError::UnsafePath(cwd.to_path_buf()));
    }

    let path = cwd.join(PROJECT_VERSION_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AvmError::io(format!("inspect {}", path.display()), error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AvmError::UnsafePath(path));
    }
    fs::remove_file(&path)
        .map_err(|error| AvmError::io(format!("remove {}", path.display()), error))?;
    Ok(Some(path))
}

fn normalize_explicit(input: &str) -> Result<String> {
    version::normalize(input)
}

fn ensure_absolute_directory_path(cwd: &Path) -> Result<()> {
    if !cwd.is_absolute() {
        return Err(AvmError::Message(format!(
            "the project directory must be an absolute path, found {}",
            cwd.display()
        )));
    }
    Ok(())
}

fn normalize_sourced(input: &str, source: &str, allow_terminal_newline: bool) -> Result<String> {
    let value = if allow_terminal_newline {
        input
            .strip_suffix("\r\n")
            .or_else(|| input.strip_suffix('\n'))
            .unwrap_or(input)
    } else {
        input
    };

    if value.contains('\r') || value.contains('\n') {
        return Err(AvmError::Message(format!(
            "{source} must contain exactly one version on one line"
        )));
    }

    let value = value.trim();
    if value.is_empty() {
        return Err(AvmError::Message(format!(
            "{source} is empty; set it to one exact version such as v3.4.5 or remove it"
        )));
    }

    let normalized = version::normalize(value).map_err(|_| {
        AvmError::Message(format!(
            "{source} is not an exact Argo CD version; expected a value such as v3.4.5"
        ))
    })?;
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Platform;
    use crate::store::Paths;

    fn test_store(root: &Path) -> Store {
        Store::new(Paths::new(root.to_path_buf()), Platform::current().unwrap())
    }

    fn write_default(store: &Store, version: &str) {
        fs::create_dir_all(&store.paths.state).unwrap();
        fs::write(store.paths.state.join("default"), format!("{version}\n")).unwrap();
    }

    #[test]
    fn environment_wins_project_pin_and_default() {
        let temporary = tempfile::tempdir().unwrap();
        let store = test_store(&temporary.path().join("avm"));
        write_default(&store, "v1.0.0");
        fs::write(temporary.path().join(PROJECT_VERSION_FILE), "v2.0.0\n").unwrap();

        let resolution =
            resolve_required(&store, Some(OsStr::new(" 3.0.0 ")), temporary.path()).unwrap();

        assert_eq!(resolution.version, "v3.0.0");
        assert_eq!(resolution.source, ResolutionSource::Environment);
    }

    #[test]
    fn nearest_project_pin_wins_parent_pin_and_default() {
        let temporary = tempfile::tempdir().unwrap();
        let store = test_store(&temporary.path().join("avm"));
        write_default(&store, "v1.0.0");
        fs::write(temporary.path().join(PROJECT_VERSION_FILE), "v2.0.0\n").unwrap();
        let project = temporary.path().join("workspace").join("service");
        let nested = project.join("src").join("commands");
        fs::create_dir_all(&nested).unwrap();
        fs::write(project.join(PROJECT_VERSION_FILE), "3.0.0\r\n").unwrap();

        let resolution = resolve_required(&store, None, &nested).unwrap();

        assert_eq!(resolution.version, "v3.0.0");
        assert_eq!(
            resolution.source,
            ResolutionSource::ProjectPin {
                path: project.join(PROJECT_VERSION_FILE)
            }
        );
    }

    #[test]
    fn default_is_the_final_fallback() {
        let temporary = tempfile::tempdir().unwrap();
        let store = test_store(&temporary.path().join("avm"));
        write_default(&store, "v3.4.5");
        let cwd = temporary.path().join("project");
        fs::create_dir(&cwd).unwrap();

        let resolution = resolve_required(&store, None, &cwd).unwrap();

        assert_eq!(resolution.version, "v3.4.5");
        assert_eq!(resolution.source, ResolutionSource::GlobalDefault);
        assert_eq!(resolution.explain(), "v3.4.5 selected by AVM default");
    }

    #[test]
    fn no_source_returns_none_without_creating_the_store() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("missing-avm-home");
        let cwd = temporary.path().join("project");
        fs::create_dir(&cwd).unwrap();
        let store = test_store(&root);

        assert_eq!(resolve_version(&store, None, &cwd).unwrap(), None);
        assert!(!root.exists());
    }

    #[test]
    fn empty_project_pin_is_actionable_and_does_not_fall_back() {
        let temporary = tempfile::tempdir().unwrap();
        let store = test_store(&temporary.path().join("avm"));
        write_default(&store, "v3.4.5");
        let pin = temporary.path().join(PROJECT_VERSION_FILE);
        fs::write(&pin, "\n").unwrap();

        let error = resolve_version(&store, None, temporary.path()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&pin.display().to_string()));
        assert!(message.contains("is empty"));
        assert!(message.contains("remove it"));
    }

    #[test]
    fn malformed_and_multiline_project_pins_are_rejected() {
        for contents in ["stable\n", "v3\n", "v3.4.5\nv3.4.6\n"] {
            let temporary = tempfile::tempdir().unwrap();
            let store = test_store(&temporary.path().join("avm"));
            fs::write(temporary.path().join(PROJECT_VERSION_FILE), contents).unwrap();

            let error = resolve_version(&store, None, temporary.path()).unwrap_err();
            assert!(
                error.to_string().contains(PROJECT_VERSION_FILE),
                "unexpected error for {contents:?}: {error}"
            );
        }
    }

    #[test]
    fn malformed_project_pin_error_does_not_echo_file_contents() {
        let temporary = tempfile::tempdir().unwrap();
        let store = test_store(&temporary.path().join("avm"));
        let secret = "api-token=do-not-print-this";
        fs::write(temporary.path().join(PROJECT_VERSION_FILE), secret).unwrap();

        let error = resolve_version(&store, None, temporary.path()).unwrap_err();
        assert!(error.to_string().contains(PROJECT_VERSION_FILE));
        assert!(!error.to_string().contains(secret));
        assert!(!error.to_string().contains("do-not-print-this"));
    }

    #[test]
    fn project_lookup_rejects_relative_start_paths() {
        let error = resolve_project_pin(Path::new("relative/project")).unwrap_err();
        assert!(error.to_string().contains("absolute path"));
    }

    #[test]
    fn malformed_environment_value_is_rejected_before_project_lookup() {
        let temporary = tempfile::tempdir().unwrap();
        let store = test_store(&temporary.path().join("avm"));
        fs::write(temporary.path().join(PROJECT_VERSION_FILE), "v3.4.5\n").unwrap();

        let error = resolve_version(&store, Some(OsStr::new("v3.4.5\nv3.4.6")), temporary.path())
            .unwrap_err();

        assert!(error.to_string().contains(VERSION_ENV));
        assert!(error.to_string().contains("one line"));
    }

    #[test]
    fn environment_override_resolves_without_a_project_directory() {
        let resolution = resolve_environment_override(Some(OsStr::new("v3.4.5")))
            .unwrap()
            .unwrap();
        assert_eq!(resolution.version, "v3.4.5");
        assert_eq!(resolution.source, ResolutionSource::Environment);
    }

    #[test]
    fn writes_canonical_project_pin_atomically() {
        let temporary = tempfile::tempdir().unwrap();
        let path = write_project_pin(temporary.path(), "3.4.5").unwrap();
        assert_eq!(path, temporary.path().join(PROJECT_VERSION_FILE));
        assert_eq!(fs::read_to_string(&path).unwrap(), "v3.4.5\n");

        write_project_pin(temporary.path(), "v3.4.6").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "v3.4.6\n");
    }

    #[test]
    fn project_pin_writer_rejects_non_files() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(PROJECT_VERSION_FILE);
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            write_project_pin(temporary.path(), "3.4.5"),
            Err(AvmError::UnsafePath(found)) if found == path
        ));
    }

    #[test]
    fn removes_only_the_pin_in_the_current_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let parent_pin = temporary.path().join(PROJECT_VERSION_FILE);
        fs::write(&parent_pin, "v3.4.5\n").unwrap();
        let project = temporary.path().join("project");
        fs::create_dir(&project).unwrap();

        assert_eq!(remove_project_pin(&project).unwrap(), None);
        assert!(parent_pin.is_file());

        let pin = write_project_pin(&project, "3.4.6").unwrap();
        assert_eq!(remove_project_pin(&project).unwrap(), Some(pin.clone()));
        assert!(!pin.exists());
        assert!(parent_pin.is_file());
    }
}
