use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use sha2::{Digest, Sha256};

fn avm() -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("avm");
    command
        .env_remove("AVM_GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("AVM_ARGOCD_VERSION");
    command
}

fn avm_process() -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_avm"));
    command
        .env_remove("AVM_GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("AVM_ARGOCD_VERSION");
    command
}

#[test]
fn reports_help_version_and_the_v01_command_surface() {
    avm()
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: avm"))
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("available"))
        .stdout(predicate::str::contains("exec"));

    avm()
        .args(["help", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("install [OPTIONS] [SELECTOR]"));

    avm()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("avm 0.1.0"));

    avm()
        .args(["completion", "powershell"])
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .assert()
        .success()
        .stdout(predicate::str::contains("Register-ArgumentCompleter"));
}

#[test]
fn empty_store_and_path_traversal_are_safe() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let sentinel = temp.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"keep").unwrap();

    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .arg("list")
        .assert()
        .success()
        .stdout("No versions installed yet.\n");

    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .args(["uninstall", "../../"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invalid Argo CD version"));

    assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");

    let corrupt = home.join("versions/v9.9.9");
    std::fs::create_dir_all(&corrupt).unwrap();
    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("v9.9.9"))
        .stdout(predicate::str::contains("corrupt"));
    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .args(["uninstall", "v9.9.9"])
        .assert()
        .success();
    assert!(!corrupt.exists());
}

#[test]
fn setup_dry_run_is_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("missing-avm-home");

    avm()
        .arg("--avm-home")
        .arg(&home)
        .args(["setup", "--shell", "bash", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("AVM setup preview for bash"))
        .stdout(predicate::str::contains("avm setup --shell bash"));

    assert!(
        !home.exists(),
        "a dry run must not create the AVM data directory"
    );
}

#[cfg(unix)]
#[test]
fn setup_applies_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let profile_home = temp.path().join("profile-home");
    std::fs::create_dir(&profile_home).unwrap();

    for _ in 0..2 {
        avm()
            .arg("--avm-home")
            .arg(&home)
            .env("HOME", &profile_home)
            .args(["setup", "--shell", "bash"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Configured bash"));
    }

    let platform = avm::platform::Platform::current().unwrap();
    assert!(home.join("bin").join(platform.binary_name()).is_file());
    let profile = std::fs::read_to_string(profile_home.join(".bashrc")).unwrap();
    assert_eq!(profile.matches("# >>> avm setup >>>").count(), 1);
    assert_eq!(profile.matches("# <<< avm setup <<<").count(), 1);
    let login_profile = std::fs::read_to_string(profile_home.join(".bash_profile")).unwrap();
    assert_eq!(login_profile.matches("# >>> avm setup >>>").count(), 1);
    assert_eq!(login_profile.matches("# <<< avm setup <<<").count(), 1);
}

#[test]
fn install_is_install_only_and_records_verified_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let binary = fixture_binary();
    let digest = hex_sha256(&binary);
    let platform = avm::platform::Platform::current().unwrap();
    let (api_url, server) = release_server(&platform.asset_name(), binary.clone(), &digest);

    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "1.2.3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Argo CD v1.2.3 is installed."))
        .stderr(predicate::str::contains("SHA-256"));

    server.join().unwrap();

    let version_dir = home.join("versions/v1.2.3");
    assert_eq!(
        std::fs::read(version_dir.join(platform.binary_name())).unwrap(),
        binary
    );
    assert!(
        !home.join("state/default").exists(),
        "install must not create a user default"
    );
    assert!(
        !home.join("bin").join(platform.binary_name()).exists(),
        "install must not configure the dispatcher"
    );

    let metadata_path = version_dir.join("install.json");
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(metadata_path).unwrap()).unwrap();
    assert_eq!(metadata["schema"], 1);
    assert_eq!(metadata["version"], "v1.2.3");
    assert_eq!(metadata["verified"], true);
    assert_eq!(metadata["sha256"], digest);
    assert!(
        !metadata.to_string().contains("fixture-secret"),
        "stored metadata must not retain signed URL query parameters"
    );
}

