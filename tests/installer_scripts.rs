use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs;

const RELEASE_PLACEHOLDER: &str = "@AVM_RELEASE_VERSION@";
const POSIX_TEMPLATE: &str = include_str!("../scripts/install.sh.in");
const POWERSHELL_TEMPLATE: &str = include_str!("../scripts/install.ps1.in");

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    encoded
}

#[test]
fn installer_templates_have_one_release_placeholder() {
    assert_eq!(POSIX_TEMPLATE.matches(RELEASE_PLACEHOLDER).count(), 1);
    assert_eq!(POWERSHELL_TEMPLATE.matches(RELEASE_PLACEHOLDER).count(), 1);
    assert!(POSIX_TEMPLATE.starts_with("#!/bin/sh\n"));
    assert!(POWERSHELL_TEMPLATE.starts_with("& {\n    [CmdletBinding()]\n"));
}

#[cfg(windows)]
#[test]
fn powershell_installer_rejects_an_unpublished_architecture_before_download() {
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let installer = temp.path().join("install.ps1");
    fs::write(
        &installer,
        POWERSHELL_TEMPLATE.replace(
            RELEASE_PLACEHOLDER,
            &format!("v{}", env!("CARGO_PKG_VERSION")),
        ),
    )
    .unwrap();
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&installer)
        .args(["-NoInit", "-AvmHome"])
        .arg(temp.path().join("avm home"))
        .env("OS", "Windows_NT")
        .env("PROCESSOR_ARCHITECTURE", "ARM64")
        .env_remove("PROCESSOR_ARCHITEW6432")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        message.contains("no AVM binary is published for windows/ARM64"),
        "{message}"
    );
    assert!(!temp.path().join("avm home").exists());
}

