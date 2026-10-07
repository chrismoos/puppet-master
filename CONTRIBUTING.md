# Contributing

Puppet Master is MIT licensed. There is no contributor license agreement and no
sign-off gate: open a pull request and it is taken as offered under the same
license as the rest of the tree.

The project is young and changing quickly. For anything larger than a bug fix,
open an issue first and describe what you want to change — it may already be in
progress, or the design may be about to move under you.

## Building from source

You need:

- Rust, stable, via rustup. Run `rustup update stable` to match CI before
  investigating a compiler or Clippy result that differs locally.
- A C compiler (`cc`, or the Xcode command line tools on macOS). `rusqlite`
  builds SQLite from bundled C source.
- `lld` on Linux, for fast linking.
- Node.js 22 and `pnpm` for the web bundle and its tests.
- `cargo-nextest`, which runs the Rust suite: `cargo install cargo-nextest`.

No `protoc` is needed — the protobuf contract is compiled at build time by
`protox`, in pure Rust. `buf` is only needed for the proto lint and
breaking-change checks that CI runs.

The `Makefile` drives everything, and `make help` lists the targets. The web UI
is embedded into the binary, so a build compiles the bundle first:

```sh
pnpm install      # once, at the repository root
make build        # web bundle + release binary
make run          # build everything and run the daemon with the web UI
```

Run the daemon from a **release** build. Debug builds exist for the test suite
and are much slower to drive interactively. The binary lands at
`target/release/pm`.

For frontend work, `make dev` runs Vite with hot reload against a separately
running daemon. After a web change, rebuild the bundle and the binary
(`make web && make rust`) before running a release build — it embeds the bundle
at compile time.

Give any daemon you start for development its own socket, database and port so
it cannot collide with one you actually use:

```sh
pm daemon --socket /tmp/pm-dev/pm.sock --db /tmp/pm-dev/pm.db \
  --scrollback-dir /tmp/pm-dev/sb --http 127.0.0.1:7777
```

## CI Rust setup

GitHub and Forgejo workflows run `sh scripts/ci-rust.sh` to install the stable
Rust toolchain with rustfmt and clippy directly through rustup. Existing rustup
installations are reused. If rustup is absent, the script downloads Rust's
official installer without modifying shell profiles. Cargo's bin directory and
the stable toolchain selection are passed to subsequent workflow steps.

Minimal Debian or Ubuntu runners also need a C compiler, lld, make, pkg-config,
and curl. The script installs missing prerequisites with apt, using root or
passwordless sudo. Other runner images must provide those prerequisites.
Existing packages are not upgraded during setup, and a readable certificate
bundle is preserved instead of reinstalling ca-certificates. This supports
runner images with read-only certificate mounts. Download, package
installation, or toolchain failures stop the job.

The setup script's regression tests use stub commands and temporary directories
without downloading Rust or changing the host: `node --test scripts/ci-rust.test.mjs`.

Linux CI also runs mobile typechecks and unit tests, TestFlight script tests,
controller-fixture unit tests, and E2E source-policy checks. Native iOS builds
and device tests still require macOS and Xcode.

## Running the tests

Tests are part of a change, not a follow-up. Design each one for the layer it
lives at: unit tests exercise a unit's own logic, integration tests exercise
the seams between components, end-to-end tests exercise observable behavior
through the real transport. Do not reach through a layer to assert on internals
its contract does not promise.

Never make a test pass by asserting the wrong behavior. If the code produces a
wrong result, the test must fail — do not copy the buggy output into the
expectation, loosen the assertion, or skip the test. If you think the current
behavior is wrong but out of scope, say so in the pull request instead of
writing a test that blesses it.

```sh
make check              # fmt, clippy, web typecheck, source policy checks
make test               # Rust (nextest, debug) + web tests
make e2e-integration    # the fast functional Chromium suite
```

Those three are the gates. `make check` and `make test` must pass before you
commit — not before review, before the commit exists — and CI fails on either.
`make e2e-integration` is what qualifies a branch for merge, run on the commit
that already contains current `master`.

Reach for a real browser when the behavior depends on xterm, WebGL or canvas,
layout, focus, routing, resizing or selection. A focused run takes explicit
spec paths:

```sh
make e2e-worker E2E_SPECS=e2e/terminal-scrollback.spec.ts
```

Browser spawn tests use the installed Codex test agent explicitly. For transient
scrollbar assertions, install the Playwright clock before navigation and pause
it only around the interaction and idle timer checks. Advance rendering frames
before checking the thumb, then resume the clock for unrelated work. Terminal
switch tests sample completed render events because synchronized output can
change the buffer while the previous screen remains painted.
Wait for terminal selection and its initial focus transfer before testing
keyboard navigation elsewhere on the page.