#[test]
fn rejects_checksum_mismatch_without_committing_an_install() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let binary = fixture_binary();
    let published_digest = hex_sha256(b"a different release asset");
    let platform = avm::platform::Platform::current().unwrap();
    let (api_url, server) = release_server(&platform.asset_name(), binary, &published_digest);

    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "v1.2.3"])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("checksum mismatch"));

    server.join().unwrap();
    assert!(!home.join("versions/v1.2.3").exists());
}

#[test]
fn rejects_an_empty_asset_without_committing_an_install() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let platform = avm::platform::Platform::current().unwrap();
    let empty = Vec::new();
    let digest = hex_sha256(&empty);
    let (api_url, server) = release_server(&platform.asset_name(), empty, &digest);

    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "v1.2.3"])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("was empty"));

    server.join().unwrap();
    assert!(!home.join("versions/v1.2.3").exists());
}

#[test]
fn rejects_an_asset_whose_streamed_size_disagrees_with_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let platform = avm::platform::Platform::current().unwrap();
    let binary = fixture_binary();
    let digest = hex_sha256(&binary);
    let advertised_size = binary.len() as u64 + 1;
    let (api_url, server) =
        release_server_with_size(&platform.asset_name(), binary, &digest, advertised_size);

    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "v1.2.3"])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("unexpected size"));

    server.join().unwrap();
    assert!(!home.join("versions/v1.2.3").exists());
}

#[test]
fn metadata_less_install_is_rejected_and_force_repairs_it() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let platform = avm::platform::Platform::current().unwrap();
    let version_dir = home.join("versions/v1.2.3");
    std::fs::create_dir_all(&version_dir).unwrap();
    let binary_path = version_dir.join(platform.binary_name());
    std::fs::write(&binary_path, b"incomplete install").unwrap();
    make_executable_for_test(&binary_path);

    let repaired_binary = fixture_binary();
    let digest = hex_sha256(&repaired_binary);
    let (api_url, metadata_server) = release_metadata_server(&platform.asset_name(), &digest);
    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "v1.2.3"])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("incomplete or corrupt"));
    metadata_server.join().unwrap();

    assert_eq!(std::fs::read(&binary_path).unwrap(), b"incomplete install");
    assert!(!version_dir.join("install.json").exists());
    std::fs::write(
        version_dir.join("install.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1,
            "version": "v1.2.3",
            "asset": platform.asset_name(),
            "sha256": "not-a-sha256-digest",
            "verified": true,
            "source_url": "https://example.invalid/argocd",
            "installed_at": 0
        }))
        .unwrap(),
    )
    .unwrap();

    let (api_url, repair_server) =
        release_server(&platform.asset_name(), repaired_binary.clone(), &digest);
    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "v1.2.3", "--force"])
        .assert()
        .success();
    repair_server.join().unwrap();

    assert_eq!(std::fs::read(binary_path).unwrap(), repaired_binary);
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(version_dir.join("install.json")).unwrap()).unwrap();
    assert_eq!(metadata["sha256"], digest);
}

