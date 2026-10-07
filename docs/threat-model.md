# Threat model

Mirrors the in-code statement (`crates/tools-std/src/lib.rs`) and the
CLI guards (`crates/rof/src/workdir.rs`).

## What the tools enforce

- Path policy for `view`/`edit`/`write` only: root-anchored to the
  policy root, symlink-safe (a symlinked directory must not launder
  writes out of the tree), secrets-denied file access.
- `--workdir` must be a disposable scratch directory; refused (exit 2)
  for `/`, `$HOME`, the harness checkout, and repos with uncommitted
  changes unless `--allow-dirty-workdir` is passed.
- Credentials: the API key is read from the env var named by
  `--api-key-env` (default `OPENAI_API_KEY`); missing credentials fail
  fast before any spend (exit 4). The key never appears in argv or logs.
- `--pass-env NAME` forwards env vars to `exec`/`test` children;
  provider-key-shaped names stay withheld at spawn.

## What the tools do NOT enforce

- `exec` and `test` run allowlisted host commands, but allowlisting is
  not isolation: permitting `cargo test`, `sh`, or any test runner
  hands the agent arbitrary code execution on the host.
- The path policy does not extend to `exec`/`test`; there is no
  filesystem sandbox.
- OS-level isolation (container/namespace, no network, read-only
  mounts) is the operator's job; the tools build no sandbox.

## Trust boundaries

- Model output is untrusted input: tool calls are parsed and gate
  through the registry; proof gating (`--proof-cmd`) can bound which
  hunks commit, but cannot make host exec safe.
- The WAL and snapshot tree assume the workdir is disposable; dirt in
  the workdir would otherwise leak into the reported patch.

## Not inferable from code

- The intended deployment isolation for production use.
- Any network egress policy; none is implemented.
