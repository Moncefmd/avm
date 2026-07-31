use std::ffi::OsString;
use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;

use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};

use crate::error::{AvmError, Result};

const WINDOWS_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub(crate) fn trusted_powershell() -> Result<PathBuf> {
    let windows_version = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(WINDOWS_VERSION_KEY, KEY_READ | KEY_WOW64_64KEY)
        .map_err(|error| AvmError::io("open the trusted Windows version registry key", error))?;
    let windows_root: OsString = windows_version
        .get_value("SystemRoot")
        .map_err(|error| AvmError::io("read the trusted Windows system root", error))?;
    let windows_root = PathBuf::from(windows_root);
    if !windows_root.is_absolute() {
        return Err(AvmError::Message(
            "the trusted Windows system root is not an absolute path".to_owned(),
        ));
    }

    let powershell = windows_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let metadata = fs::symlink_metadata(&powershell).map_err(|error| {
        AvmError::io(
            format!(
                "inspect trusted Windows PowerShell {}",
                powershell.display()
            ),
            error,
        )
    })?;
    if metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !metadata.is_file()
    {
        return Err(AvmError::UnsafePath(powershell));
    }
    Ok(powershell)
}
