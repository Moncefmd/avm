use std::fs;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command as ProcessCommand;

use tempfile::NamedTempFile;

use crate::error::{AvmError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FileSnapshot {
    Missing,
    Present(Vec<u8>),
}

impl FileSnapshot {
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Missing => None,
            Self::Present(contents) => Some(contents),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PlannedWrite {
    pub(crate) path: PathBuf,
    expected: FileSnapshot,
    desired: Vec<u8>,
    max_bytes: u64,
    description: &'static str,
}

#[derive(Debug)]
pub(crate) struct PlannedDelete {
    path: PathBuf,
    expected: FileSnapshot,
    max_bytes: u64,
    description: &'static str,
}

impl PlannedWrite {
    pub(crate) fn new(
        path: PathBuf,
        expected: FileSnapshot,
        desired: Vec<u8>,
        max_bytes: u64,
        description: &'static str,
    ) -> Self {
        Self {
            path,
            expected,
            desired,
            max_bytes,
            description,
        }
    }
}

impl PlannedDelete {
    pub(crate) fn new(
        path: PathBuf,
        expected: FileSnapshot,
        max_bytes: u64,
        description: &'static str,
    ) -> Self {
        Self {
            path,
            expected,
            max_bytes,
            description,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn read_snapshot(
    path: &Path,
    max_bytes: u64,
    description: &'static str,
) -> Result<FileSnapshot> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileSnapshot::Missing);
        }
        Err(error) => return Err(AvmError::io(format!("inspect {}", path.display()), error)),
    };
    if is_link_like(&metadata) || !metadata.is_file() {
        return Err(AvmError::UnsafePath(path.to_path_buf()));
    }
    if metadata.len() > max_bytes {
        return Err(AvmError::Message(format!(
            "refusing to inspect oversized {description} {}",
            path.display()
        )));
    }
    fs::read(path)
        .map(FileSnapshot::Present)
        .map_err(|error| AvmError::io(format!("read {}", path.display()), error))
}

pub(crate) fn preflight(write: &PlannedWrite) -> Result<()> {
    let current = read_snapshot(&write.path, write.max_bytes, write.description)?;
    if current == write.expected || current.bytes() == Some(write.desired.as_slice()) {
        Ok(())
    } else {
        Err(concurrent_change(&write.path))
    }
}

pub(crate) fn apply(write: PlannedWrite) -> Result<()> {
    let parent = write
        .path
        .parent()
        .ok_or_else(|| AvmError::UnsafePath(write.path.clone()))?;
    fs::create_dir_all(parent)
        .map_err(|error| AvmError::io(format!("create {}", parent.display()), error))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
        AvmError::io(
            format!("create temporary file in {}", parent.display()),
            error,
        )
    })?;
    std::io::Write::write_all(&mut temporary, &write.desired).map_err(|error| {
        AvmError::io(format!("write temporary {}", write.path.display()), error)
    })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| AvmError::io(format!("sync temporary {}", write.path.display()), error))?;

    let current = read_snapshot(&write.path, write.max_bytes, write.description)?;
    if current.bytes() == Some(write.desired.as_slice()) {
        return Ok(());
    }
    if current != write.expected {
        return Err(concurrent_change(&write.path));
    }
    if let Ok(metadata) = fs::metadata(&write.path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|error| {
                AvmError::io(
                    format!("preserve permissions for {}", write.path.display()),
                    error,
                )
            })?;
    }

    if matches!(write.expected, FileSnapshot::Missing) {
        temporary.persist_noclobber(&write.path).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                concurrent_change(&write.path)
            } else {
                AvmError::io(format!("create {}", write.path.display()), error.error)
            }
        })?;
    } else {
        persist_replacement(temporary, &write.path)?;
    }
    Ok(())
}

pub(crate) fn preflight_delete(delete: &PlannedDelete) -> Result<()> {
    let current = read_snapshot(&delete.path, delete.max_bytes, delete.description)?;
    if current == delete.expected || matches!(current, FileSnapshot::Missing) {
        Ok(())
    } else {
        Err(concurrent_change(&delete.path))
    }
}

pub(crate) fn apply_delete(delete: PlannedDelete) -> Result<bool> {
    let current = read_snapshot(&delete.path, delete.max_bytes, delete.description)?;
    if matches!(current, FileSnapshot::Missing) {
        return Ok(false);
    }
    if current != delete.expected {
        return Err(concurrent_change(&delete.path));
    }
    fs::remove_file(&delete.path)
        .map(|()| true)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(AvmError::io(
                    format!("remove {}", delete.path.display()),
                    error,
                ))
            }
        })
}

#[cfg(not(windows))]
fn persist_replacement(temporary: NamedTempFile, destination: &Path) -> Result<()> {
    temporary
        .persist(destination)
        .map_err(|error| AvmError::io(format!("replace {}", destination.display()), error.error))?;
    Ok(())
}