#[cfg(windows)]
#[test]
fn powershell_installer_verifies_installs_and_replaces_only_managed_avm() {
    use std::process::{Command, Output};

    const CHECKSUM_DOWNLOAD: &str = "        Save-GitHubReleaseFile -Client $client -Uri \"$releaseRoot/$asset.sha256\" -Destination $stagedChecksum -MaximumBytes $MaximumChecksumBytes";
    const BINARY_DOWNLOAD: &str = "        Save-GitHubReleaseFile -Client $client -Uri \"$releaseRoot/$asset\" -Destination $stagedBinary -MaximumBytes $MaximumBinaryBytes";

    let temp = tempfile::tempdir().unwrap();
    let installer = temp.path().join("install.ps1");
    let mut rendered = POWERSHELL_TEMPLATE.replace(
        RELEASE_PLACEHOLDER,
        &format!("v{}", env!("CARGO_PKG_VERSION")),
    );
    assert_eq!(rendered.matches(CHECKSUM_DOWNLOAD).count(), 1);
    assert_eq!(rendered.matches(BINARY_DOWNLOAD).count(), 1);
    rendered = rendered.replace(
        CHECKSUM_DOWNLOAD,
        "        Copy-Item -LiteralPath $env:AVM_TEST_CHECKSUM -Destination $stagedChecksum",
    );
    rendered = rendered.replace(
        BINARY_DOWNLOAD,
        "        Copy-Item -LiteralPath $env:AVM_TEST_BINARY -Destination $stagedBinary",
    );
    fs::write(&installer, &rendered).unwrap();

    let release_binary = fs::read(env!("CARGO_BIN_EXE_avm")).unwrap();
    let release_digest = sha256_hex(&release_binary);
    let checksum = temp.path().join("release checksum");
    fs::write(
        &checksum,
        format!("{release_digest}  avm-windows-amd64.exe"),
    )
    .unwrap();
    let avm_home = temp.path().join("avm home");

    let run = || -> Output {
        Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&installer)
            .args(["-NoInit", "-AvmHome"])
            .arg(&avm_home)
            .env("AVM_TEST_CHECKSUM", &checksum)
            .env("AVM_TEST_BINARY", env!("CARGO_BIN_EXE_avm"))
            .output()
            .unwrap()
    };

    for _ in 0..2 {
        let output = run();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let destination = avm_home.join("bin/avm.exe");
    let marker = avm_home.join("bin/avm.exe.sha256");
    assert_eq!(fs::read(&destination).unwrap(), release_binary);
    assert!(marker.is_file());

    let previous = b"previous managed AVM";
    fs::write(&destination, previous).unwrap();
    fs::write(&marker, format!("{}  avm.exe\n", sha256_hex(previous))).unwrap();
    let upgraded = run();
    assert!(
        upgraded.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&upgraded.stdout),
        String::from_utf8_lossy(&upgraded.stderr)
    );
    assert_eq!(
        fs::read(&destination).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_avm")).unwrap()
    );

    fs::write(
        &checksum,
        format!("{}  avm-windows-amd64.exe", "0".repeat(64)),
    )
    .unwrap();
    let before = fs::read(&destination).unwrap();
    let rejected = run();
    assert!(!rejected.status.success());
    assert_eq!(fs::read(destination).unwrap(), before);

    fs::write(
        &checksum,
        format!("{release_digest}  avm-windows-amd64.exe"),
    )
    .unwrap();
    let init_call = "        & $destination @initArguments";
    assert_eq!(rendered.matches(init_call).count(), 1);
    let init_arguments_log = temp.path().join("init arguments");
    let init_failure_installer = temp.path().join("install-init-failure.ps1");
    fs::write(
        &init_failure_installer,
        rendered.replace(
            init_call,
            "        [IO.File]::WriteAllLines($env:AVM_TEST_INIT_ARGUMENTS, [string[]] $initArguments)\n        & $env:ComSpec /d /c 'echo simulated init failure 1>&2 & exit /b 7'",
        ),
    )
    .unwrap();
    let failed_init_home = temp.path().join("failed init home");
    let failed_init = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&init_failure_installer)
        .args(["-NoCompletion", "-AvmHome"])
        .arg(&failed_init_home)
        .env("AVM_TEST_CHECKSUM", &checksum)
        .env("AVM_TEST_BINARY", env!("CARGO_BIN_EXE_avm"))
        .env("AVM_TEST_INIT_ARGUMENTS", &init_arguments_log)
        .output()
        .unwrap();
    assert!(!failed_init.status.success());
    let failed_init_message = format!(
        "{}{}",
        String::from_utf8_lossy(&failed_init.stdout),
        String::from_utf8_lossy(&failed_init.stderr)
    );
    assert!(
        failed_init_message.contains("was installed, but shell initialization failed"),
        "{failed_init_message}"
    );
    assert!(failed_init_home.join("bin/avm.exe").is_file());
    assert_eq!(
        fs::read_to_string(&init_arguments_log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "--avm-home",
            failed_init_home.to_str().unwrap(),
            "init",
            "--shell",
            "powershell",
            "--no-completion"
        ]
    );

    let no_init_parameter = "        [switch] $NoInit,";
    assert_eq!(rendered.matches(no_init_parameter).count(), 1);
    let iex_installer = temp.path().join("install-iex.ps1");
    fs::write(
        &iex_installer,
        rendered.replace(no_init_parameter, "        [switch] $NoInit = $true,"),
    )
    .unwrap();
    let iex_runner = temp.path().join("invoke-installer.ps1");
    fs::write(
        &iex_runner,
        r#"$ErrorActionPreference = 'Continue'
Set-StrictMode -Off
$beforeErrorActionPreference = $ErrorActionPreference
Invoke-Expression ([IO.File]::ReadAllText($env:AVM_TEST_INSTALLER))
if ($ErrorActionPreference -cne $beforeErrorActionPreference) {
    throw 'ErrorActionPreference leaked from installer'
}
if (Get-Command Assert-GitHubDownloadUri -ErrorAction SilentlyContinue) {
    throw 'installer function leaked into caller scope'
}
if (Get-Variable Repository -Scope Local -ErrorAction SilentlyContinue) {
    throw 'installer variable leaked into caller scope'
}
$null = $AVM_INSTALLER_UNDEFINED_SCOPE_PROBE
"#,
    )
    .unwrap();
    let iex_home = temp.path().join("iex home");
    let iex_output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&iex_runner)
        .env("AVM_HOME", &iex_home)
        .env("AVM_TEST_INSTALLER", &iex_installer)
        .env("AVM_TEST_CHECKSUM", &checksum)
        .env("AVM_TEST_BINARY", env!("CARGO_BIN_EXE_avm"))
        .output()
        .unwrap();
    assert!(
        iex_output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&iex_output.stdout),
        String::from_utf8_lossy(&iex_output.stderr)
    );
    assert!(iex_home.join("bin/avm.exe").is_file());
}

