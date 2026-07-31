#[cfg(test)]
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use directories::BaseDirs;
#[cfg(windows)]
use directories::UserDirs;

use crate::atomic_file::{self, FileSnapshot, PlannedWrite};
use crate::error::{AvmError, Result};

const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProfileFormat {
    Utf8NoBom,
    PowerShell,
}

#[derive(Clone, Debug)]
pub(crate) struct BlockRequest {
    pub path: PathBuf,
    pub start: &'static str,
    pub end: &'static str,
    pub legacy_markers: Vec<(&'static str, &'static str)>,
    pub command: String,
    pub format: ProfileFormat,
}

#[derive(Clone, Debug)]
pub(crate) struct BlockRemovalRequest {
    pub path: PathBuf,
    pub markers: Vec<(&'static str, &'static str)>,
    pub format: ProfileFormat,
}

#[derive(Debug)]
pub(crate) struct ProfileUpdate {
    write: PlannedWrite,
}

struct ProfileDocument {
    path: PathBuf,
    original: String,
    updated: String,
    newline: &'static str,
    encoding: TextEncoding,
    format: ProfileFormat,
    snapshot: FileSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextEncoding {
    Utf8,
    Utf8Bom,
    Utf16LeBom,
    Utf16BeBom,
}

pub(crate) fn home_dir() -> Result<PathBuf> {
    BaseDirs::new()
        .ok_or_else(|| AvmError::Message("could not determine the home directory".to_owned()))
        .map(|directories| directories.home_dir().to_path_buf())
}

pub(crate) fn powershell_profiles() -> Result<Vec<PathBuf>> {
    #[cfg(windows)]
    {
        let documents = UserDirs::new()
            .and_then(|directories| directories.document_dir().map(Path::to_path_buf))
            .ok_or_else(|| {
                AvmError::Message("could not determine the Windows Documents directory".to_owned())
            })?;
        Ok(windows_powershell_profiles(&documents))
    }
    #[cfg(not(windows))]
    {
        Ok(vec![
            home_dir()?
                .join(".config")
                .join("powershell")
                .join("profile.ps1"),
        ])
    }
}

#[cfg(any(windows, test))]
pub(crate) fn windows_powershell_profiles(documents: &Path) -> Vec<PathBuf> {
    vec![
        documents.join("WindowsPowerShell").join("profile.ps1"),
        documents.join("PowerShell").join("profile.ps1"),
    ]
}

pub(crate) fn plan_updates(requests: Vec<BlockRequest>) -> Result<Vec<ProfileUpdate>> {
    let mut documents = Vec::<ProfileDocument>::new();
    for request in requests {
        let index = match documents
            .iter()
            .position(|document| document.path == request.path)
        {
            Some(index) => index,
            None => {
                documents.push(read_profile(&request.path, request.format)?);
                documents.len() - 1
            }
        };
        let document = &mut documents[index];
        if document.format != request.format {
            return Err(AvmError::Message(format!(
                "conflicting profile formats requested for {}",
                document.path.display()
            )));
        }
        document.updated = update_block(
            &document.path,
            &document.updated,
            document.newline,
            request.start,
            request.end,
            &request.legacy_markers,
            &request.command,
        )?;
    }

    documents
        .into_iter()
        .filter(|document| document.updated != document.original)
        .map(|document| {
            Ok(ProfileUpdate {
                write: PlannedWrite::new(
                    document.path,
                    document.snapshot,
                    encode_profile(&document.updated, document.encoding),
                    MAX_PROFILE_BYTES,
                    "shell profile",
                ),
            })
        })
        .collect()
}

pub(crate) fn plan_removals(requests: Vec<BlockRemovalRequest>) -> Result<Vec<ProfileUpdate>> {
    let mut documents = Vec::<ProfileDocument>::new();
    for request in requests {
        let index = match documents
            .iter()
            .position(|document| document.path == request.path)
        {
            Some(index) => index,
            None => {
                if !profile_may_contain_markers(&request.path, &request.markers, request.format)? {
                    continue;
                }
                documents.push(read_profile(&request.path, request.format)?);
                documents.len() - 1
            }
        };
        let document = &mut documents[index];
        if document.format != request.format {
            return Err(AvmError::Message(format!(
                "conflicting profile formats requested for {}",
                document.path.display()
            )));
        }
        document.updated =
            remove_marked_block(&document.path, &document.updated, &request.markers)?;
    }

    documents
        .into_iter()
        .filter(|document| document.updated != document.original)
        .map(|document| {
            Ok(ProfileUpdate {
                write: PlannedWrite::new(
                    document.path,
                    document.snapshot,
                    encode_profile(&document.updated, document.encoding),
                    MAX_PROFILE_BYTES,
                    "shell profile",
                ),
            })
        })
        .collect()
}

fn profile_may_contain_markers(
    path: &Path,
    markers: &[(&'static str, &'static str)],
    format: ProfileFormat,
) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(AvmError::io(format!("inspect {}", path.display()), error)),
    };
    if atomic_file::is_link_like(&metadata) || !metadata.is_file() {
        eprintln!(
            "warning: preserved unsafe or non-regular shell profile {}",
            path.display()
        );
        return Ok(false);
    }

    const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];
    const UTF16_LE_BOM: &[u8] = &[0xff, 0xfe];
    const UTF16_BE_BOM: &[u8] = &[0xfe, 0xff];
    const UTF32_LE_BOM: &[u8] = &[0xff, 0xfe, 0x00, 0x00];
    const UTF32_BE_BOM: &[u8] = &[0x00, 0x00, 0xfe, 0xff];

    let file = File::open(path)
        .map_err(|error| AvmError::io(format!("read {}", path.display()), error))?;
    let mut reader = BufReader::new(file);
    let prefix = reader
        .fill_buf()
        .map_err(|error| AvmError::io(format!("read {}", path.display()), error))?;
    let marker_text = markers
        .iter()
        .flat_map(|(start, end)| [*start, *end])
        .collect::<Vec<_>>();

    match format {
        ProfileFormat::Utf8NoBom
            if prefix.starts_with(UTF8_BOM)
                || prefix.starts_with(UTF16_LE_BOM)
                || prefix.starts_with(UTF16_BE_BOM) =>
        {
            Ok(false)
        }
        ProfileFormat::Utf8NoBom => scan_utf8_marker_lines(&mut reader, &marker_text, path),
        ProfileFormat::PowerShell
            if prefix.starts_with(UTF32_LE_BOM) || prefix.starts_with(UTF32_BE_BOM) =>
        {
            Ok(false)
        }
        ProfileFormat::PowerShell if prefix.starts_with(UTF8_BOM) => {
            reader.consume(UTF8_BOM.len());
            scan_utf8_marker_lines(&mut reader, &marker_text, path)
        }
        ProfileFormat::PowerShell if prefix.starts_with(UTF16_LE_BOM) => {
            reader.consume(UTF16_LE_BOM.len());
            scan_utf16_marker_lines(&mut reader, &marker_text, path, true)
        }
        ProfileFormat::PowerShell if prefix.starts_with(UTF16_BE_BOM) => {
            reader.consume(UTF16_BE_BOM.len());
            scan_utf16_marker_lines(&mut reader, &marker_text, path, false)
        }
        ProfileFormat::PowerShell => scan_utf8_marker_lines(&mut reader, &marker_text, path),
    }
}

