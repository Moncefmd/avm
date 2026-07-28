# Architecture

AVM is one Rust package with a library and a thin executable. The library owns version parsing,
release discovery, filesystem transactions, selection, and command orchestration. `main.rs`
chooses CLI mode or `argocd` dispatcher mode and maps typed errors to exit codes.

## Product model

AVM separates three actions that should never be surprising:

1. **Install** makes a release available locally, with verification required by default.
2. **Select** writes either a personal default or an exact project pin.
3. **Execute** resolves a selected installed release and runs it.

`avm install` performs only the first action. `avm default` and `avm pin` may install before
persisting their selection. `avm exec` may install its required explicit selector but never mutates
a persisted selection. The ordinary `argocd` dispatcher is entirely local and read-only.

Because `default <selector>` and `pin <selector>` can install, they expose the same `--force` and
`--allow-unverified` controls as `install`; neither flag is valid with `--unset`. `exec` exposes
`--allow-unverified` for an installation it initiates, but does not replace an existing
installation.

## On-disk contract

```text
<project>/
└── .argocd-version               # canonical exact project pin, safe to commit

~/.avm/
├── bin/
│   └── argocd[.exe]              # copy of AVM acting as a dispatcher
├── versions/
│   └── v3.4.5/
│       ├── argocd[.exe]          # upstream release binary
│       └── install.json          # asset, digest, verification, and source
├── state/
│   ├── default                   # canonical exact tag plus newline
│   └── dispatcher.json           # proves AVM owns the dispatcher
├── cache/
│   └── releases.json             # paginated GitHub release snapshot
└── locks/
    ├── state.lock
    └── v3.4.5.lock
```

Project pins live outside AVM home and contain one canonical tag plus a newline, for example
`v3.4.5`. Every complete installation contains both its upstream executable and `install.json`.
The default root is `~/.avm`; `AVM_HOME` or the global `--avm-home <DIR>` option selects another
root for a management command. The dispatcher derives its root from its own `<root>/bin` location,
so persistent custom-home setups must configure that root's bin directory in PATH.

## Module responsibilities

| Module | Responsibility |
| --- | --- |
| `cli` | Clap command and flag definitions. |
| `app` | User-visible command orchestration and output. |
| `version` | Strict selector parsing, normalization, and semantic ordering. |
| `resolver` | Offline precedence, ancestor pin discovery, and atomic pin writes. |
| `platform` | Rust host to exact Argo CD release-asset mapping. |
| `github` | GitHub REST, pagination, bounded retries, downloads, and checksum parsing. |
| `store` | Paths, locks, staging, atomic state, metadata, cache, and deletion safety. |
| `shim` | Dispatch from the consistent `argocd` command to an upstream executable. |
| `error` | Typed failures and stable exit categories. |

Dependencies flow inward through values such as `Release`, `InstallMetadata`, and `Platform`
instead of global mutable state. Tests can override the API endpoint and AVM home.

## Selector model

A management selector is one of:

- `stable`;
- a major line such as `3`;
- a minor line such as `3.4`; or
- an exact release such as `3.4.5` or `v3.4.5`.

Release discovery resolves every selector to one canonical exact tag. Only that exact tag may be
written to a project pin or `state/default`, used as a version directory, or passed to the
dispatcher.

## Install transaction

1. Parse and validate the selector before building a path or endpoint.
2. Resolve it through release metadata to one canonical exact tag.
3. Select one allowlisted asset for the current platform.
4. Obtain an expected SHA-256 from GitHub's asset digest or the release checksum manifest, or
   require explicit `--allow-unverified` when neither source provides one.
5. Acquire the exclusive per-version lock.
6. If a complete installation exists, hash its binary and require its metadata to agree.
7. Stream the asset into a random staging directory under `versions/`, hashing as bytes arrive.
8. Reject a mismatch before making the file executable or visible.
9. Write and sync `install.json`.
10. Rename the complete staging directory to its canonical version directory.
11. If `--force` is replacing an installation, keep the installed directory as a same-filesystem
    backup until the new rename succeeds.

`install` stops after this transaction. `default` atomically writes `state/default`; `pin`
atomically writes the current directory's `.argocd-version`; `exec` changes neither.

Directories whose names start with `.` are never listed as installed. Interrupted staging and
removal directories remain hidden and inspectable.

## Selection and execution

The dispatcher resolves an exact installed version without network access or filesystem mutation.
The precedence is:

1. `AVM_ARGOCD_VERSION`;
2. the nearest `.argocd-version`, found by walking from the current directory toward the
   filesystem root; and
3. the exact tag in `state/default`.

An invalid higher-precedence source fails closed instead of falling through. Ambient and persisted
sources may name only exact releases.

`avm exec <selector> -- <args>` is a separate explicit path. It resolves and prepares its required
selector, then runs it without consulting or changing ambient selection.

The dispatcher is a small copy of the AVM executable at `bin/argocd[.exe]`. When invoked with that
filename, it:

