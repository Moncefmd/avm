# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use the repository's private
**Security → Report a vulnerability** flow on GitHub so details can be investigated before
disclosure.

Include:

- the AVM version and operating system;
- the affected command and exact arguments, with tokens removed;
- whether `AVM_HOME` or `AVM_GITHUB_API_URL` was overridden;
- a minimal reproduction and the expected security boundary;
- any evidence that a release asset, checksum, dispatcher, or path escaped its expected location.

Do not include GitHub tokens, private filesystem contents, or other credentials.

## Supported versions

Until AVM reaches 1.0, security fixes are made on the latest `0.x` release line. Users should update
to the newest available patch release.

## Integrity model

AVM verifies every normal install against:

1. the `sha256:` digest supplied for the exact asset by GitHub's release API; or
2. the exact filename entry in Argo CD's published checksum manifest.

Historical releases that provide neither are rejected unless the user passes
`--allow-unverified`. That flag is an explicit reduction in protection and should not be used in
automation.

AVM records the digest for an exact release tag and refuses a later published digest change while
that version remains installed. Accepting such a mutable-tag change requires explicitly
uninstalling the recorded version first.

The current checksum model protects against transfer corruption and mismatched release assets. A
checksum obtained from the same release is not independent proof of publisher identity. Argo CD
publishes SLSA provenance for modern CLI releases; AVM provenance verification remains planned.

## Token handling

An optional token can be supplied through `AVM_GITHUB_TOKEN`, `GH_TOKEN`, or `GITHUB_TOKEN`.
Tokens:

- are used only for GitHub API metadata requests;
- are restricted to the configured API origin, including redirects and pagination;
- are never written to disk or normal output;
- are marked sensitive in the HTTP client;
- are not attached to the separate release-download client.

A read-only token is sufficient.

## Filesystem boundaries

AVM validates version input as SemVer, restricts version operations to direct children of
`$AVM_HOME/versions`, rejects symlinks at managed directories and version directories, stages
downloads before commit, and refuses to overwrite an unmarked regular
`$AVM_HOME/bin/argocd[.exe]`.