fn scan_utf8_marker_lines(reader: &mut impl Read, markers: &[&str], path: &Path) -> Result<bool> {
    let maximum = markers.iter().map(|marker| marker.len()).max().unwrap_or(0);
    let mut line = Vec::with_capacity(maximum + 2);
    let mut overflow = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| AvmError::io(format!("read {}", path.display()), error))?;
        if read == 0 {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(!overflow
                && markers
                    .iter()
                    .any(|marker| line.as_slice() == marker.as_bytes()));
        }
        for byte in &chunk[..read] {
            if *byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if !overflow
                    && markers
                        .iter()
                        .any(|marker| line.as_slice() == marker.as_bytes())
                {
                    return Ok(true);
                }
                line.clear();
                overflow = false;
            } else if !overflow {
                line.push(*byte);
                if line.len() > maximum + 1 {
                    line.clear();
                    overflow = true;
                }
            }
        }
    }
}

fn scan_utf16_marker_lines(
    reader: &mut impl Read,
    markers: &[&str],
    path: &Path,
    little_endian: bool,
) -> Result<bool> {
    let markers = markers
        .iter()
        .map(|marker| marker.encode_utf16().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let maximum = markers.iter().map(Vec::len).max().unwrap_or(0);
    let mut line = Vec::with_capacity(maximum + 2);
    let mut overflow = false;
    let mut carry = None;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| AvmError::io(format!("read {}", path.display()), error))?;
        if read == 0 {
            if carry.is_some() {
                return Ok(false);
            }
            if line.last() == Some(&0x000d) {
                line.pop();
            }
            return Ok(!overflow
                && markers
                    .iter()
                    .any(|marker| line.as_slice() == marker.as_slice()));
        }
        for byte in &chunk[..read] {
            let Some(first) = carry.take() else {
                carry = Some(*byte);
                continue;
            };
            let unit = if little_endian {
                u16::from_le_bytes([first, *byte])
            } else {
                u16::from_be_bytes([first, *byte])
            };
            if unit == 0x000a {
                if line.last() == Some(&0x000d) {
                    line.pop();
                }
                if !overflow
                    && markers
                        .iter()
                        .any(|marker| line.as_slice() == marker.as_slice())
                {
                    return Ok(true);
                }
                line.clear();
                overflow = false;
            } else if !overflow {
                line.push(unit);
                if line.len() > maximum + 1 {
                    line.clear();
                    overflow = true;
                }
            }
        }
    }
}

