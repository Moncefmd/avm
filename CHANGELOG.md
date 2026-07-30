# Changelog

All notable changes to AVM are documented here. The project follows
[Semantic Versioning](https://semver.org/).

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
