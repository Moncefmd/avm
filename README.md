# AVM - Argo CD Version Manager

AVM is a fast, project-aware, cross-platform version manager for the
[Argo CD CLI](https://argo-cd.readthedocs.io/en/stable/user-guide/commands/argocd/).
It gives Argo CD the same kind of version workflow that nvm provides for Node.js: install several
releases side by side, choose a personal default, pin an exact version in each repository, and
keep using the ordinary `argocd` command.

## Why AVM?

Teams often work across clusters and repositories that expect different Argo CD CLI versions.
Downloading binaries by hand makes those expectations invisible and leaves each developer to
maintain PATH entries independently. AVM turns the selected version into explicit local state:

- `avm install 3.4` installs the newest stable `3.4.x` release without changing a selection.
- `avm default stable` chooses a personal fallback.
- `avm pin 3.4` writes the resolved exact version to `.argocd-version` for the current project.
- `argocd ...` automatically runs the version selected for the current directory.
- `avm exec 3 -- ...` runs one command with a specific version without changing any selection.

## Highlights

- Fast local resolution for every ordinary `argocd` invocation.
- Commit-friendly project pins containing one exact Argo CD release.
- Predictable precedence: environment, nearest project pin, then personal default.
- An offline, read-only `argocd` dispatcher; running Argo CD never triggers a download.
- SHA-256 verification using GitHub's asset digest or Argo CD's checksum manifest.
- Atomic staged installs, so an interrupted download never appears installed.
- Strict SemVer validation before a selector reaches a URL or filesystem path.
- Paginated, semantically sorted release lists with a six-hour local cache.
- Local shell completion with no network access while pressing Tab.
- Per-version and default-state locks, atomic project-pin writes, and guarded execution.
- Native Linux, macOS, and Windows behavior without requiring symlink privileges.

## Install

Download the binary for your system from
[GitHub Releases](https://github.com/Moncefmd/avm/releases), rename it to `avm` (or `avm.exe` on
Windows), and place it in a directory already on PATH.

Raw Linux and macOS downloads need their executable bit restored:

```sh
chmod +x avm
sudo install -m 0755 avm /usr/local/bin/avm
```

You can also build from source with Rust 1.97.1:

```sh
git clone https://github.com/Moncefmd/avm.git
cd avm
cargo build --release --locked
```

The resulting executable is `target/release/avm` (`target\release\avm.exe` on Windows).

## Quick start

```sh
# Add AVM's dispatcher to your shell environment.
avm setup

# Choose a personal fallback. AVM installs the resolved release if necessary.
avm default stable

# Pin the newest stable 3.4.x release in this project.
avm pin 3.4

# Inspect the effective selection, then use Argo CD normally.
avm status
argocd version --client
```

`avm setup` applies the required change idempotently. It creates the managed dispatcher in
`~/.avm/bin` and configures the selected or detected shell. Preview the change without writing it
with:

```sh
avm setup --dry-run
```

Select a shell explicitly when detection is not appropriate:

```sh
avm setup --shell zsh
avm setup --shell fish
avm setup --shell powershell
```

On Bash, Zsh, and Fish, AVM maintains one marked profile block. On Windows, PowerShell setup
updates the per-user PATH.

## Selecting a version

For the normal `argocd` command, AVM resolves an exact installed version in this order:

1. `AVM_ARGOCD_VERSION`;
2. the nearest `.argocd-version`, starting in the current directory and walking toward the
   filesystem root; and
3. the personal default set by `avm default`.

`avm exec` has a required selector that applies only to that invocation. It does not participate
in the normal dispatcher precedence.

An invalid higher-priority value is an error. AVM never silently falls through to another source.
Environment values and persisted files contain canonical exact tags. Management commands accept
these selectors:

| Selector | Meaning |
| --- | --- |
| `stable` | Newest stable Argo CD release. |
| `3` | Newest stable release in major line 3. |
| `3.4` | Newest stable release in minor line 3.4. |
| `3.4.5`, `v3.4.5` | Exact release. |

Commands that persist a choice resolve it first and write a canonical tag such as `v3.4.5`.

## Commands

### Configure the shell

```sh
avm setup
avm setup --shell bash
avm setup --dry-run
```

Setup is safe to run repeatedly. `--dry-run` prints the proposed changes without modifying the
dispatcher, shell profile, or user PATH.

### Install releases

```sh
avm install
avm install stable
avm install 3
avm install 3.4
avm install v3.4.5
```

With no selector, `install` uses the effective environment, project, or default selection when one
exists, and otherwise uses `stable`. Installation never changes the personal default or a project
pin. AVM verifies the download, writes it to a staging directory, and exposes it only after the
binary and its metadata are complete.

Use `--force` to replace an installed release. If a release has no SHA-256 information, AVM fails
closed unless the operation explicitly includes `--allow-unverified`.

The same `--force` and `--allow-unverified` controls are available on `default <selector>` and
`pin <selector>` because those forms may install a release; neither flag can be combined with
`--unset`. `exec` accepts `--allow-unverified` when it must install its selected release, but
deliberately has no `--force` mode.

### Set a personal default

```sh
avm default stable
avm default 3.4
avm default --unset
```

`default <selector>` installs the resolved release if needed and stores its exact tag as the
fallback for directories without an environment selection or project pin. `default --unset`
removes that fallback.

### Pin a project

```sh
avm pin 3.4
avm pin v3.4.5
avm pin --unset
```

`pin <selector>` installs the resolved release if needed and writes its exact tag to
`.argocd-version` in the current directory. Commit this small file with the project so every
contributor selects the same CLI version. `pin --unset` removes the pin in the current directory.

### Execute with an explicit version

```sh
avm exec 3.4 -- version --client
avm exec v3.4.5 -- app sync guestbook --prune
```

Arguments after `--` are forwarded unchanged. AVM installs the selected release if needed, holds a
shared lock while it runs, and returns the Argo CD process's exit status. The personal default and
project pin remain unchanged.

### Inspect local state

```sh
avm status
avm status --json
avm list
avm list --json
```

`status` explains the effective version, its source, its executable path, and its verification and
health state. `list` shows installed versions. Both commands are local and perform no network
access.

### Discover releases

```sh
avm available
avm available 3.4
avm available --prerelease
avm available --refresh
avm available --json

avm info stable
avm info 3
avm info v3.4.5
avm info 3.4 --refresh --json
```

`available` lists releases. A selector query such as `3`, `3.4`, or `v3.4.5` filters semantically;
other query text is matched case-insensitively against release tags. `info` resolves one selector
and shows the release and its assets. The paginated release list used by `available` and stable,
major-, or minor-line selectors is cached for six hours. When GitHub is temporarily unavailable,
AVM can use a stale list and print a warning. `--refresh` requests a fresh list. Exact-release
lookups use a targeted live API request rather than the list cache.

### Remove an installed release

```sh
avm uninstall v3.4.5
```

`uninstall` accepts one exact version, with or without the `v` prefix. AVM refuses removal while the
version is the personal default or is pinned in the current project. If another process is
executing it, AVM waits up to ten seconds for the version lock; it proceeds if execution finishes
or fails without removing anything if the lock remains busy.

### Generate shell completion

```sh
avm completion bash
avm completion zsh
avm completion fish
avm completion powershell
avm completion bash --install
```

Generated completion reads installed versions and cached release metadata locally. It never
contacts GitHub during completion. `--install` writes Bash, Zsh, or Fish completion to its per-user
completion directory. PowerShell completion is profile-specific, so its `--install` form prints
generation and dot-sourcing instructions instead.

### Diagnose the installation

```sh
avm doctor
avm doctor --json
```

`doctor` checks AVM's directories, dispatcher ownership, PATH configuration, installed-version
metadata, and binary digests. It validates the effective selection, nearest project pin, and
personal default independently, so a temporary higher-priority selection cannot hide a broken
fallback. An unhealthy local state returns exit code `5`. When the diagnostic pass completes,
`--json` emits its report before that nonzero exit so automation can parse it. A fatal structural
or filesystem error can stop the pass before a report is available.

## Configuration

AVM has no required global configuration file. Projects may commit `.argocd-version`, and these
environment variables are recognized:

| Variable | Purpose |
| --- | --- |
| `AVM_HOME` | Override the default `~/.avm` data directory. |
| `AVM_ARGOCD_VERSION` | Select one exact version for the current process and its children. |
| `AVM_GITHUB_TOKEN` | Optional GitHub token for higher API rate limits. |
| `GH_TOKEN` | Token fallback after `AVM_GITHUB_TOKEN`. |
| `GITHUB_TOKEN` | Final token fallback, useful in GitHub Actions. |
| `AVM_GITHUB_API_URL` | Override the Argo CD releases API endpoint for a mirror or test fixture. |

Every management command also accepts the global `--avm-home <DIR>` option for a one-command
override of `AVM_HOME`. When using a custom home persistently, run
`avm --avm-home <DIR> setup` so the dispatcher from that home is the one configured in PATH.

Tokens are sent only to same-origin GitHub API metadata requests. AVM rejects cross-origin API
redirects and pagination links and uses a separate unauthenticated client for release-asset
downloads.

## Data layout

```text
~/.avm/
|-- bin/
|   `-- argocd[.exe]
|-- versions/
|   `-- v3.4.5/
|       |-- argocd[.exe]
|       `-- install.json
|-- state/
|   |-- default
|   `-- dispatcher.json
|-- cache/
|   `-- releases.json
`-- locks/
    |-- state.lock
    `-- v3.4.5.lock
```

Every installed release has `install.json` metadata with its recorded SHA-256 digest and
verification source. The dispatcher and persisted selections refer only to canonical exact tags.

## Supported Argo CD assets

AVM maps the current host to an exact asset published by Argo CD:

| Host | Argo CD asset | AVM binary |
| --- | --- | --- |
| macOS x86-64 | `argocd-darwin-amd64` | Published |
| macOS Apple Silicon | `argocd-darwin-arm64` | Published |
| Linux x86-64 | `argocd-linux-amd64` | Published |
| Linux ARM64 | `argocd-linux-arm64` | Published |
| Linux POWER little-endian | `argocd-linux-ppc64le` | Build from source |
| Linux IBM Z | `argocd-linux-s390x` | Build from source |
| Windows x86-64 | `argocd-windows-amd64.exe` | Published |

Unsupported hosts fail with an explicit error instead of guessing an asset name.

## Design and security

- [Architecture and invariants](docs/architecture.md)
- [Security policy](SECURITY.md)
- [Contributing](CONTRIBUTING.md)

Checksums detect corruption and release-asset substitution, but a checksum hosted with the release
is not independent publisher authentication. Argo CD publishes SLSA provenance for modern CLI
releases; provenance verification is a planned hardening layer.

## License

Licensed under the [Apache License 2.0](LICENSE).
