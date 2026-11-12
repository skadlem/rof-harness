# `--exec-policy any` on the DNA pair, and the named-deliverable signal (2026-11-11)

Two paid runs (frozen recipe 300k/60/120, `--exec-policy any`) plus a zero-cost analysis of
the named deliverable across every banked trace.

## 1. The allowlist was real, and removing it changed nothing measurable

| task | denials, allowlist | denials, `any` | writes | tests | verdict |
|---|---:|---:|---:|---|---|
| dna-assembly | 6 | **0** | 10 -> 6 | 0/1 -> 0/1 | Unresolved (tokens) |
| dna-insert | 16 | **0** | 5 -> **10** | 0/1 -> 0/1 | Unresolved (tokens) |

22 denials removed, one task doubled its writes, **no test flipped**. So the toolchain block
diagnosed in `2026-11-11-three-misses.md` was real but not sufficient — exactly what that
report itself warned ("removing the block is necessary, not proven sufficient").

## 2. The actual cause is visible in both workdirs

Both tasks require **`primers.fasta`** — `instruction.md` names it twice ("The output fasta
file should be titled primers.fasta"). Neither run created it. The workdirs hold scratch
artefacts instead (`align.pl`, `tm.pl`, `scan2.pl`, `cand.txt`, `diff.py`). Same signature as
`make-mips-interpreter`, which never wrote `vm.js`.

Three of three "published-easy" misses therefore share one mechanism: **the agent produces
scratch files and never the named deliverable**, and the run ends on the token cap first.

## 3. The named-deliverable signal, over 18 banked runs

Deliverable filenames taken from each public `instruction.md`; write/edit paths matched by
basename.

| checkpoint k | runs with no deliverable yet | of those, solved | P(success) | pooled |
|---:|---:|---:|---:|---:|
| 5 | 17 | 2 | 0.12 | 0.11 |
| 10 | 15 | 1 | 0.07 | 0.11 |
| **15** | **13** | **0** | **0.00** | 0.11 |
| 20 | 13 | 0 | 0.00 | 0.11 |

Both solved runs had written their deliverable by step 12 (break-filter 10, headless-terminal
12). No run that failed to create its named deliverable by step 15 ever solved.

**Read this as structure, not statistics.** For a single-file task the verifier reads that
file; if it does not exist the task cannot pass, so the predicate is near-definitional. The
empirical 0/13 is consistent with that and cannot establish it: with zero successes the 95%
upper bound is ~23% (rule of three), which straddles the pooled rate. What the 18 runs *do*
establish is the safety number the literature asks for — **a step-15 gate would have killed 0
of 2 successful runs**.

## 4. What that supports

- **Not** a learned stuck detector (chance-level within-task AUC, and our own
  `2026-11-11-hf-within-task.md` test refuted the *any-write* version).
- **Yes** a deterministic named-deliverable predicate, used two ways:
  1. **Stopping / reallocation:** if the named deliverable is absent at step 15, end the run
     and spend the saved tokens on another task. That is the between-task allocation the
     literature supports (P18/P2), driven by a signal that costs zero inference.
  2. **Forcing:** inject "the required output `<path>` does not exist yet" as a directive and
     keep the run alive. This is the *narrow* version of the write-gate that failed on `hf`:
     it keys on the required path, not on any write, and it would have fired on all three
     published-easy misses.
- **Must be measured before enabling:** the forcing arm on a fresh development task, two arms,
  token-matched; and a shadow count of false stops on every future successful run. The
  shadow number today is 0 of 2, which is too few to trust on its own.
