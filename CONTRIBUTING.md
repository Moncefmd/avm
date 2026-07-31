# Contributing

Contributions are welcome. Every change should keep AVM's command behavior, safety invariants, and
cross-platform operation explicit.

## Development setup

AVM uses Rust 1.97.1 and the 2024 edition:

```sh
rustup show
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
bash -n scripts/install.sh.in
shellcheck scripts/install.sh.in
```

Commit `Cargo.lock`; AVM is an application and release builds must be reproducible.

## Product contract

AVM has a deliberately small command surface:

```text
init [--shell <shell>] [--no-completion] [--dry-run]
uninit [--dry-run] [--remove-path]
install [selector]
default <selector> | default --unset
pin <selector> | pin --unset
exec <selector> -- <argocd arguments>
status
list
available
info <selector>
uninstall <exact-version>
completion <shell> [--install [--dry-run]]
doctor
```

Installation, initialization, persistent selection, cleanup, and one-off execution are separate
actions. `install` never changes a selection, and `default`/`pin` never initialize shell
integration. The ordinary dispatcher resolves `AVM_ARGOCD_VERSION`, the nearest project pin, then
the personal default. Only `exec` takes an explicit per-invocation selector.

Selectors are `stable`, a major line such as `3`, a minor line such as `3.4`, or an exact release.
Persisted state always contains a canonical exact tag.

## Test expectations

Changes should add focused coverage at the lowest useful layer:

- selector and path validation in unit tests;
- platform mapping for every affected target;
- checksum, pagination, retry, and API behavior with local HTTP fixtures;
- install, default, pin, exec, and uninstall transitions under a temporary `AVM_HOME`;
- end-to-end CLI behavior in `tests/cli.rs`;
- dispatcher resolution for environment, nearest project pin, and personal default;
- launcher relocation across an AVM package upgrade and exact argument/exit forwarding;
- ownership-checked, idempotent `uninit` behavior that preserves downloaded versions and selection;
- Windows-specific behavior when a change touches executable naming, replacement, PATH, or locking.
- bootstrap installer mapping, version validation, strict checksum parsing, ownership, atomic
  replacement, idempotence, and initialization behavior when installer templates change.

Tests must not use the developer's real `~/.avm`, mutate a real shell profile, or depend on live
GitHub unless they are an explicitly documented manual smoke test.

## Safety expectations

Never relax:

- strict SemVer parsing before path or URL construction;
- path containment and symlink refusal;
- verified, staged installation;
- atomic state and project-pin replacement;
- dispatcher ownership checks;
- authenticated API origin restrictions;
- unauthenticated release-asset downloads;
- the fixed per-version then state-lock ordering;
- offline, read-only behavior for ordinary `argocd` execution.

New network operations need explicit timeout, redirect, retry, size, and credential rules. New
filesystem mutations need a containment proof, ownership rule, interruption behavior, and focused
tests.

`scripts/install.sh.in` and `scripts/install.ps1.in` each contain exactly one
`@AVM_RELEASE_VERSION@` placeholder. The release workflow replaces it with the tag and publishes
the rendered scripts; never publish or document a raw template as an executable installer.

## Pull requests

Explain:

1. the user-visible outcome;
2. the command or state contract affected;
3. filesystem and network failure behavior;
4. tests run on each relevant operating system; and
5. any release impact.

Keep generated binaries, generated completion scripts, local caches, and toolchain directories out
of Git.