#[cfg(windows)]
#[test]
fn powershell_installer_rejects_relative_avm_home_before_download() {
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let installer = temp.path().join("install.ps1");
    fs::write(
        &installer,
        POWERSHELL_TEMPLATE.replace(
            RELEASE_PLACEHOLDER,
            &format!("v{}", env!("CARGO_PKG_VERSION")),
        ),
    )
    .unwrap();
    for relative_home in ["relative avm home", "C:relative", "\\relative"] {
        let output = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&installer)
            .args(["-NoInit", "-AvmHome", relative_home])
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(message.contains("absolute filesystem path"), "{message}");
    }
    assert!(!temp.path().join("relative avm home").exists());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::env;
    use std::ffi::OsString;
    use std::io::Write as _;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};

    struct InstallerFixture {
        temp: tempfile::TempDir,
        installer: PathBuf,
        tools: PathBuf,
        checksum: PathBuf,
        curl_log: PathBuf,
        release_binary: PathBuf,
        avm_home: PathBuf,
        profile_home: PathBuf,
        uname_os: String,
        uname_arch: String,
    }

    impl InstallerFixture {
        fn new(checksum_record: Option<String>) -> Self {
            Self::for_platform("Linux", "x86_64", "avm-linux-amd64", checksum_record)
        }

        fn for_platform(
            uname_os: &str,
            uname_arch: &str,
            asset: &str,
            checksum_record: Option<String>,
        ) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let installer = temp.path().join("install.sh");
            let rendered = POSIX_TEMPLATE.replace(
                RELEASE_PLACEHOLDER,
                &format!("v{}", env!("CARGO_PKG_VERSION")),
            );
            fs::write(&installer, rendered).unwrap();
            make_executable(&installer);

            let tools = temp.path().join("test tools");
            fs::create_dir(&tools).unwrap();
            write_executable(
                &tools.join("uname"),
                r#"#!/bin/sh
case "${1-}" in
    -s) printf '%s\n' "$AVM_TEST_UNAME_OS" ;;
    -m) printf '%s\n' "$AVM_TEST_UNAME_ARCH" ;;
    *) printf '%s\n' "$AVM_TEST_UNAME_OS" ;;
esac
"#,
            );
            write_executable(
                &tools.join("curl"),
                r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$AVM_TEST_CURL_LOG"
destination=''
url=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            shift
            destination=$1
            ;;
        https://*) url=$1 ;;
    esac
    shift
done
[ -n "$destination" ]
case "$url" in
    *.sha256) cp "$AVM_TEST_CHECKSUM" "$destination" ;;
    */avm-*) cp "$AVM_TEST_RELEASE_BINARY" "$destination" ;;
    *) exit 22 ;;
