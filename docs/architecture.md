# Architecture

AVM is one Rust package with a library and a thin executable. The library owns version parsing,
release discovery, filesystem transactions, selection, and command orchestration. `main.rs`
chooses CLI mode or protocol-stable `argocd` launcher mode and maps typed errors to exit codes.

## Product model

AVM separates five actions that should never be surprising:

1. **Install** makes a release available locally, with verification required by default.
2. **Initialize** creates explicitly requested shell integration.
3. **Select** writes either a personal default or an exact project pin.
4. **Execute** resolves a selected installed release and runs it.
5. **Uninitialize** removes owned shell integration while preserving versions and selection.

`avm install` performs only the first action. `avm default` and `avm pin` may install before
persisting their selection, but never initialize shell integration. `avm exec` may install its
required explicit selector but never mutates a persisted selection. The ordinary `argocd`
dispatcher is entirely local and read-only.

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
│   ├── avm[.exe]                 # optional release-installer destination
│   ├── avm[.exe].sha256          # release-installer ownership and digest marker
│   └── argocd[.exe]              # protocol-v1 launcher
├── versions/
│   └── v3.4.5/
│       ├── argocd[.exe]          # upstream release binary
│       └── install.json          # asset, digest, verification, and source
├── state/
│   ├── default                   # canonical exact tag plus newline
│   ├── dispatcher.json           # launcher protocol, digest, and ownership
│   └── integration.json          # Windows user-PATH ownership receipt
├── cache/
│   └── releases.json             # paginated GitHub release snapshot
├── completions/
│   └── avm.ps1                   # managed PowerShell registration script
└── locks/
    ├── state.lock
    └── v3.4.5.lock