pub(crate) fn apply_updates(updates: Vec<ProfileUpdate>) -> Result<()> {
    preflight_updates(&updates)?;
    for update in updates {
        atomic_file::apply(update.write)?;
    }
    Ok(())
}

pub(crate) fn preflight_updates(updates: &[ProfileUpdate]) -> Result<()> {
    for update in updates {
        atomic_file::preflight(&update.write)?;
    }
    Ok(())
}

impl ProfileUpdate {
    pub(crate) fn path(&self) -> &Path {
        &self.write.path
    }
}

fn read_profile(path: &Path, format: ProfileFormat) -> Result<ProfileDocument> {
    let snapshot = atomic_file::read_snapshot(path, MAX_PROFILE_BYTES, "shell profile")?;
    let (existing, encoding) = match snapshot.bytes() {
        Some(contents) => decode_profile(path, contents, format)?,
        None => (
            String::new(),
            match format {
                ProfileFormat::Utf8NoBom => TextEncoding::Utf8,
                ProfileFormat::PowerShell => TextEncoding::Utf8Bom,
            },
        ),
    };
    let newline = newline_style(path, &existing, format)?;
    Ok(ProfileDocument {
        path: path.to_path_buf(),
        original: existing.clone(),
        updated: existing,
        newline,
        encoding,
        format,
        snapshot,
    })
}

fn update_block(
    path: &Path,
    existing: &str,
    newline: &str,
    start: &str,
    end: &str,
    legacy_markers: &[(&str, &str)],
    command: &str,
) -> Result<String> {
    let mut regions = Vec::new();
    for (candidate_start, candidate_end) in
        std::iter::once((start, end)).chain(legacy_markers.iter().copied())
    {
        let starts = whole_line_matches(existing, candidate_start);
        let ends = whole_line_matches(existing, candidate_end);
        match (starts.as_slice(), ends.as_slice()) {
            ([], []) => {}
            ([start_index], [end_index]) if start_index < end_index => {
                regions.push((*start_index, *end_index + candidate_end.len()))
            }
            _ => return Err(malformed_markers(path)),
        }
    }
    if regions.len() > 1 {
        return Err(malformed_markers(path));
    }

    let command = normalize_newlines(path, command, newline)?;
    let block = format!("{start}{newline}{command}{newline}{end}{newline}");
    match regions.as_slice() {
        [] => {
            let mut updated = existing.to_owned();
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push_str(newline);
            }
            updated.push_str(&block);
            Ok(updated)
        }
        [(region_start, region_end)] => {
            let suffix = existing[*region_end..]
                .strip_prefix("\r\n")
                .or_else(|| existing[*region_end..].strip_prefix('\n'))
                .unwrap_or(&existing[*region_end..]);
            Ok(format!("{}{block}{suffix}", &existing[..*region_start]))
        }
        _ => unreachable!(),
    }
}

