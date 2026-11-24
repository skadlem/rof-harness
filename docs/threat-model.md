# Threat model

Mirrors the in-code statement (`crates/tools-std/src/lib.rs`) and the
CLI guards (`crates/rof/src/workdir.rs`).

## What the tools enforce

- Command allowlist for `exec`/`test`: `--allow-cmd` binary prefixes,
  skipped only by the explicit `--exec-policy any` (see below).
- Path policy for file tools: root-anchored to the
  policy root (a leading `/app/` is that root, so the workspace can be
  addressed as task statements spell it), symlink-safe (a symlinked
  directory must not launder writes out of the tree). Secrets globs apply
  to requested and resolved paths and to recursive search descendants.
- `--workdir` must be a disposable scratch directory; refused (exit 2)
  for `/`, `$HOME`, the harness checkout, and repos with uncommitted
  changes unless `--allow-dirty-workdir` is passed.
- Credentials: the API key is read from the env var named by
  `--api-key-env` (default `OPENAI_API_KEY`); missing credentials fail
  fast before any spend (exit 4). The key never appears in argv or logs.
- `exec`/`test` children get a cleared environment: PATH, HOME, USER, LANG,
  LC_ALL, TERM, TMPDIR, CARGO_HOME and RUSTUP_HOME, plus names given with
  `--pass-env NAME`. A name ending in `API_KEY` is never forwarded; a key
  under any other name (for example `GO_KEY`) is forwarded if listed.
- `--mcp` servers are spawned with this process's full environment (no
  clearing), so they can read the provider key.

## What the tools do NOT enforce

- `exec` and `test` run allowlisted host commands, but allowlisting is
  not isolation: permitting `cargo test`, `sh`, or any test runner
  hands the agent arbitrary code execution on the host. The list can be
  skipped entirely with `--exec-policy any`, which is recorded in the
  report provenance; it exists for callers whose process boundary is
  elsewhere (in-container eval wraps every exec as
  `docker exec <container>`, so the container — not the list — is the
  boundary), and it removes even the hygiene the list provides.
- The path policy does not extend to `exec`/`test`; there is no
  filesystem sandbox.
- OS-level isolation (container/namespace, no network, read-only
  mounts) is the operator's job; the tools build no sandbox.
- In-container evaluation starts the agent leg without `/tests`; the frozen
  tests are copied in at grade time (`crates/rof/src/eval_agent.rs`), so the
  agent cannot read the grader. `/logs` is mounted writable during the agent
  leg, so reward files are not protected from agent commands. Verifier
  artifacts are cleared immediately before grading.

## Trust boundaries

- Model output is untrusted input: tool calls are parsed and gate
  through the registry; proof gating (`--proof-cmd`) can bound which
  hunks commit, but cannot make host exec safe.
- The WAL and snapshot tree assume the workdir is disposable; dirt in
  the workdir would otherwise leak into the reported patch.

## Not inferable from code

- The intended deployment isolation for production use.
- Any network egress policy; none is implemented.