#[cfg(windows)]
fn persist_replacement(temporary: NamedTempFile, destination: &Path) -> Result<()> {
    let powershell = crate::windows::trusted_powershell()?;
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
try {
    [IO.File]::Replace(
        $env:AVM_REPLACEMENT_FILE,
        $env:AVM_DESTINATION_FILE,
        $env:AVM_BACKUP_FILE,
        $false
    )
} catch {
    Write-Error $_
    exit 1
}
"#;
    let backup = NamedTempFile::new_in(
        destination
            .parent()
            .ok_or_else(|| AvmError::UnsafePath(destination.to_path_buf()))?,
    )
    .map_err(|error| AvmError::io("reserve Windows replacement backup path", error))?;
    let backup_path = backup.path().to_path_buf();
    backup
        .close()
        .map_err(|error| AvmError::io("release Windows replacement backup path", error))?;
    let (file, temporary_path) = temporary
        .keep()
        .map_err(|error| AvmError::io("retain temporary replacement file", error.error))?;
    drop(file);
    let status = ProcessCommand::new(&powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .env("AVM_REPLACEMENT_FILE", &temporary_path)
        .env("AVM_DESTINATION_FILE", destination)
        .env("AVM_BACKUP_FILE", &backup_path)
        .status();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            let _ = fs::remove_file(&temporary_path);
            return Err(AvmError::io(
                "start PowerShell for atomic file replacement",
                error,
            ));
        }
    };
    if !status.success() {
        let _ = fs::remove_file(&temporary_path);
        let backup_note = if backup_path.exists() {
            format!("; the original was retained at {}", backup_path.display())
        } else {
            String::new()
        };
        return Err(AvmError::Message(format!(
            "could not atomically replace {} from {} while preserving its Windows security metadata{}",
            destination.display(),
            temporary_path.display(),
            backup_note
        )));
    }
    if let Err(error) = fs::remove_file(&backup_path) {
        eprintln!(
            "warning: remove temporary replacement backup {}: {error}",
            backup_path.display()
        );
    }
    Ok(())
}

fn concurrent_change(path: &Path) -> AvmError {
    AvmError::ConcurrentFileChange {
        path: path.to_path_buf(),
    }
}

#[cfg(windows)]
pub(crate) fn is_link_like(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub(crate) fn is_link_like(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(path: &Path, expected: FileSnapshot, desired: &[u8]) -> PlannedWrite {
        PlannedWrite::new(
            path.to_path_buf(),
            expected,
            desired.to_vec(),
            1024,
            "test file",
        )
    }

    #[test]
    fn refuses_to_overwrite_a_file_changed_after_planning() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile");
        fs::write(&path, "before").unwrap();
        let write = planned(
            &path,
            read_snapshot(&path, 1024, "test file").unwrap(),
            b"planned",
        );
        fs::write(&path, "concurrent").unwrap();

        assert!(matches!(
            apply(write),
            Err(AvmError::ConcurrentFileChange { .. })
        ));
        assert_eq!(fs::read(path).unwrap(), b"concurrent");
    }

    #[test]
    fn refuses_to_replace_a_file_created_after_a_missing_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile");
        let write = planned(&path, FileSnapshot::Missing, b"planned");
        fs::write(&path, "concurrent").unwrap();

        assert!(matches!(
            apply(write),
            Err(AvmError::ConcurrentFileChange { .. })
        ));
        assert_eq!(fs::read(path).unwrap(), b"concurrent");
    }

    #[test]
    fn accepts_an_identical_concurrent_write_as_already_applied() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile");
        let write = planned(&path, FileSnapshot::Missing, b"planned");
        fs::write(&path, "planned").unwrap();

        apply(write).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"planned");
    }

    #[test]
    fn planned_delete_refuses_a_concurrent_replacement_and_is_idempotent() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("completion");
        fs::write(&path, "managed").unwrap();
        let expected = read_snapshot(&path, 1024, "test file").unwrap();
        let delete = PlannedDelete::new(path.clone(), expected, 1024, "test file");
        fs::write(&path, "concurrent").unwrap();
        assert!(matches!(
            apply_delete(delete),
            Err(AvmError::ConcurrentFileChange { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "concurrent");

        let expected = read_snapshot(&path, 1024, "test file").unwrap();
        let delete = PlannedDelete::new(path.clone(), expected, 1024, "test file");
        assert!(apply_delete(delete).unwrap());
        let delete = PlannedDelete::new(path, FileSnapshot::Missing, 1024, "test file");
        assert!(!apply_delete(delete).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn windows_replacement_preserves_a_protected_dacl() {
        const PROTECT_ACL: &str = r#"
$ErrorActionPreference = 'Stop'
$acl = [IO.File]::GetAccessControl($env:AVM_ACL_TEST_PATH)
$acl.SetAccessRuleProtection($true, $true)
[IO.File]::SetAccessControl($env:AVM_ACL_TEST_PATH, $acl)
"#;
        const READ_SDDL: &str = r#"
([IO.File]::GetAccessControl($env:AVM_ACL_TEST_PATH)).GetSecurityDescriptorSddlForm(
    [Security.AccessControl.AccessControlSections]::All
)
"#;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile.ps1");
        fs::write(&path, "before").unwrap();
        let status = std::process::Command::new(crate::windows::trusted_powershell().unwrap())
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                PROTECT_ACL,
            ])
            .env("AVM_ACL_TEST_PATH", &path)
            .status()
            .unwrap();
        assert!(status.success());
        let before = powershell_output(&path, READ_SDDL);

        let write = planned(
            &path,
            read_snapshot(&path, 1024, "test file").unwrap(),
            b"after",
        );
        apply(write).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"after");
        assert_eq!(powershell_output(&path, READ_SDDL), before);
    }

    #[cfg(windows)]
    #[test]
    fn trusted_powershell_is_an_absolute_system32_application() {
        let powershell = crate::windows::trusted_powershell().unwrap();
        assert!(powershell.is_absolute());
        assert!(powershell.ends_with(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
        assert!(powershell.is_file());
    }

    #[cfg(windows)]
    fn powershell_output(path: &Path, script: &str) -> String {
        let output = std::process::Command::new(crate::windows::trusted_powershell().unwrap())
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
            ])
            .env("AVM_ACL_TEST_PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "PowerShell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}