fn remove_marked_block(path: &Path, existing: &str, markers: &[(&str, &str)]) -> Result<String> {
    let mut regions = Vec::new();
    for (start, end) in markers {
        let starts = whole_line_matches(existing, start);
        let ends = whole_line_matches(existing, end);
        match (starts.as_slice(), ends.as_slice()) {
            ([], []) => {}
            ([start_index], [end_index]) if start_index < end_index => {
                let mut region_end = end_index + end.len();
                if existing[region_end..].starts_with("\r\n") {
                    region_end += 2;
                } else if existing[region_end..].starts_with('\n') {
                    region_end += 1;
                }
                regions.push((*start_index, region_end));
            }
            _ => return Err(malformed_markers(path)),
        }
    }
    if regions.len() > 1 {
        return Err(malformed_markers(path));
    }
    let Some((start, end)) = regions.first().copied() else {
        return Ok(existing.to_owned());
    };
    Ok(format!("{}{}", &existing[..start], &existing[end..]))
}

fn whole_line_matches(contents: &str, marker: &str) -> Vec<usize> {
    contents
        .match_indices(marker)
        .filter_map(|(index, _)| {
            let before_is_boundary =
                index == 0 || contents.as_bytes().get(index - 1) == Some(&b'\n');
            let after = index + marker.len();
            let after_is_boundary = after == contents.len()
                || contents[after..].starts_with('\n')
                || contents[after..].starts_with("\r\n");
            (before_is_boundary && after_is_boundary).then_some(index)
        })
        .collect()
}

fn decode_profile(
    path: &Path,
    contents: &[u8],
    format: ProfileFormat,
) -> Result<(String, TextEncoding)> {
    const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];
    const UTF16_LE_BOM: &[u8] = &[0xff, 0xfe];
    const UTF16_BE_BOM: &[u8] = &[0xfe, 0xff];
    const UTF32_LE_BOM: &[u8] = &[0xff, 0xfe, 0x00, 0x00];
    const UTF32_BE_BOM: &[u8] = &[0x00, 0x00, 0xfe, 0xff];

    if contents.starts_with(UTF32_LE_BOM) || contents.starts_with(UTF32_BE_BOM) {
        return Err(unsupported_encoding(path));
    }
    let (decoded, encoding) = match format {
        ProfileFormat::Utf8NoBom => {
            if contents.starts_with(UTF8_BOM)
                || contents.starts_with(UTF16_LE_BOM)
                || contents.starts_with(UTF16_BE_BOM)
            {
                return Err(unsupported_encoding(path));
            }
            (
                std::str::from_utf8(contents)
                    .map_err(|_| unsupported_encoding(path))?
                    .to_owned(),
                TextEncoding::Utf8,
            )
        }
        ProfileFormat::PowerShell if contents.starts_with(UTF8_BOM) => (
            std::str::from_utf8(&contents[UTF8_BOM.len()..])
                .map_err(|_| unsupported_encoding(path))?
                .to_owned(),
            TextEncoding::Utf8Bom,
        ),
        ProfileFormat::PowerShell if contents.starts_with(UTF16_LE_BOM) => (
            decode_utf16(path, &contents[UTF16_LE_BOM.len()..], true)?,
            TextEncoding::Utf16LeBom,
        ),
        ProfileFormat::PowerShell if contents.starts_with(UTF16_BE_BOM) => (
            decode_utf16(path, &contents[UTF16_BE_BOM.len()..], false)?,
            TextEncoding::Utf16BeBom,
        ),
        ProfileFormat::PowerShell => (
            std::str::from_utf8(contents)
                .map_err(|_| unsupported_encoding(path))?
                .to_owned(),
            TextEncoding::Utf8,
        ),
    };
    if decoded.contains('\0') {
        return Err(unsupported_encoding(path));
    }
    Ok((decoded, encoding))
}