#[test]
fn major_and_minor_selectors_choose_the_newest_stable_release() {
    let platform = avm::platform::Platform::current().unwrap();
    let binary = fixture_binary();
    let digest = hex_sha256(&binary);

    let major_temp = tempfile::tempdir().unwrap();
    let major_home = major_temp.path().join(".avm");
    let (api_url, major_server) =
        release_list_server(&platform.asset_name(), binary.clone(), &digest, 2);
    avm()
        .arg("--avm-home")
        .arg(&major_home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Argo CD v1.3.0 is installed."))
        .stderr(predicate::str::contains("Resolved 1 to v1.3.0."));
    major_server.join().unwrap();
    assert!(major_home.join("versions/v1.3.0").is_dir());
    assert!(!major_home.join("versions/v1.2.5-rc1").exists());

    let minor_temp = tempfile::tempdir().unwrap();
    let minor_home = minor_temp.path().join(".avm");
    let (api_url, minor_server) = release_list_server(&platform.asset_name(), binary, &digest, 2);
    avm()
        .arg("--avm-home")
        .arg(&minor_home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "1.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Argo CD v1.2.4 is installed."))
        .stderr(predicate::str::contains("Resolved 1.2 to v1.2.4."));
    minor_server.join().unwrap();
    assert!(minor_home.join("versions/v1.2.4").is_dir());
    assert!(!minor_home.join("versions/v1.2.5-rc1").exists());
}

#[test]
fn stable_filter_uses_semver_and_api_prerelease_signals() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let platform = avm::platform::Platform::current().unwrap();
    let binary = fixture_binary();
    let digest = hex_sha256(&binary);
    let (api_url, server) = release_list_server(&platform.asset_name(), binary, &digest, 2);

    let stable_output = avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["available", "stable", "--prerelease", "--json"])
        .output()
        .unwrap();
    assert!(stable_output.status.success());
    let stable: serde_json::Value = serde_json::from_slice(&stable_output.stdout).unwrap();
    let stable_tags = stable["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|release| release["tag_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(!stable_tags.contains(&"v1.2.5-rc1"));
    assert!(!stable_tags.contains(&"v1.2.6"));

    let all_output = avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["available", "1.2", "--prerelease", "--refresh", "--json"])
        .output()
        .unwrap();
    assert!(all_output.status.success());
    let all: serde_json::Value = serde_json::from_slice(&all_output.stdout).unwrap();
    let all_tags = all["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|release| release["tag_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(all_tags.contains(&"v1.2.5-rc1"));
    assert!(all_tags.contains(&"v1.2.6"));

    server.join().unwrap();

    let (api_url, exact_server) = single_release_server("v1.2.5-rc1");
    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["info", "v1.2.5-rc1"])
        .assert()
        .success();
    exact_server.join().unwrap();
}

#[test]
fn default_exact_local_selection_works_offline_and_can_be_unset() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    install_shell_fixture_without_default(&home);

    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["default", "v1.2.3"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Default Argo CD version is now v1.2.3.",
        ))
        .stderr(predicate::str::contains(
            "already installed; using the healthy local install",
        ));

    assert_eq!(
        std::fs::read_to_string(home.join("state/default")).unwrap(),
        "v1.2.3\n"
    );

    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .args(["default", "--unset"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Removed the default Argo CD version.",
        ));
    assert!(!home.join("state/default").exists());
    assert!(home.join("versions/v1.2.3").is_dir());
}

#[test]
fn pin_exact_local_selection_works_offline_and_can_be_unset() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    install_shell_fixture_without_default(&home);

    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["pin", "1.2.3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pinned Argo CD v1.2.3"))
        .stderr(predicate::str::contains(
            "already installed; using the healthy local install",
        ));

    let pin = project.join(".argocd-version");
    assert_eq!(std::fs::read_to_string(&pin).unwrap(), "v1.2.3\n");
    assert!(!home.join("state/default").exists());

    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .args(["pin", "--unset"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed the project pin"));
    assert!(!pin.exists());
    assert!(home.join("versions/v1.2.3").is_dir());
}

#[test]
fn exec_forwards_arguments_and_child_exit_code() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    install_shell_fixture_without_default(&home);

    let mut command = avm();
    command
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases");
    if cfg!(windows) {
        command.args([
            "exec",
            "1.2.3",
            "--",
            "/D",
            "/C",
            "echo forwarded-token --hyphenated & exit /B 23",
        ]);
    } else {
        command.args([
            "exec",
            "1.2.3",
            "--",
            "-c",
            "printf '%s\\n' \"$1\"; printf '%s\\n' \"$2\"; exit 23",
            "avm-exec-test",
            "forwarded-token",
            "--hyphenated",
        ]);
    }

    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(23));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("forwarded-token"), "{stdout:?}");
    assert!(stdout.contains("--hyphenated"), "{stdout:?}");
    assert!(!home.join("state/default").exists());
}