1. derives AVM home from its own `<root>/bin/argocd[.exe]` location;
2. resolves environment, project, and default selection for the working directory;
3. acquires a shared lock for the selected version;
4. requires a complete installation with valid metadata and a regular, non-empty executable;
5. spawns it with inherited arguments and standard streams; and
6. waits and propagates the child's exit code while retaining the shared lock.

Dispatcher execution never creates directories, downloads a release, repairs state, or changes a
selection. A missing selected version produces an actionable error directing the user to
`avm install`.

## Lock order

The fixed order is:

1. per-version lock;
2. state lock.

Commands never acquire them in the opposite order. Locks use bounded `try_lock` polling and produce
an actionable error after ten seconds instead of blocking indefinitely. A running Argo CD process
holds a shared version lock, so `uninstall` cannot remove its executable.

`uninstall` normalizes exact versions with or without a `v` prefix. It waits up to ten seconds to
acquire the exclusive version lock, then either removes the version after running executions finish
or returns a lock error without modifying it. Project-pin writes use atomic file replacement rather
than the default-state lock.

## Release metadata and completion

`available` follows GitHub's `Link: rel="next"` pagination until no next page remains. Drafts are
discarded, prereleases are opt-in, and valid tags are sorted by SemVer rather than publication time
or string order. A valid selector query filters semantically by stability, major line, minor line,
or exact tag; other text is matched case-insensitively against release tags. `info` resolves one
selector and returns its release and assets.

The paginated release list used by `available` and stable, major-, or minor-line selectors is cached
for six hours. `--refresh` explicitly requests a fresh list. A stale list may satisfy a request when
GitHub is temporarily unavailable, with a warning to the user. Exact-release selector lookups use a
targeted live API request instead of this list cache.

Generated shell registration calls AVM's local completion engine. Installed versions and cached
release tags provide candidates. Completion never contacts GitHub. `completion <shell> --install`
writes registration for Bash, Zsh, and Fish to their per-user completion directories; PowerShell
prints profile-specific generation and dot-sourcing instructions.

## Setup

`setup` applies its changes idempotently by default. `--dry-run` reports the exact changes without
writing. `--shell <shell>` overrides shell detection.

- Bash, Zsh, and Fish profiles receive one marked AVM block through an atomic same-directory
  replacement. Existing permissions and newline style are preserved.
- Symlinked, non-regular, oversized, malformed-marker, BOM, and non-UTF-8 profiles are refused.
- Windows PowerShell updates the per-user PATH without editing a PowerShell profile.
- PowerShell on another platform reports a manual instruction when it cannot identify a safe
  persistent target.

Setup acquires the state lock and creates or repairs only an AVM-owned dispatcher.

## Diagnostics and machine output

`status` reports effective resolution and its source, then checks that version's execution lock and
recorded binary digest without network access. `list` is a fast structural inventory of local
installations. `available` and `info` report cached or fetched release data. `doctor` reports the
managed directories, dispatcher, PATH configuration, default, and project pin, then validates the
execution lock, metadata, and binary digest of every installed version. The effective selection,
nearest project pin, and personal default are audited independently even when precedence causes one
to shadow another.

Human-readable output is the default. Commands that support `--json` emit a versioned envelope with
`schema: 1`. When `doctor` completes its diagnostic pass, an unhealthy report uses local-state exit
code `5` and JSON is emitted before that nonzero exit. A fatal layout or filesystem error may abort
the pass before any report can be serialized.

## Safety invariants

- A version is a canonical `v`-prefixed SemVer tag before it becomes a path component.
- A project pin is a bounded regular UTF-8 file containing one exact version.
- Nearest-pin discovery is read-only, and `pin` refuses symlink targets before atomic replacement.
- Every version directory is a direct child of `versions/`.
- A visible installation contains a regular executable and valid `install.json`.
- AVM refuses symlinks at managed root and version directories before mutation.
- Recursive deletion targets only a validated canonical version directory after an atomic rename.
- AVM refuses to overwrite an unmarked regular `bin/argocd`.
- A marked dispatcher is compared byte-for-byte with the running AVM executable before it is
  trusted.
- API credentials are absent from the release-download client.
- Authenticated API requests cannot follow redirects or pagination links to another origin.
- Non-loopback network URLs use HTTPS, including redirects.
- Downloads have connection, total-time, redirect, retry, and one-GiB size limits.
- Ordinary `argocd` execution performs no network access and no filesystem writes.
- A release without verification metadata requires explicit `--allow-unverified`.
- An installed exact tag pins its recorded digest until that version is uninstalled.
- Interrupted installs remain hidden and are never treated as complete.

## Exit categories

| Code | Category |
| --- | --- |
| `0` | Success |
| `2` | Usage or argument error |
| `3` | Network, GitHub API, rate limit, or missing release |
| `4` | Integrity, checksum, or download-size failure |
| `5` | Local state, lock, filesystem, or unsupported-platform failure |

## Deliberate non-goals for v0.1

- The dispatcher does not install missing versions.
- Project configuration is one exact version, not an executable or extensible environment file.
- AVM does not execute a downloaded binary as an installation test.
- Checksum verification is implemented; independent SLSA provenance verification is future work.