fn decode_utf16(path: &Path, contents: &[u8], little_endian: bool) -> Result<String> {
    if !contents.len().is_multiple_of(2) {
        return Err(unsupported_encoding(path));
    }
    let units = contents
        .chunks_exact(2)
        .map(|bytes| {
            if little_endian {
                u16::from_le_bytes([bytes[0], bytes[1]])
            } else {
                u16::from_be_bytes([bytes[0], bytes[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| unsupported_encoding(path))
}

fn encode_profile(contents: &str, encoding: TextEncoding) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 => contents.as_bytes().to_vec(),
        TextEncoding::Utf8Bom => [b"\xef\xbb\xbf".as_slice(), contents.as_bytes()].concat(),
        TextEncoding::Utf16LeBom => {
            let mut encoded = vec![0xff, 0xfe];
            for unit in contents.encode_utf16() {
                encoded.extend_from_slice(&unit.to_le_bytes());
            }
            encoded
        }
        TextEncoding::Utf16BeBom => {
            let mut encoded = vec![0xfe, 0xff];
            for unit in contents.encode_utf16() {
                encoded.extend_from_slice(&unit.to_be_bytes());
            }
            encoded
        }
    }
}

fn newline_style(path: &Path, contents: &str, format: ProfileFormat) -> Result<&'static str> {
    let bytes = contents.as_bytes();
    let mut first = None;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                first.get_or_insert("\r\n");
                index += 2;
            }
            b'\r' => {
                return Err(AvmError::Message(format!(
                    "refusing to edit shell profile {} with bare carriage-return newlines",
                    path.display()
                )));
            }
            b'\n' => {
                first.get_or_insert("\n");
                index += 1;
            }
            _ => index += 1,
        }
    }
    let default = if cfg!(windows) && format == ProfileFormat::PowerShell {
        "\r\n"
    } else {
        "\n"
    };
    Ok(first.unwrap_or(default))
}

fn normalize_newlines(path: &Path, command: &str, newline: &str) -> Result<String> {
    let normalized = command.replace("\r\n", "\n");
    if normalized.contains('\r') {
        return Err(AvmError::Message(format!(
            "refusing to write a generated profile command with bare carriage returns to {}",
            path.display()
        )));
    }
    Ok(normalized.replace('\n', newline))
}

fn unsupported_encoding(path: &Path) -> AvmError {
    AvmError::Message(format!(
        "refusing to edit shell profile {} with an unsupported or invalid encoding",
        path.display()
    ))
}