#[test]
fn managed_dispatcher_resolves_locally_forwards_arguments_and_writes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    install_shell_fixture_without_default(&home);

    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["pin", "v1.2.3"])
        .assert()
        .success();

    let platform = avm::platform::Platform::current().unwrap();
    let dispatcher = home.join("bin").join(platform.binary_name());
    std::fs::write(home.join("state/default"), "v9.9.9\n").unwrap();
    let state_before = directory_entry_names(&home.join("state"));
    let locks_before = directory_entry_names(&home.join("locks"));
    let cache_before = directory_entry_names(&home.join("cache"));

    let mut command = assert_cmd::Command::new(&dispatcher);
    command
        .current_dir(&project)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .env_remove("AVM_GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("AVM_ARGOCD_VERSION");
    if cfg!(windows) {
        command.args([
            "/D",
            "/C",
            "echo dispatcher-token --hyphenated & exit /B 23",
        ]);
    } else {
        command.args([
            "-c",
            "printf '%s\\n' \"$1\"; printf '%s\\n' \"$2\"; exit 23",
            "avm-dispatcher-test",
            "dispatcher-token",
            "--hyphenated",
        ]);
    }

    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(23));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("dispatcher-token"), "{stdout:?}");
    assert!(stdout.contains("--hyphenated"), "{stdout:?}");
    assert_eq!(
        std::fs::read_to_string(project.join(".argocd-version")).unwrap(),
        "v1.2.3\n"
    );
    assert_eq!(directory_entry_names(&home.join("state")), state_before);
    assert_eq!(directory_entry_names(&home.join("locks")), locks_before);
    assert_eq!(directory_entry_names(&home.join("cache")), cache_before);
}

#[test]
fn uninstall_waits_for_a_running_dispatcher_before_removing_its_version() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    install_shell_fixture_without_default(&home);

    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["pin", "v1.2.3"])
        .assert()
        .success();

    let platform = avm::platform::Platform::current().unwrap();
    let dispatcher = home.join("bin").join(platform.binary_name());
    let ready = project.join("child-ready");
    let mut running = std::process::Command::new(&dispatcher);
    running
        .current_dir(&project)
        .env_remove("AVM_ARGOCD_VERSION")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if cfg!(windows) {
        running.args([
            "/D",
            "/C",
            "type nul > child-ready & ping -n 4 127.0.0.1 >NUL",
        ]);
    } else {
        running.args(["-c", ": > child-ready; sleep 3"]);
    }
    let mut running = running.spawn().unwrap();
    wait_for_child_path(&ready, &mut running);

    let mut uninstall = avm_process();
    uninstall
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .args(["uninstall", "v1.2.3"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut uninstall = uninstall.spawn().unwrap();

    thread::sleep(std::time::Duration::from_millis(250));
    assert!(
        uninstall.try_wait().unwrap().is_none(),
        "uninstall must wait while the dispatcher holds a shared version lock"
    );
    assert!(home.join("versions/v1.2.3").is_dir());

    assert!(running.wait().unwrap().success());
    let output = uninstall.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.join("versions/v1.2.3").exists());
}

#[cfg(unix)]
#[test]
fn exec_translates_a_child_signal_to_a_conventional_exit_code() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    install_shell_fixture_without_default(&home);

    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["exec", "1.2.3", "--", "-c", "kill -TERM $$"])
        .assert()
        .code(143);
}

