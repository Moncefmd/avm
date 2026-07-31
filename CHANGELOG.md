# Changelog

All notable changes to AVM are documented here. The project follows
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.1.0] - 2026-07-31

### Added

- Added release-pinned PowerShell and POSIX bootstrap installers that detect the host, download the
  matching AVM binary and checksum, install under `AVM_HOME/bin`, and run `avm init` by default.
- Added `-NoInit`/`--no-init`, `-NoCompletion`/`--no-completion`, exact-version, and explicit force
  controls for automated or customized installation.
- Added `avm uninit` with a read-only preview, ownership-checked dispatcher and completion cleanup,
  marked profile-block removal, and opt-in cleanup for untracked legacy Windows PATH entries.

### Changed

- Replaced the vague `avm setup` command with `avm init`, which explicitly prepares the managed
  `argocd` dispatcher, PATH integration, and tab completion without installing or selecting an
  Argo CD version.
- `avm init` installs completion by default; `--no-completion` keeps dispatcher and PATH setup only.
- `avm completion <shell> --install` now activates PowerShell as well as Bash, Zsh, and Fish, with
  `--dry-run` available to preview installation.
- The managed `argocd` executable is now a protocol-stable launcher. It locates the current `avm`
  executable and delegates offline dispatch through a hidden versioned protocol, so package-manager
  upgrades at the active PATH location do not leave dispatch behavior on an older AVM release.
- Dispatcher creation belongs exclusively to `avm init`; changing a default or project pin no
  longer creates or repairs shell integration implicitly.

### Security

- Bootstrap installers use HTTPS-only bounded downloads, strict checksum records, private
  same-filesystem staging, smoke validation, atomic replacement, and installer-owned digest
  markers. Unsafe destinations and unmanaged executables are refused by default.
- PowerShell completion no longer evaluates the typed command line; it parses inert arguments and
  invokes AVM through an argument array.
- Profile and completion writes detect concurrent changes before atomic replacement, and
  PowerShell profile edits preserve supported UTF-8 and UTF-16 encodings. Windows replacements
  also preserve the destination file's ACL and security metadata.
- Dispatcher ownership metadata is bounded, non-symlinked, protocol-versioned, and bound to the
  launcher's SHA-256 digest. Cleanup refuses modified or unowned launchers and files.
- Shell marker detection is whole-line-only, Windows PowerShell is launched from a trusted
  System32 path, and integration lifecycle changes are serialized. Windows PATH updates preserve
  raw expandable entries and their registry type while journaling ownership before mutation.
  Cleanup can still remove independent owned integration when an uncertain dispatcher or unrelated
  shell artifact must be preserved.

## [1.0.0] - 2026-07-30

### Added

- A Rust command-line application for managing Argo CD CLI releases.
- Stable, major-line, minor-line, and exact version selectors.
- Verified, staged, atomic installations that never change selection implicitly.
- A personal default and commit-friendly exact project pins.
- Environment, nearest-project-pin, and personal-default resolution for the `argocd` dispatcher.
- Explicit one-off execution through `avm exec <selector> -- <args>`.
- An offline, read-only cross-platform dispatcher for Linux, macOS, and Windows.
- GitHub API pagination, semantic sorting, bounded retries, rate-limit errors, and a six-hour
  release cache.
- Release discovery through `available` and detailed release inspection through `info`.
- Strict version and path validation with marked dispatcher ownership.
- Per-version and state locks with bounded acquisition.
- Local and machine-readable status, installed-version inventory, release, and diagnostic output.
- Local-only dynamic shell completion using installed versions and cached releases.
- Idempotent shell setup with explicit shell selection and a read-only `--dry-run` mode.
- SHA-256 verification through release asset digests or Argo CD checksum manifests.
- Exact-tag digest pinning that detects changes to a recorded release.
- Unit, fixture, and end-to-end tests across the installation and selection lifecycle.

### Security

- Version input is canonicalized before it can affect a path or URL.
- Release downloads require SHA-256 verification unless `--allow-unverified` is explicit.
- API credentials are isolated from release-asset downloads.
- Authenticated redirects and pagination are restricted to the configured API origin.
- Managed-directory symlinks and incomplete installations are rejected.
- Project pins are bounded, exact, regular UTF-8 files and are replaced atomically without
  following symlinks.
- The `argocd` dispatcher never downloads, repairs, or writes state.
- Shell setup refuses unsafe profiles, unsupported encodings, and malformed ownership markers.
- Recorded digests are checked before an exact installed version is reused.
- Corrupt canonical version directories are reported and can be safely uninstalled.