`make e2e-terminal-heavy` covers terminal lifecycle, replay and high-volume
scrollback. `make e2e-performance` is the only lane that produces release
performance numbers; it builds real release binaries, runs scenarios with one
browser worker without trace or screenshot capture, and must run with nothing
else competing for the machine.
Latency measurements are saved under the test output directory.

Changes under `apps/mobile` are only meaningfully verified on macOS with Xcode.
A Linux machine can run the mobile unit lanes and nothing more, so say plainly
in a pull request which mobile behavior you did not verify.

## Commit conventions

Commit conventions are enforced by review.

- **Imperative subject, ending in a period.** "Hide the terminal diagnostics
  toggle." Put the reasoning in the body.
- **No semicolons in commit messages.** If the subject wants one, make it
  briefer and move the rest into the body.
- **Describe the change and its rationale.** Keep commit messages focused on
  what changed and why.
- **No external URLs** in code, comments, commit messages, docs or tests
  without asking first. That includes data provenance, reference material and
  tool homepages.
- **Never commit a secret.** Dev-local placeholder values that are not real
  secrets are the only exception.
- **Do not edit the version in `Cargo.toml`.** Releases are cut by
  `scripts/publish.mjs`, which owns the version end to end.

## Code style

The default is **no comment**. A comment exists only to carry context the code
cannot — a non-obvious invariant, a spec mapping, an operational hazard, or the
why behind a non-obvious choice — and only while that stays true. One plain
sentence, then stop.

Never narrate what code does. Comments that walk through the steps ("loop over
the entries", "build the request and send it") are not wanted: the code already
says that. If a block seems to need a caption, rename or extract instead. No
banner comments, no per-line annotations, and no commentary about the change
itself — "now we also handle X" belongs in the commit message. Delete a comment
that restates the code or has gone stale rather than rewording it.

Log statements are not comments. `info!`, `warn!` and `debug!` are the
operator-facing observability surface and should stay verbose enough to debug a
live daemon.

Named constants, not constant-looking numbers: protocol codes, bit masks,
header lengths, field offsets, timers and sentinels get a name or a typed enum.
Small integers are fine when they are ordinary arithmetic or indexes.

Errors are typed enums (`thiserror`) at library boundaries and `anyhow` only in
binaries and tests. Generated wire types stay inside `pm-protocol`; everything
else programs against the domain types. State transitions and message dispatch
use exhaustive `match`, so a new enum variant cannot be silently absorbed by a
catch-all arm.

Do not name internal planning artifacts — phase numbers, milestone codes,
sprint names — in anything shipped with the repository. State the actual
constraint instead, so a reader can act on it without context the repository
does not carry. References to external specification sections are fine.

## Releasing

Releases are cut on macOS with `make publish`, which bumps the patch version,
runs the gates, builds every target, signs and uploads them, and tags. Pass
`BUMP=minor` or `BUMP=major` to go further.

A release can go to a channel first: `make publish CHANNEL=dev` publishes a
pre-release such as `0.10.0-dev.3` that only hosts on `pm update --channel
dev` see. Later channel builds count up within the same target version, and
`BUMP=minor` retargets. When a channel build is good, `make promote
FROM=0.10.0-dev.3` rebuilds that commit as `0.10.0` on the `release/0.10`
branch, publishes it as stable, and merges the branch back into master. A
hotfix to a stable line is a plain `make publish` on its `release/X.Y` branch.

Published versions are never removed: a controller may run any of them and its
workers fetch it by exact version. A channel release always ships its worker
image for the same reason.

## Changing the worker protocol

Any change to a type reachable from `WorkerMessage`, `ControllerMessage`,
`RepoOp` or `RepoAnswer` changes the contract between a controller and its
workers, which are independently updated and must interoperate. Before landing
one: bump `WORKER_PROTOCOL_VERSION`, add a named first-supported-version
constant, gate every sending path including reconnect and automatic resume, and
keep the old path working through a shim or a clear capability refusal. A
version bump on its own is incomplete if an older peer can still be sent the
new capability.

## Documentation

Document user-facing features, new CLI commands and flags, and new
configuration in the same change that introduces them, including the default
behavior, how it interacts with existing state, the important failure or
disabled states, and a short example. Update the existing files rather than
starting a parallel source of truth.

## Reporting a vulnerability

Do not open a public issue. `SECURITY.md` describes the trust model, the
deliberate limits, and how to report privately.