#[test]
fn list_json_has_a_versioned_envelope_and_selection_markers() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    install_shell_fixture_without_default(&home);
    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["default", "v1.2.3"])
        .assert()
        .success();

    let output = avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], 1);
    assert_eq!(report["effective_version"], "v1.2.3");
    let versions = report["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["version"], "v1.2.3");
    assert_eq!(versions[0]["is_default"], true);
    assert_eq!(versions[0]["healthy"], true);
}

#[test]
fn available_and_info_emit_versioned_json() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let platform = avm::platform::Platform::current().unwrap();
    let binary = b"catalog-fixture".to_vec();
    let digest = hex_sha256(&binary);
    let (api_url, available_server) =
        release_list_server(&platform.asset_name(), binary, &digest, 1);

    let output = avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["available", "1.2", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    available_server.join().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], 1);
    assert_eq!(report["query"], "1.2");
    assert_eq!(report["releases"].as_array().unwrap().len(), 2);
    assert_eq!(report["releases"][0]["tag_name"], "v1.2.4");

    let expected = "d".repeat(64);
    let (api_url, info_server) = release_metadata_server(&platform.asset_name(), &expected);
    let output = avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["info", "v1.2.3", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    info_server.join().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], 1);
    assert_eq!(report["selector"], "v1.2.3");
    assert_eq!(report["release"]["tag_name"], "v1.2.3");
}

#[test]
fn custom_api_tokens_require_explicit_opt_in() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");

    let (api_url, ambient_server) = single_release_server("v1.2.3");
    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .env("GITHUB_TOKEN", "ambient-sentinel")
        .args(["info", "v1.2.3"])
        .assert()
        .success();
    let ambient_request = ambient_server.join().unwrap().to_ascii_lowercase();
    assert!(
        !ambient_request.contains("authorization:"),
        "custom endpoints must not receive ambient GitHub credentials: {ambient_request}"
    );

    let (api_url, explicit_server) = single_release_server("v1.2.3");
    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .env("AVM_GITHUB_TOKEN", "explicit-sentinel")
        .args(["info", "v1.2.3"])
        .assert()
        .success();
    let explicit_request = explicit_server.join().unwrap().to_ascii_lowercase();
    assert!(
        explicit_request.contains("authorization: bearer explicit-sentinel"),
        "an explicit custom-endpoint token should be sent: {explicit_request}"
    );
}

#[test]
fn exact_selector_rejects_a_mismatched_release_response() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let (api_url, server) = single_release_server("v9.9.9");

    avm()
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["info", "v1.2.3"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains(
            "lookup for v1.2.3 returned v9.9.9",
        ));

    server.join().unwrap();
    assert!(!home.join("versions/v1.2.3").exists());
    assert!(!home.join("versions/v9.9.9").exists());
}

#[test]
fn status_reports_the_project_source_with_an_equivalent_canonical_path() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    install_shell_fixture_without_default(&home);

    let project = temp.path().join("project");
    let nested = project.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let pin = project.join(".argocd-version");
    std::fs::write(&pin, "v1.2.3\n").unwrap();

    let output = avm()
        .current_dir(&nested)
        .arg("--avm-home")
        .arg(&home)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], 1);
    assert_eq!(report["effective_version"], "v1.2.3");
    assert_eq!(report["effective_source"], "project");
    assert!(report["default_version"].is_null());
    assert_eq!(report["installed"], true);
    assert_eq!(report["healthy"], true);
    assert_eq!(report["verified"], true);

    let reported_path = PathBuf::from(report["source_path"].as_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(reported_path).unwrap(),
        std::fs::canonicalize(pin).unwrap()
    );

    let platform = avm::platform::Platform::current().unwrap();
    let binary = home.join("versions/v1.2.3").join(platform.binary_name());
    std::fs::write(&binary, b"tampered").unwrap();
    make_executable_for_test(&binary);
    let output = avm()
        .current_dir(&nested)
        .arg("--avm-home")
        .arg(&home)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["installed"], true);
    assert_eq!(report["healthy"], false);
    assert_eq!(report["verified"], false);
}