fn malformed_markers(path: &Path) -> AvmError {
    AvmError::Message(format!(
        "shell profile {} contains malformed, duplicate, or conflicting AVM markers",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT_START: &str = "# >>> avm init >>>";
    const INIT_END: &str = "# <<< avm init <<<";
    const SETUP_START: &str = "# >>> avm setup >>>";
    const SETUP_END: &str = "# <<< avm setup <<<";
    const COMPLETION_START: &str = "# >>> avm completion >>>";
    const COMPLETION_END: &str = "# <<< avm completion <<<";

    fn request(path: &Path, start: &'static str, end: &'static str, command: &str) -> BlockRequest {
        BlockRequest {
            path: path.to_path_buf(),
            start,
            end,
            legacy_markers: Vec::new(),
            command: command.to_owned(),
            format: ProfileFormat::Utf8NoBom,
        }
    }

    fn powershell_request(path: &Path, command: &str) -> BlockRequest {
        let mut request = request(path, COMPLETION_START, COMPLETION_END, command);
        request.format = ProfileFormat::PowerShell;
        request
    }

    #[test]
    fn plans_multiple_blocks_for_one_profile_without_losing_either() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join(".bashrc");
        fs::write(&profile, "# before\n").unwrap();

        let updates = plan_updates(vec![
            request(&profile, INIT_START, INIT_END, "export PATH=/safe"),
            request(
                &profile,
                COMPLETION_START,
                COMPLETION_END,
                ". /safe/avm.bash",
            ),
        ])
        .unwrap();
        apply_updates(updates).unwrap();

        let contents = fs::read_to_string(profile).unwrap();
        assert!(contents.contains(INIT_START));
        assert!(contents.contains(COMPLETION_START));
        assert!(contents.starts_with("# before\n"));
    }

    #[test]
    fn migrates_one_legacy_block_and_refuses_conflicting_markers() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join(".zshrc");
        fs::write(&profile, format!("{SETUP_START}\nold\n{SETUP_END}\n")).unwrap();
        let update = BlockRequest {
            path: profile.clone(),
            start: INIT_START,
            end: INIT_END,
            legacy_markers: vec![(SETUP_START, SETUP_END)],
            command: "new".to_owned(),
            format: ProfileFormat::Utf8NoBom,
        };
        apply_updates(plan_updates(vec![update.clone()]).unwrap()).unwrap();
        let contents = fs::read_to_string(&profile).unwrap();
        assert!(!contents.contains(SETUP_START));
        assert_eq!(contents.matches(INIT_START).count(), 1);
        assert!(contents.contains("new"));

        fs::write(
            &profile,
            format!("{INIT_START}\na\n{INIT_END}\n{SETUP_START}\nb\n{SETUP_END}\n"),
        )
        .unwrap();
        assert!(plan_updates(vec![update]).is_err());
    }

    #[test]
    fn initialization_ignores_marker_text_that_is_not_a_whole_line() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join(".bashrc");
        let original = format!("echo '{INIT_START}'\necho user-content\necho '{INIT_END}'\n");
        fs::write(&profile, &original).unwrap();

        apply_updates(
            plan_updates(vec![request(&profile, INIT_START, INIT_END, "managed")]).unwrap(),
        )
        .unwrap();

        let contents = fs::read_to_string(&profile).unwrap();
        assert!(contents.starts_with(&original));
        assert!(contents.contains(&format!("{INIT_START}\nmanaged\n{INIT_END}\n")));
    }

    #[test]
    fn plans_both_windows_powershell_profile_families() {
        let documents = Path::new(r"C:\Users\Example\Documents");
        assert_eq!(
            windows_powershell_profiles(documents),
            vec![
                documents.join("WindowsPowerShell/profile.ps1"),
                documents.join("PowerShell/profile.ps1"),
            ]
        );
    }

    #[test]
    fn preserves_powershell_bom_encodings_and_crlf_for_multiline_blocks() {
        for encoding in [
            TextEncoding::Utf8Bom,
            TextEncoding::Utf16LeBom,
            TextEncoding::Utf16BeBom,
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let profile = temporary.path().join("profile.ps1");
            fs::write(
                &profile,
                encode_profile("Write-Output 'π 😀'\r\n", encoding),
            )
            .unwrap();

            apply_updates(
                plan_updates(vec![powershell_request(
                    &profile,
                    "first command\nsecond command",
                )])
                .unwrap(),
            )
            .unwrap();

            let bytes = fs::read(&profile).unwrap();
            let (decoded, actual_encoding) =
                decode_profile(&profile, &bytes, ProfileFormat::PowerShell).unwrap();
            assert_eq!(actual_encoding, encoding);
            assert!(decoded.contains("π 😀"));
            assert!(decoded.contains("first command\r\nsecond command"));
            assert!(!decoded.replace("\r\n", "").contains('\n'));
            assert!(
                plan_updates(vec![powershell_request(
                    &profile,
                    "first command\nsecond command",
                )])
                .unwrap()
                .is_empty()
            );
        }
    }

    #[test]
    fn new_powershell_profiles_use_a_utf8_bom() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join("profile.ps1");
        apply_updates(
            plan_updates(vec![powershell_request(&profile, "Register-Something")]).unwrap(),
        )
        .unwrap();

        assert!(fs::read(profile).unwrap().starts_with(&[0xef, 0xbb, 0xbf]));
    }

    #[test]
    fn posix_profiles_reject_boms_and_conflicting_format_policies() {
        for encoding in [
            TextEncoding::Utf8Bom,
            TextEncoding::Utf16LeBom,
            TextEncoding::Utf16BeBom,
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let profile = temporary.path().join(".bashrc");
            let original = encode_profile("echo safe\n", encoding);
            fs::write(&profile, &original).unwrap();
            assert!(plan_updates(vec![request(&profile, INIT_START, INIT_END, "new")]).is_err());
            assert_eq!(fs::read(profile).unwrap(), original);
        }

        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join("profile.ps1");
        let error = plan_updates(vec![
            request(&profile, INIT_START, INIT_END, "path"),
            powershell_request(&profile, "completion"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("conflicting profile formats"));
    }

    #[test]
    fn profile_batch_preflight_prevents_a_partial_concurrent_overwrite() {
        let temporary = tempfile::tempdir().unwrap();
        let first = temporary.path().join(".bashrc");
        let second = temporary.path().join(".bash_profile");
        fs::write(&first, "first before\n").unwrap();
        fs::write(&second, "second before\n").unwrap();
        let updates = plan_updates(vec![
            request(&first, INIT_START, INIT_END, "path"),
            request(&second, INIT_START, INIT_END, "path"),
        ])
        .unwrap();
        fs::write(&second, "concurrent\n").unwrap();

        assert!(matches!(
            apply_updates(updates),
            Err(AvmError::ConcurrentFileChange { .. })
        ));
        assert_eq!(fs::read_to_string(first).unwrap(), "first before\n");
        assert_eq!(fs::read_to_string(second).unwrap(), "concurrent\n");
    }

    #[test]
    fn rejects_malformed_utf16_and_bomless_utf16_powershell_profiles() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join("profile.ps1");
        fs::write(&profile, [0xff, 0xfe, 0x61]).unwrap();
        assert!(plan_updates(vec![powershell_request(&profile, "new")]).is_err());

        fs::write(&profile, [b'a', 0, b'b', 0]).unwrap();
        assert!(plan_updates(vec![powershell_request(&profile, "new")]).is_err());
    }

    #[test]
    fn removes_only_exact_managed_blocks_and_is_idempotent() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join(".bashrc");
        fs::write(
            &profile,
            format!(
                "# user before\n{INIT_START}\npath\n{INIT_END}\n\
                 # mention {COMPLETION_START} inline\n{COMPLETION_START}\ncomplete\n{COMPLETION_END}\n# user after\n"
            ),
        )
        .unwrap();
        let removals = vec![
            BlockRemovalRequest {
                path: profile.clone(),
                markers: vec![(INIT_START, INIT_END), (SETUP_START, SETUP_END)],
                format: ProfileFormat::Utf8NoBom,
            },
            BlockRemovalRequest {
                path: profile.clone(),
                markers: vec![(COMPLETION_START, COMPLETION_END)],
                format: ProfileFormat::Utf8NoBom,
            },
        ];

        apply_updates(plan_removals(removals.clone()).unwrap()).unwrap();
        let contents = fs::read_to_string(&profile).unwrap();
        assert_eq!(
            contents,
            format!("# user before\n# mention {COMPLETION_START} inline\n# user after\n")
        );
        assert!(plan_removals(removals).unwrap().is_empty());
    }

    #[test]
    fn removal_skips_unrelated_oversized_and_invalid_profiles() {
        let temporary = tempfile::tempdir().unwrap();
        let oversized = temporary.path().join(".zshrc");
        let invalid = temporary.path().join(".profile");
        let directory = temporary.path().join("config.fish");
        let mut oversized_contents = format!("echo '{INIT_START}'\n").into_bytes();
        oversized_contents.resize(MAX_PROFILE_BYTES as usize + 1, b'x');
        fs::write(&oversized, oversized_contents).unwrap();
        let invalid_contents = [
            &[0xff, 0x00, 0xfe][..],
            format!(" inline {INIT_START}\n").as_bytes(),
        ]
        .concat();
        fs::write(&invalid, &invalid_contents).unwrap();
        fs::create_dir(&directory).unwrap();

        let requests = [oversized.clone(), invalid.clone(), directory]
            .into_iter()
            .map(|path| BlockRemovalRequest {
                path,
                markers: vec![(INIT_START, INIT_END)],
                format: ProfileFormat::Utf8NoBom,
            })
            .collect();

        assert!(plan_removals(requests).unwrap().is_empty());
        assert_eq!(
            fs::metadata(oversized).unwrap().len(),
            MAX_PROFILE_BYTES + 1
        );
        assert_eq!(fs::read(invalid).unwrap(), invalid_contents);
    }

    #[test]
    fn removal_does_not_silently_skip_an_oversized_profile_with_exact_markers() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join(".bashrc");
        let mut contents = format!("{INIT_START}\nmanaged\n{INIT_END}\n").into_bytes();
        contents.resize(MAX_PROFILE_BYTES as usize + 1, b'x');
        fs::write(&profile, contents).unwrap();
        let request = BlockRemovalRequest {
            path: profile,
            markers: vec![(INIT_START, INIT_END)],
            format: ProfileFormat::Utf8NoBom,
        };

        assert!(plan_removals(vec![request]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn removal_preserves_an_unrelated_symlinked_profile() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let profile = temporary.path().join(".zshrc");
        fs::write(&target, "user profile\n").unwrap();
        std::os::unix::fs::symlink(&target, &profile).unwrap();
        let request = BlockRemovalRequest {
            path: profile.clone(),
            markers: vec![(INIT_START, INIT_END)],
            format: ProfileFormat::Utf8NoBom,
        };

        assert!(plan_removals(vec![request]).unwrap().is_empty());
        assert!(
            fs::symlink_metadata(profile)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "user profile\n");
    }

    #[test]
    fn removal_preserves_powershell_encoding_and_crlf() {
        for encoding in [
            TextEncoding::Utf8Bom,
            TextEncoding::Utf16LeBom,
            TextEncoding::Utf16BeBom,
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let profile = temporary.path().join("profile.ps1");
            let contents = format!(
                "Write-Output 'before'\r\n{COMPLETION_START}\r\ncomplete\r\n{COMPLETION_END}\r\nWrite-Output 'after'\r\n"
            );
            fs::write(&profile, encode_profile(&contents, encoding)).unwrap();
            let request = BlockRemovalRequest {
                path: profile.clone(),
                markers: vec![(COMPLETION_START, COMPLETION_END)],
                format: ProfileFormat::PowerShell,
            };

            apply_updates(plan_removals(vec![request]).unwrap()).unwrap();
            let bytes = fs::read(&profile).unwrap();
            let (decoded, actual_encoding) =
                decode_profile(&profile, &bytes, ProfileFormat::PowerShell).unwrap();
            assert_eq!(actual_encoding, encoding);
            assert_eq!(
                decoded, "Write-Output 'before'\r\nWrite-Output 'after'\r\n",
                "encoding: {encoding:?}"
            );
        }
    }

    #[test]
    fn removal_refuses_unmatched_or_duplicate_markers() {
        for contents in [
            format!("{INIT_START}\nmissing end\n"),
            format!("{INIT_START}\na\n{INIT_END}\n{INIT_START}\nb\n{INIT_END}\n"),
            format!("{INIT_START}\na\n{INIT_END}\n{SETUP_START}\nb\n{SETUP_END}\n"),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let profile = temporary.path().join(".profile");
            fs::write(&profile, &contents).unwrap();
            let request = BlockRemovalRequest {
                path: profile.clone(),
                markers: vec![(INIT_START, INIT_END), (SETUP_START, SETUP_END)],
                format: ProfileFormat::Utf8NoBom,
            };
            assert!(plan_removals(vec![request]).is_err());
            assert_eq!(fs::read_to_string(profile).unwrap(), contents);
        }
    }
}