esac
"#,
            );

            let release_binary = temp.path().join("release avm");
            write_executable(
                &release_binary,
                &format!(
                    "#!/bin/sh\n\
                     if [ \"${{1-}}\" = '--version' ]; then\n\
                         printf 'avm {}\\n'\n\
                         exit 0\n\
                     fi\n\
                     if [ \"${{1-}}\" = 'init' ] && [ \"${{AVM_TEST_SUPPORTS_INIT-1}}\" = '0' ]; then\n\
                         exit 2\n\
                     fi\n\
                     exec \"$AVM_TEST_REAL_BINARY\" \"$@\"\n",
                    env!("CARGO_PKG_VERSION")
                ),
            );
            let digest = sha256_hex(&fs::read(&release_binary).unwrap());
            let checksum = temp.path().join("release checksum");
            fs::write(
                &checksum,
                checksum_record.unwrap_or_else(|| format!("{digest}  {asset}")),
            )
            .unwrap();
            let curl_log = temp.path().join("curl arguments");

            let avm_home = temp.path().join("avm home");
            let profile_home = temp.path().join("profile home");
            fs::create_dir(&profile_home).unwrap();

            Self {
                temp,
                installer,
                tools,
                checksum,
                curl_log,
                release_binary,
                avm_home,
                profile_home,
                uname_os: uname_os.to_owned(),
                uname_arch: uname_arch.to_owned(),
            }
        }

        fn base_command(&self) -> Command {
            let mut command = Command::new("sh");
            command
                .env("AVM_HOME", &self.avm_home)
                .env("HOME", &self.profile_home)
                .env("SHELL", "/bin/bash")
                .env("AVM_TEST_RELEASE_BINARY", &self.release_binary)
                .env("AVM_TEST_REAL_BINARY", env!("CARGO_BIN_EXE_avm"))
                .env("AVM_TEST_CHECKSUM", &self.checksum)
                .env("AVM_TEST_CURL_LOG", &self.curl_log)
                .env("AVM_TEST_UNAME_OS", &self.uname_os)
                .env("AVM_TEST_UNAME_ARCH", &self.uname_arch)
                .env("PATH", path_with_test_tools(&self.tools));
            command
        }

        fn command(&self, arguments: &[&str]) -> Command {
            let mut command = self.base_command();
            command.arg(&self.installer).args(arguments);
            command
        }

        fn run(&self, arguments: &[&str]) -> Output {
            self.command(arguments).output().unwrap()
        }

        fn run_via_stdin(&self, arguments: &[&str]) -> Output {
            let mut command = self.base_command();
            command
                .arg("-s")
                .arg("--")
                .args(arguments)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(&fs::read(&self.installer).unwrap())
                .unwrap();
            child.wait_with_output().unwrap()
        }
    }

    fn make_executable(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        make_executable(path);
    }

    fn path_with_test_tools(tools: &Path) -> OsString {
        let mut entries = vec![tools.to_path_buf()];
        if let Some(path) = env::var_os("PATH") {
            entries.extend(env::split_paths(&path));
        }
        env::join_paths(entries).unwrap()
    }

    fn output_details(output: &Output) -> String {
        format!(
            "status: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    #[test]
    fn installs_and_initializes_idempotently_in_a_path_with_spaces() {
        let fixture = InstallerFixture::new(None);

        for _ in 0..2 {
            let output = fixture.run(&[]);
            assert!(output.status.success(), "{}", output_details(&output));
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("avm default stable"),
                "{}",
                output_details(&output)
            );
        }

        assert!(fixture.avm_home.join("bin/avm").is_file());
        assert!(fixture.avm_home.join("bin/avm.sha256").is_file());
        assert!(fixture.avm_home.join("bin/argocd").is_file());
        assert!(!fixture.avm_home.join("state/default").exists());
        let bashrc = fs::read_to_string(fixture.profile_home.join(".bashrc")).unwrap();
        assert_eq!(bashrc.matches("# >>> avm init >>>").count(), 1);
        assert_eq!(bashrc.matches("# >>> avm completion >>>").count(), 1);
    }

    #[test]
    fn no_init_installs_only_avm_and_its_marker() {
        let fixture = InstallerFixture::new(None);
        let output = fixture.run_via_stdin(&["--version", env!("CARGO_PKG_VERSION"), "--no-init"]);
        assert!(output.status.success(), "{}", output_details(&output));
        assert!(fixture.avm_home.join("bin/avm").is_file());
        assert!(fixture.avm_home.join("bin/avm.sha256").is_file());
        assert!(!fixture.avm_home.join("bin/argocd").exists());
        assert!(!fixture.profile_home.join(".bashrc").exists());

        let curl_arguments = fs::read_to_string(&fixture.curl_log).unwrap();
        assert!(curl_arguments.contains("--proto =https"));
        assert!(curl_arguments.contains("-q --proto =https"));
        assert!(curl_arguments.contains("--proto-redir =https"));
        assert!(curl_arguments.contains("--tlsv1.2"));
        assert!(curl_arguments.contains("--connect-timeout 10"));
        assert!(curl_arguments.contains("--max-time 120"));
        assert!(curl_arguments.contains(&format!(
            "https://github.com/Moncefmd/avm/releases/download/v{}/avm-linux-amd64.sha256",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(curl_arguments.contains(&format!(
            "https://github.com/Moncefmd/avm/releases/download/v{}/avm-linux-amd64",
            env!("CARGO_PKG_VERSION")
        )));
    }

    #[test]
    fn legacy_no_init_recommends_the_legacy_setup_command() {
        let fixture = InstallerFixture::new(None);
        let output = fixture
            .command(&["--no-init"])
            .env("AVM_TEST_SUPPORTS_INIT", "0")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", output_details(&output));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("legacy release uses setup"), "{stdout}");
        assert!(!stdout.contains(" init when you are ready"), "{stdout}");
    }

    #[test]
    fn explicit_shell_can_skip_completion_without_skipping_initialization() {
        let fixture = InstallerFixture::new(None);
        let output = fixture.run(&["--shell", "bash", "--no-completion"]);
        assert!(output.status.success(), "{}", output_details(&output));
        assert!(fixture.avm_home.join("bin/argocd").is_file());
        let bashrc = fs::read_to_string(fixture.profile_home.join(".bashrc")).unwrap();
        assert!(bashrc.contains("# >>> avm init >>>"));
        assert!(!bashrc.contains("# >>> avm completion >>>"));
    }

    #[test]
    fn maps_every_published_posix_target_and_rejects_unsupported_hosts() {
        for (os, architecture, asset) in [
            ("Linux", "x86_64", "avm-linux-amd64"),
            ("Linux", "aarch64", "avm-linux-arm64"),
            ("Darwin", "x86_64", "avm-darwin-amd64"),
            ("Darwin", "arm64", "avm-darwin-arm64"),
        ] {
            let fixture = InstallerFixture::for_platform(os, architecture, asset, None);
            let output = fixture.run(&["--no-init"]);
            assert!(output.status.success(), "{}", output_details(&output));
        }

        for (os, architecture) in [("Linux", "armv7"), ("FreeBSD", "x86_64")] {
            let fixture =
                InstallerFixture::for_platform(os, architecture, "unused-unsupported-asset", None);
            let output = fixture.run(&["--no-init"]);
            assert!(!output.status.success(), "{}", output_details(&output));
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("no AVM binary is published"),
                "{}",
                output_details(&output)
            );
            assert!(!fixture.avm_home.exists());
        }
    }

    #[test]
    fn checksum_mismatch_preserves_the_existing_installation() {
        let fixture = InstallerFixture::new(Some(format!("{}  avm-linux-amd64", "0".repeat(64))));
        fs::create_dir_all(fixture.avm_home.join("bin")).unwrap();
        fs::write(fixture.avm_home.join("bin/avm"), b"existing").unwrap();

        let output = fixture.run(&["--no-init"]);
        assert!(!output.status.success(), "{}", output_details(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("checksum mismatch"),
            "{}",
            output_details(&output)
        );
        assert_eq!(
            fs::read(fixture.avm_home.join("bin/avm")).unwrap(),
            b"existing"
        );
    }

    #[test]
    fn checksum_filename_must_match_the_selected_asset() {
        let fixture = InstallerFixture::new(None);
        let digest = sha256_hex(&fs::read(&fixture.release_binary).unwrap());
        fs::write(&fixture.checksum, format!("{digest}  avm-linux-arm64")).unwrap();

        let output = fixture.run(&["--no-init"]);
        assert!(!output.status.success(), "{}", output_details(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("malformed checksum record"),
            "{}",
            output_details(&output)
        );
        assert!(!fixture.avm_home.join("bin/avm").exists());
    }

    #[test]
    fn unmanaged_destination_requires_force() {
        let fixture = InstallerFixture::new(None);
        fs::create_dir_all(fixture.avm_home.join("bin")).unwrap();
        fs::write(fixture.avm_home.join("bin/avm"), b"personal executable").unwrap();

        let refused = fixture.run(&["--no-init"]);
        assert!(!refused.status.success(), "{}", output_details(&refused));
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("unmanaged"),
            "{}",
            output_details(&refused)
        );
        assert_eq!(
            fs::read(fixture.avm_home.join("bin/avm")).unwrap(),
            b"personal executable"
        );

        let forced = fixture.run(&["--no-init", "--force"]);
        assert!(forced.status.success(), "{}", output_details(&forced));
        assert_eq!(
            fs::read(fixture.avm_home.join("bin/avm")).unwrap(),
            fs::read(&fixture.release_binary).unwrap()
        );
    }

    #[test]
    fn valid_marker_allows_a_managed_upgrade() {
        let fixture = InstallerFixture::new(None);
        let bin = fixture.avm_home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let previous = b"previous managed AVM";
        fs::write(bin.join("avm"), previous).unwrap();
        fs::write(
            bin.join("avm.sha256"),
            format!("{}  avm\n", sha256_hex(previous)),
        )
        .unwrap();

        let output = fixture.run(&["--no-init"]);
        assert!(output.status.success(), "{}", output_details(&output));
        assert_eq!(
            fs::read(bin.join("avm")).unwrap(),
            fs::read(&fixture.release_binary).unwrap()
        );
    }

    #[test]
    fn symlink_destination_is_never_followed() {
        let fixture = InstallerFixture::new(None);
        fs::create_dir_all(fixture.avm_home.join("bin")).unwrap();
        let outside = fixture.temp.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, fixture.avm_home.join("bin/avm")).unwrap();

        let output = fixture.run(&["--no-init", "--force"]);
        assert!(!output.status.success(), "{}", output_details(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("symlinked destination"),
            "{}",
            output_details(&output)
        );
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn version_input_is_validated_before_download() {
        let fixture = InstallerFixture::new(None);
        let output = fixture.run(&["--version", "../../v9.9.9", "--no-init"]);
        assert!(!output.status.success(), "{}", output_details(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("invalid version"),
            "{}",
            output_details(&output)
        );
        assert!(!fixture.avm_home.exists());
    }

    #[test]
    fn avm_home_cannot_alias_the_filesystem_root() {
        let fixture = InstallerFixture::new(None);
        for unsafe_home in ["/", "//", "/.", "/tmp/.."] {
            let output = fixture
                .command(&["--no-init"])
                .env("AVM_HOME", unsafe_home)
                .output()
                .unwrap();
            assert!(!output.status.success(), "{}", output_details(&output));
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("filesystem root")
                    || String::from_utf8_lossy(&output.stderr).contains("path segments"),
                "{}",
                output_details(&output)
            );
        }
        assert!(!fixture.curl_log.exists());
    }
}