#[test]
fn doctor_emits_parseable_json_and_exits_five_when_unhealthy() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");

    let output = avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], 1);
    assert!(report["effective_version"].is_null());
    assert_eq!(report["dispatcher_healthy"], false);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("one or more AVM diagnostic checks failed")
    );
}

#[test]
fn doctor_audits_project_and_default_independently() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    install_shell_fixture_without_default(&home);

    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["pin", "v1.2.3"])
        .assert()
        .success();

    let output = avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("PATH", path_with_avm_bin(&home))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["effective_version"], "v1.2.3");
    assert_eq!(report["effective_source"], "project");
    assert_eq!(report["effective_healthy"], true);
    assert_eq!(report["effective_verified"], true);
    assert!(report["default_version"].is_null());
    assert_eq!(report["dispatcher_healthy"], true);
    assert_eq!(report["bin_on_path"], true);

    std::fs::write(home.join("state/default"), "v9.9.9\n").unwrap();
    let output = avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("PATH", path_with_avm_bin(&home))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["effective_version"], "v1.2.3");
    assert_eq!(report["effective_healthy"], true);
    assert_eq!(report["default_version"], "v9.9.9");
    assert_eq!(report["default_healthy"], false);

    std::fs::remove_file(home.join("state/default")).unwrap();
    std::fs::write(project.join(".argocd-version"), "not-a-version\n").unwrap();
    let output = avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("PATH", path_with_avm_bin(&home))
        .env("AVM_ARGOCD_VERSION", "v1.2.3")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["effective_source"], "environment");
    assert_eq!(report["effective_healthy"], true);
    assert!(report["project_error"].is_string());
}

#[test]
fn doctor_audits_the_digest_of_every_installed_version() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    install_shell_fixture_without_default(&home);
    avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["default", "v1.2.3"])
        .assert()
        .success();

    let platform = avm::platform::Platform::current().unwrap();
    let source = home.join("versions/v1.2.3");
    let tampered = home.join("versions/v1.2.4");
    std::fs::create_dir(&tampered).unwrap();
    let binary = tampered.join(platform.binary_name());
    std::fs::copy(source.join(platform.binary_name()), &binary).unwrap();
    make_executable_for_test(&binary);
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source.join("install.json")).unwrap()).unwrap();
    metadata["version"] = serde_json::Value::String("v1.2.4".to_owned());
    std::fs::write(
        tampered.join("install.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    std::fs::write(home.join("locks/v1.2.4.lock"), []).unwrap();
    std::fs::write(&binary, b"tampered").unwrap();
    make_executable_for_test(&binary);

    let unlocked = home.join("versions/v1.2.5");
    std::fs::create_dir(&unlocked).unwrap();
    let unlocked_binary = unlocked.join(platform.binary_name());
    std::fs::copy(source.join(platform.binary_name()), &unlocked_binary).unwrap();
    make_executable_for_test(&unlocked_binary);
    metadata["version"] = serde_json::Value::String("v1.2.5".to_owned());
    std::fs::write(
        unlocked.join("install.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();

    let output = avm()
        .current_dir(temp.path())
        .arg("--avm-home")
        .arg(&home)
        .env("PATH", path_with_avm_bin(&home))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["effective_version"], "v1.2.3");
    assert_eq!(report["effective_healthy"], true);
    assert_eq!(report["installed_versions"], 1);
    assert_eq!(report["corrupt_versions"], 2);
}

#[test]
fn uninstall_refuses_selected_versions_then_succeeds_after_unset() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    install_shell_fixture_without_default(&home);

    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["default", "v1.2.3"])
        .assert()
        .success();
    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .args(["uninstall", "v1.2.3"])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("is the default"));

    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .args(["default", "--unset"])
        .assert()
        .success();
    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .args(["pin", "v1.2.3"])
        .assert()
        .success();
    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .args(["uninstall", "v1.2.3"])
        .assert()
        .failure()
        .code(5)
        .stderr(predicate::str::contains("is pinned by"));

    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .args(["pin", "--unset"])
        .assert()
        .success();
    avm()
        .current_dir(&project)
        .arg("--avm-home")
        .arg(&home)
        .args(["uninstall", "v1.2.3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Uninstalled Argo CD v1.2.3."));
    assert!(!home.join("versions/v1.2.3").exists());
}