```

Project pins live outside AVM home and contain one canonical tag plus a newline, for example
`v3.4.5`. Every complete installation contains both its upstream executable and `install.json`.
The default root is `~/.avm`; `AVM_HOME` or the global `--avm-home <DIR>` option selects another
root for a management command. The launcher derives its root from its own `<root>/bin` location and
passes that absolute root to the current AVM executable, so persistent custom-home setups must
configure that root's bin directory in PATH.

The release installers place AVM itself in that same bin directory, so initialization needs only
one PATH entry for both `avm` and `argocd`. The adjacent SHA-256 marker belongs to the bootstrap
installer, not the Argo CD version store; it proves that a future installer may replace the AVM
executable without claiming ownership of an unrelated file.

## Module responsibilities

| Module | Responsibility |
| --- | --- |
| `cli` | Clap command and flag definitions. |
| `app` | User-visible command orchestration and output. |
| `atomic_file` | Guarded snapshots, concurrent-change detection, and atomic file replacement. |
| `catalog` | Release selection, stability policy, cache freshness, and stale-cache fallback. |
| `version` | Strict selector parsing, normalization, and semantic ordering. |
| `release` | Transport-independent release, asset, and binary-size domain values. |
| `resolver` | Offline precedence, ancestor pin discovery, and atomic pin writes. |
| `platform` | Rust host to exact Argo CD release-asset mapping. |
| `github` | GitHub REST, pagination, bounded retries, downloads, and checksum parsing. |
| `installer` | Verified download, staging, validation, and install transaction orchestration. |
| `onboarding` | First-run orchestration across dispatcher, PATH, profiles, and completion. |
| `launcher` | Stable `argocd`-to-current-AVM protocol delegation without a shell. |
| `shell` | Shell detection, PATH integration plans, and Windows user PATH updates. |
| `completion` | Dynamic registration generation and guarded per-shell installation. |
| `profile` | Shared marked-block planning and atomic shell-profile edits. |
| `store` | Paths, locks, staging, atomic state, metadata, cache, and deletion safety. |
| `shim` | Offline selection and dispatch from current AVM to an upstream executable. |
| `error` | Typed failures and stable exit categories. |

The `release`, `version`, `platform`, and `error` modules are leaf domain boundaries. GitHub
transport and persistent storage both depend on release values; storage never depends on the
GitHub client. `app` coordinates catalog, shell, resolver, store, and transport services without
global mutable state. Tests can override the API endpoint and AVM home.

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
8. Require a non-empty body whose streamed size matches published asset metadata when available.
9. Reject a checksum mismatch before making the file executable or visible.
10. Revalidate the staged executable at the storage boundary, then write and sync `install.json`.
11. Rename the complete staging directory to its canonical version directory.
12. If `--force` is replacing an installation, keep the installed directory as a same-filesystem
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

The dispatcher path contains a protocol-v1 launcher copied from AVM. The launcher behavior is
deliberately frozen and version-independent even though the v1 implementation shares AVM's binary.
When invoked as `argocd`, it:

1. derives AVM home from its own `<root>/bin/argocd[.exe]` location;
2. resolves `avm[.exe]` in normal absolute PATH order, falls back to the adjacent
   standalone-installer copy when it is not represented there, and rejects relative and empty PATH
   entries;
3. invokes the current AVM without a shell as
   `avm --avm-home <root> __dispatch-v1 -- <original arguments>`;
4. lets current AVM resolve environment, project, and default selection for the working directory;
5. acquires a shared lock for the selected version;
6. requires a complete installation with valid metadata and a regular, non-empty executable; and
7. runs it with the original OS arguments and propagates the child's exit code while retaining the
   shared lock.

`__dispatch-v1` is a hidden compatibility protocol, not a user command. It remains available to old
launchers when AVM is upgraded. A future purpose-built smaller launcher can use the same protocol
without changing the package-manager lifecycle.

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
writes managed registration for Bash, Zsh, and Fish to their per-user completion directories and
stores PowerShell registration under `AVM_HOME/completions`. It adds an idempotent activation block
where the shell requires one. On Windows, both Windows PowerShell and PowerShell 7 current-user
profiles are planned together. The PowerShell adapter parses command text into inert argument
values and invokes AVM with an argument array; it never evaluates the typed command line.
`--dry-run` preflights and reports the installation without writing.

## Initialization

`init` creates or repairs the managed `argocd` dispatcher, configures its bin directory in PATH,
and installs tab completion by default. It never installs an Argo CD release or changes the project
pin or personal default. `--dry-run` previews the resulting configuration without writing,
`--shell <shell>`
overrides shell detection, and `--no-completion` limits the operation to dispatcher and PATH setup.

- Bash receives one marked AVM block in `.bashrc` and one in its first active login profile;
  existing `.bash_profile`, `.bash_login`, or `.profile` precedence is preserved. The PATH command
  is idempotent when a login profile already sources `.bashrc`. Completion uses a separate marked
  block in `.bashrc`.
- Zsh receives separate PATH and completion blocks. Fish uses `config.fish` for PATH and its
  conventional per-user completion directory. All completion destinations and profile edits are
  preflighted before any file is changed; writes use atomic same-directory replacement. Existing
  permissions and newline style are preserved.
- Symlinked, non-regular, oversized, malformed-marker, and invalid-encoding profiles are refused.
  POSIX shell profiles remain strict UTF-8 without a BOM. PowerShell profiles preserve existing
  BOM-less UTF-8, UTF-8 BOM, UTF-16LE BOM, or UTF-16BE BOM encoding and newline style.
- Windows PowerShell updates the per-user PATH and receives guarded completion blocks in the
  current-user profiles for both Windows PowerShell and PowerShell 7. Other platforms use the
  conventional PowerShell profile under the user's configuration directory.
- A single legacy `avm setup` PATH block is migrated to the `avm init` marker. Conflicting or
  duplicate legacy and current markers are refused.

Initialization acquires the state lock and creates or repairs only an AVM-owned dispatcher. The
shared profile planner combines PATH and completion requests for the same file before applying
either block, so one operation cannot overwrite the other's planned contents. Before replacing any
profile or completion file, the atomic-file layer confirms that its contents still match the
preflight snapshot and refuses a concurrent edit. Existing files on Windows use the native-backed
replacement path that preserves the destination DACL and other security metadata.

`uninit` is the inverse shell-integration operation. It is global rather than shell-specific
because all shells share one dispatcher. It plans and preflights every recognized Bash, Zsh, Fish,
Windows PowerShell, and PowerShell 7 profile before mutation, then removes only exact marked AVM
blocks. Managed completion files require the AVM ownership header; unmarked files are reported and
preserved. The dispatcher requires a bounded regular `dispatcher.json` whose protocol and recorded
SHA-256 match the launcher before either file is removed. A released v1.0 launcher is recognized
only by its exact published platform digest. If launcher ownership is uncertain, cleanup preserves
that artifact with a warning and continues removing independently owned integration.

Initialization, completion installation, and uninitialization serialize their complete integration
lifecycle under the state lock. On Windows, `init` journals PATH-update intent before mutation,
then records whether it actually added `AVM_HOME/bin` to the user PATH.
The raw registry value and its string or expandable-string type are preserved, and a Windows
environment-change notification is broadcast after mutation. `uninit`
removes one normalized matching entry only when that receipt proves ownership. A legacy entry with
no receipt is preserved unless `--remove-path` is explicit; that opt-in mode may remove one raw
environment-variable entry that expands to the same path. Cleanup never removes installed Argo CD
versions, the personal default, project pins, release cache, or the AVM executable itself. A dry run
creates nothing and reports the same ownership decisions without writing.

## AVM bootstrap installation

`install.sh` and `install.ps1` are release assets generated from version-placeholder templates.
The publishing workflow embeds the tag into each script, so an installer obtained through the
`releases/latest/download` URL still fetches its binary and checksum from one exact release. The
scripts support only the five AVM targets built by the release matrix and reject all other host
mappings.

Downloads are anonymous, HTTPS-only, redirect-restricted, time- and size-bounded, and staged in a
private directory on the destination filesystem. The sidecar must contain exactly one SHA-256
record naming the expected asset. A staged binary is made executable where necessary and must
report the embedded release version before it can replace the current executable. Existing
symlinks, reparse points, non-regular files, or unowned executables are not replaced implicitly.
After installation, the script invokes the installed executable by absolute path with `init`; an
initialization failure leaves the verified AVM binary available for an explicit retry but never
selects or installs an Argo CD release.

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
- Dispatcher metadata is bounded, regular, non-symlinked, protocol-versioned, and bound to the
  launcher's SHA-256. Package upgrades do not require launcher bytes to match current AVM bytes.
- The launcher ignores relative PATH entries and delegates with OS argument arrays rather than a
  shell command string.
- `uninit` removes only marked profile blocks, header-owned completion files, and a digest-owned
  launcher; unknown, modified, non-regular, and unprovable artifacts are preserved and reported
  without blocking independent cleanup.
- API credentials are absent from the release-download client.
- Ambient `GH_TOKEN` and `GITHUB_TOKEN` values authenticate only the built-in GitHub endpoint;
  custom API endpoints require the explicit `AVM_GITHUB_TOKEN`.
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

## Deliberate non-goals for the current design

- The dispatcher does not install missing versions.
- Project configuration is one exact version, not an executable or extensible environment file.
- AVM does not execute a downloaded binary as an installation test.
- Checksum verification is implemented; independent SLSA provenance verification is future work.