#[test]
fn dynamic_completion_uses_the_command_surface_and_local_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".avm");
    install_shell_fixture_without_default(&home);

    let top_level = completion_output(
        [OsString::from("--"), OsString::from("avm"), OsString::new()],
        1,
    );
    let commands = completion_values(&top_level);
    for expected in [
        "setup",
        "install",
        "default",
        "pin",
        "exec",
        "status",
        "list",
        "available",
        "info",
        "uninstall",
        "completion",
        "doctor",
    ] {
        assert!(
            commands.iter().any(|value| value == expected),
            "missing {expected:?} in {commands:?}"
        );
    }

    let install = completion_output(
        [
            OsString::from("--"),
            OsString::from("avm"),
            OsString::from("--avm-home"),
            home.as_os_str().to_owned(),
            OsString::from("install"),
            OsString::new(),
        ],
        4,
    );
    assert!(
        completion_values(&install)
            .iter()
            .any(|value| value == "stable"),
        "install completion should offer the stable selector: {install:?}"
    );

    let uninstall = completion_output(
        [
            OsString::from("--"),
            OsString::from("avm"),
            OsString::from("--avm-home"),
            home.as_os_str().to_owned(),
            OsString::from("uninstall"),
            OsString::from("v"),
        ],
        4,
    );
    assert!(
        completion_values(&uninstall)
            .iter()
            .any(|value| value == "v1.2.3"),
        "uninstall completion should use installed versions: {uninstall:?}"
    );
}

fn completion_output<const N: usize>(arguments: [OsString; N], index: usize) -> String {
    let output = avm()
        .args(arguments)
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", index.to_string())
        .env("_CLAP_COMPLETE_COMP_TYPE", "9")
        .env("_CLAP_COMPLETE_SPACE", "true")
        .env("_CLAP_IFS", "\u{b}")
        .env("AVM_GITHUB_API_URL", "http://127.0.0.1:1/releases")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "completion failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn completion_values(output: &str) -> Vec<String> {
    output
        .split(['\n', '\u{b}'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn install_shell_fixture_without_default(home: &Path) {
    let binary = fixture_binary();
    let digest = hex_sha256(&binary);
    let platform = avm::platform::Platform::current().unwrap();
    let (api_url, server) = release_server(&platform.asset_name(), binary, &digest);

    avm()
        .arg("--avm-home")
        .arg(home)
        .env("AVM_GITHUB_API_URL", &api_url)
        .args(["install", "1.2.3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Argo CD v1.2.3 is installed."));
    server.join().unwrap();
    assert!(!home.join("state/default").exists());
}

fn fixture_binary() -> Vec<u8> {
    std::fs::read(fixture_executable()).unwrap()
}

fn fixture_executable() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"))
    } else {
        PathBuf::from("/bin/sh")
    }
}

fn make_executable_for_test(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn path_with_avm_bin(home: &Path) -> OsString {
    let mut entries = vec![home.join("bin")];
    if let Some(path) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(entries).unwrap()
}

fn directory_entry_names(path: &Path) -> Vec<OsString> {
    let mut names = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn wait_for_child_path(path: &Path, child: &mut std::process::Child) {
    let started = std::time::Instant::now();
    while !path.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "child exited with {status} before creating {}",
                path.display()
            );
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn release_server(
    asset_name: &str,
    binary: Vec<u8>,
    digest: &str,
) -> (String, thread::JoinHandle<()>) {
    let advertised_size = binary.len() as u64;
    release_server_with_size(asset_name, binary, digest, advertised_size)
}

fn release_server_with_size(
    asset_name: &str,
    binary: Vec<u8>,
    digest: &str,
    advertised_size: u64,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let api_url = format!("{base}/releases");
    let asset_url = format!("{base}/asset?token=fixture-secret");
    let asset_name = asset_name.to_owned();
    let digest = digest.to_owned();

    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap();

            if target == "/releases/tags/v1.2.3" {
                let body = serde_json::json!({
                    "tag_name": "v1.2.3",
                    "draft": false,
                    "prerelease": false,
                    "assets": [{
                        "name": asset_name,
                        "browser_download_url": asset_url,
                        "digest": format!("sha256:{digest}"),
                        "size": advertised_size
                    }]
                })
                .to_string();
                write_response(&mut stream, "200 OK", "application/json", body.as_bytes());
            } else if target == "/asset?token=fixture-secret" {
                write_response(&mut stream, "200 OK", "application/octet-stream", &binary);
            } else {
                write_response(&mut stream, "404 Not Found", "text/plain", b"missing");
            }
        }
    });

    (api_url, server)
}

fn single_release_server(response_tag: &str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let api_url = format!("http://{address}/releases");
    let response_tag = response_tag.to_owned();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]).into_owned();
        let body = serde_json::json!({
            "tag_name": response_tag,
            "draft": false,
            "prerelease": false,
            "assets": []
        })
        .to_string();
        write_response(&mut stream, "200 OK", "application/json", body.as_bytes());
        request
    });

    (api_url, server)
}

fn release_metadata_server(asset_name: &str, digest: &str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let api_url = format!("{base}/releases");
    let asset_name = asset_name.to_owned();
    let digest = digest.to_owned();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("GET /releases/tags/v1.2.3 "));
        let body = serde_json::json!({
            "tag_name": "v1.2.3",
            "draft": false,
            "prerelease": false,
            "assets": [{
                "name": asset_name,
                "browser_download_url": format!("{base}/asset"),
                "digest": format!("sha256:{digest}"),
                "size": 24
            }]
        })
        .to_string();
        write_response(&mut stream, "200 OK", "application/json", body.as_bytes());
    });

    (api_url, server)
}

fn release_list_server(
    asset_name: &str,
    binary: Vec<u8>,
    digest: &str,
    expected_requests: usize,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let api_url = format!("{base}/releases");
    let asset_url = format!("{base}/asset");
    let asset_name = asset_name.to_owned();
    let digest = digest.to_owned();

    let server = thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap();

            if target == "/releases?per_page=100" {
                let release = |tag: &str, prerelease: bool| {
                    serde_json::json!({
                        "tag_name": tag,
                        "draft": false,
                        "prerelease": prerelease,
                        "assets": [{
                            "name": asset_name,
                            "browser_download_url": asset_url,
                            "digest": format!("sha256:{digest}"),
                            "size": binary.len()
                        }]
                    })
                };
                let body = serde_json::json!([
                    release("v1.2.3", false),
                    // The API flag is intentionally inconsistent: stable resolution must also
                    // derive prerelease status from the semantic version.
                    release("v1.2.5-rc1", false),
                    // The inverse inconsistency is also excluded from stable resolution.
                    release("v1.2.6", true),
                    release("v1.3.0", false),
                    release("v1.2.4", false),
                    release("v11.2.0", false)
                ])
                .to_string();
                write_response(&mut stream, "200 OK", "application/json", body.as_bytes());
            } else if target == "/asset" {
                write_response(&mut stream, "200 OK", "application/octet-stream", &binary);
            } else {
                write_response(&mut stream, "404 Not Found", "text/plain", b"missing");
            }
        }
    });

    (api_url, server)
}

fn write_response(stream: &mut std::net::TcpStream, status: &str, kind: &str, body: &[u8]) {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
