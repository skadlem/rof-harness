# Three published-easy misses — what the traces say (2026-11-11)

Zero-spend trace read of the three Slice A cells that score 0 against a published
per-task frontier pass rate of 0.88-0.96: `make-mips-interpreter` (p=0.96),
`dna-assembly` (p=0.96), `dna-insert` (p=0.88). No model call, no agent run, $0.

Inputs: `/tmp/slice-a-faithful/<task>/<task>/` (300k/60/120) and
`/tmp/slice-a-envelope/<task>/<task>/` (1M/120/240) — `events.jsonl`,
`verifier-logs/verifier/{ctrf.json,reward.txt}`, `verifier-stdout.txt`; the public
`instruction.md` per task. Line numbers below are `events.jsonl` lines. No test source or
hidden fixture was read: the `/tests` detail in §1.5, §2.5 and §3.5 is the verifier's own
traceback from `verifier-stdout.txt`.

> Now (2026-11-24): both `/tmp/slice-a-*` trace directories are gone, so these numbers
> cannot be re-derived in this checkout; they stand as measured at read time.

## Run facts (measured, from `MessageEnd.usage` summed per run)

| task | arm | steps | tool calls | in tok | out tok | gross | halt | report tests |
|---|---|---:|---:|---:|---:|---:|---|---|
| make-mips-interpreter | faithful | 28 | 42 | 292,745 | 11,036 | 303,781 | `Failed: tokens` | 0/3 |
| make-mips-interpreter | envelope | 68 | 99 | 988,318 | 31,796 | 1,020,114 | `Failed: tokens` | 0/3 |
| dna-assembly | faithful | 36 | 44 | 266,847 | 42,818 | 309,665 | `Failed: tokens` | 0/1 |
| dna-assembly | envelope | 70 | 80 | 885,185 | 127,633 | 1,012,818 | `Failed: tokens` | 0/1 |
| dna-insert | faithful | 32 | 49 | 244,976 | 55,885 | 300,861 | `Failed: tokens` | 0/1 |
| dna-insert | envelope | 75 | 99 | 829,454 | 176,415 | 1,005,869 | `Failed: tokens` | 0/1 |

Gross = input+output, the shape the report provenance records (`budget_shape: "Gross"`,
`budget_tokens: 300000|1000000`, `budget_steps: 60|120`, `exec_policy: "Allowlist"`,
`steer: false` in all six `*.report.json`). All six sums land within 2% of the cap, so
**tokens terminated every run, never steps**: 68-75 of 120 steps used at 1M.
`reasoning_tokens` is populated (23,143 / 117,199 / 164,881 in the three envelope runs) and
charged inside `output_tokens`.

`RunEnd` is `{"outcome":{"Failed":"tokens"}}` in all six, with no completion message.
Prose is 1-3 messages per run, 43-121 characters per run, so **intent is not recoverable
from the trace** — only tool behaviour is.

---

## 1. make-mips-interpreter

### 1.1 What did the agent build?

**Nothing at the required path. `vm.js` was never written, in either arm.** Write calls: 2
(faithful), 5 (envelope), none `vm.js`:

| arm | writes | targets |
|---|---:|---|
| faithful | 2 | `elfinfo.js` (L82), `elfinfo.js` (L135) |
| envelope | 5 | `scan.js` (L269), `mk.js` (L303), `scan2.js` (L313), `scan3.js` (L330), `scan4.js` (L340) |

`vm.js` appears in either `events.jsonl` only in the goal text (L1) and the agent's
`cat .rof-events.jsonl` dump (L15 faithful, L60 envelope). The writes are reconnaissance,
not deliverables — L269 (`call_00_SJ4Bvkrb8bfZqy5CYfbM2506`):

```
{"type":"ToolStart","name":"write",
 "args":{"content":"const fs = require('fs');\nconst b = fs.readFileSync('doomgeneric_mips');\n
  … for (const s of segs) { … ops[op] … functs[f] …","path":"scan.js"}}
```

— a program-header walk plus a MIPS opcode histogram; L303 `mk.js` parses the linker map
into `syms.json`.

### 1.2 Reached or blocked?

| arm | is_error | denied | binary absent (`OCI runtime exec failed`) | shell-op guard | timeout |
|---|---:|---:|---:|---:|---:|
| faithful | 6 | 0 | 1 (`file`) | 0 | 0 |
| envelope | 17 | 1 (`readelf -h doomgeneric_mips`) | 2 (`file`, `xxd`) | 1 | 0 |

- L13-15: `file doomgeneric_mips` → `OCI runtime exec failed: … exec: "file": executable file not found in $PATH`
- L14-16: `readelf -h doomgeneric_mips` → `denied: command not allowlisted: readelf -h doomgeneric_mips`
- L35-37: `xxd -l 64 doomgeneric_mips` → `executable file not found in $PATH`
- L372: `grep -n "asm\|v0\|__NR\|…" doomgeneric/doomgeneric/my_stdlib.c | head -60` → `failed: exec has no shell and stdin is closed: '|' is a literal argument here`

**Toolchain: not blocked.** The deliverable is a JS interpreter, so no MIPS compiler is
needed, and `node` is present and allowlisted (`node --version` succeeds at call 5 of the
envelope run). `file`/`readelf`/`xxd` are conveniences it replaced with its own `node -e`
parsers within 2 calls.

**Harness artefact that cost repeated calls: quoted `grep` alternation returns nothing.**
11 envelope greps used `\|` and came back empty; the same literals matched as one quoted
pattern or via `-e`. Same file: L242 `grep -n "real_syscall6\|SYS_" doomgeneric/doomgeneric/my_stdlib.c`
→ `""` (`is_error:true`); L247 `grep -n real_syscall6 doomgeneric/doomgeneric/my_stdlib.c`
→ 778 bytes of hits. Never diagnosed; it kept issuing new greps.

> Now: fixed — `split_cmd` keeps `\|` inside double quotes
> (`crates/tools-std/src/runner.rs:20`, test `crates/tools-std/src/lib.rs:1348`, commit
> `db10e63` 2026-11-13).

### 1.3 Where did the tokens go?

Envelope 99 calls = exec 76, search 9, view 9, write 5; exec heads `grep` 28, `node` 20,
`sed` 12, `ls` 8, `head` 2, `cat` 2, `file`/`readelf`/`xxd`/`wc` 1 each. Faithful 42 =
exec 27, view 11, search 2, write 2; `grep` 12, `cat` 5, `head` 4, `ls` 3, `node` 2, `file` 1.

Dominant: **re-reading the two provided C sources and the linker map** — no compile phase
(no compiler), no test-fix loop (no failing test existed; it never ran `node vm.js`).
Envelope repeats:

- `doomgeneric_img.c` read 8 times — calls 23, 32, 40, 52, 60, 67, 95 (two full-file dumps)
- `my_stdlib.c` read 9 times — calls 26, 28, 43, 53, 61, 64, 94, 98, 99
- `doomgeneric_mips.map` touched 15 times — calls 9, 11, 12, 13, 21, 22, 70, 71, 74, 75, 80, 81, 87, 89, 90

Max duplicate is `ls -la /app` ×2: no command on a loop, just the same files re-opened
with different windows.

Tool output totals 193,120 bytes ≈ 48k tokens against 988,318 input tokens, so **≥90% of
the input budget is re-sent context, not new information.** Mean input/step 14,534 tokens,
last step 24,415: every step re-sends the conversation, so read-heavy reconnaissance is
quadratically priced.

### 1.4 Did it converge?

No. Last 10 calls of the envelope run (90-99): 9 of 10 read the two C sources or the map,
0 writes. Last 3, verbatim:

```
[097] L395 exec  grep -n "SYS_" doomgeneric/doomgeneric/my_stdlib.c        (2,651 B of hits)
[098] L401 exec  sed -n 180,300p doomgeneric/doomgeneric/my_stdlib.c       (4,529 B)
[099] L402 exec  sed -n 1620,1730p doomgeneric/doomgeneric/my_stdlib.c     (2,672 B)
```

Faithful last 3: `view my_stdlib.c` (L162), `grep -n "real_syscall6\|SYS_" my_stdlib.c`
(L167, empty), `grep -n "DOOMGENERIC_RES" doomgeneric/doomgeneric.h` (L168).

The halt caught it reading the syscall shim — not fixing a test, not re-reading an error,
not looping on one command — extending reconnaissance of a 1.5 MB binary and its source
until the budget ran out. Whether it had a design: **trace does not say**.

### 1.5 What does the verifier actually require?

CTRF failing names: `test_outputs.py::test_vm_execution`, `::test_frame_bmp_exists`,
`::test_frame_bmp_similar_to_reference`.

One-line requirement: *run `node vm.js` so that it executes the supplied MIPS ELF and saves
the rendered frames one at a time, with the first frame correctly created.* Mapping:
`test_vm_execution` ↔ "I can run `node vm.js`"; the two frame tests ↔ "saving the frames as
they are rendered, one at a time … the first frame is correctly created and saved".

**Partial gap, stated as a finding:** the output path and format (`/tmp/frame.bmp`, BMP)
are *not* in `instruction.md` but in the provided source, at the line the agent read
(envelope call 51, L194) →
`167:    printf("DoomGeneric initialized. Frames will be saved to /tmp/frame.bmp\n");`.
Justifiable from a public artefact, not from the task statement: a solver saving frames
anywhere else scores 0 on a requirement it was never told.

### 1.6 Verdict

**`budget-loop`** — 99 calls, 0 writes of the deliverable; the whole 1M gross budget went
to re-reading two provided sources plus binary/symbol archaeology, and the halt caught it
mid-read. (`wrong-approach` is the adjacent reading — it analysed the binary instead of
implementing a VM. `artefact-form` is ruled out: there is no artefact.)

---

## 2. dna-assembly

### 2.1 What did the agent build?

**Nothing at the required path. `primers.fasta` was never written, in either arm.**
Verifier: `E AssertionError: File /app/primers.fasta does not exist.` All writes are
analysis scripts:

| arm | writes | targets |
|---|---:|---|
| faithful | 10 | `solve.py` (L13), `t.pl` (L81), `len.awk` (L127), `out.awk` (L137), `cmp.awk`, `cmp2.awk`, `cmp3.awk` ×2, `align.awk`, `b.awk` |
| envelope | 23 | `work/analyze.py`, `work/an.pl`, `work/an2.py`, `work/t.sh` ×2, `work/{len,an3..an9,map,map2,map3,jun,j2,ver,dbg,lines,mm,d2}.awk` |

Envelope L366 `write work/mm.awk` (`call_00_v6Y9uvDKPTODcBbN9Bo72354`) is representative: a
FASTA parser (`/^>/ { name=substr($0,2); …`) plus a character-by-character comparison of
one fragment against the output.

### 2.2 Reached or blocked?

| arm | is_error | denied | binary absent | shell-op guard |
|---|---:|---:|---:|---:|
| faithful | 12 | 6 | 3 (`python3` ×2, `node`) | 0 |
| envelope | 17 | 10 | 3 (`python3` ×2, `gcc`) | 1 (L154) |

Faithful denials: `which oligotm` (L28), `apt-get install -y primer3` (L62), `perl -v`
(L75), `perl t.pl` (L86), `perl -e print "hi"` (L110), `perl t.pl extra` (L115).
Envelope: `which oligotm primer3_core` (L27), `oligotm -tp 1 -sc 1 -mv 50 -dv 2 -n 0.8 -d 500
ATGCATGCATGCATGCATGC` (L29), `apt-get install -y --no-install-recommends primer3` (L77),
`perl -v` (L82), `perl work/an.pl` (L92), `bash work/t.sh` (L133), `sh work/t.sh` (L148),
`oligotm …` (L160), `/usr/bin/perl -v` (L269), `perl -e "print 1+1"` (L281). Shell-op
guard: `grep -c '>' sequences.fasta` (L154) → `failed: exec has no shell and stdin is
closed: '>' is a literal argument here`. Zero timeouts.

**A missing toolchain is a real finding here, and part of it is harness-imposed, not
image-imposed.** `ls /usr/bin` (faithful L24, envelope L19) shows `perl`, `bash`, `sh`,
`awk`, `sed` **present**; `python3`, `gcc`, `node`, `oligotm` **absent**. Two options lost:

- `perl` — installed, denied 8 times, including by absolute path (`/usr/bin/perl -v` →
  `denied: command not allowlisted`). Pure allowlist artefact.
- `oligotm` — the tool `instruction.md` designates "the ground truth" for melting
  temperature — absent and denied, its installer `apt-get install primer3` denied (2
  calls). The verifier log shows primer3 2.6.1-4 installed by `test.sh` in the same
  container at grading time, so package and network are reachable; only agent policy
  forbids it.

Only `awk`/`sed` were usable for computation. 45% of faithful calls (20/44) and 28% of
envelope calls (23/80) are environment reconnaissance (`ls`, `find`, `which`, `apt-get`,
bare version probes).

### 2.3 Where did the tokens go?

Envelope 80 calls: `awk` 20, `ls` 11, `find` 4, `perl` 3, `python3` 2, `grep` 2, rest 1
each. Faithful 44: `ls` 10, `awk` 9, `perl` 4, `find` 3, `python3` 2, others 1 each.
Repeated commands: `ls /usr/bin` ×3 (faithful) / **×5** (envelope), `ls /app` ×2 (envelope).

Dominant sink = **environment search, then awk sequence analysis** — no compile phase, no
test-fix loop. 20 awk runs in the envelope arm, 13 of them fresh scripts (`an3..an9`,
`map*`, `jun`, `j2`, `ver`, `dbg`, `lines`, `mm`, `d2`) on one question: how the four
fragments map onto `output`. Same arithmetic as §1.3 — mean input/step 12,645, last step
16,510.

### 2.4 Did it converge?

Not to an artefact. Last 10 calls (71-80) alternate write→run five times:
`write work/ver.awk`/`awk`, `write work/dbg.awk`/`awk`, `write work/lines.awk`/`awk`,
`write work/mm.awk`/`awk`, `write work/d2.awk`/`awk`. Last 3:

```
[078] L361 exec  awk -f work/mm.awk sequences.fasta   -> "egfp vs out[211..]: mismatch … flag vs out[925..]: mismatch …" (dozens)
[079] L366 write work/d2.awk
[080] L371 exec  awk -f work/d2.awk sequences.fasta   -> "flag: ATGGGTAG… / out900-1020: TCTCGGCATGGACGAGCTGTACAAA… / eg tail 30: …"
```

Same failure — fragment-to-output alignment — retried with a new awk script every 2 calls;
no primer constructed at either budget. No `primers.fasta` exists to be wrong.

### 2.5 What does the verifier actually require?

CTRF failing name: `test_outputs.py::test_primers` (1 test, "assertion error"). Verifier
message: `File /app/primers.fasta does not exist.`

One-line requirement: *emit `primers.fasta` with the minimum number of Golden-Gate primer
pairs for the four parts, annealing length 15-45 nt, Tm 58-72 °C by oligotm with the stated
flags, pair Tm spread ≤5 °C, headers `>TEMPLATENAME_DIR`, no blank lines.* File name,
header format, Tm bounds and oligotm flags are all in `instruction.md`, and the failing
assertion names the same file: no unmappable test here.

### 2.6 Verdict

**`toolchain-blocked`.** Two of the three tools the task depends on were unavailable at
design time — `oligotm` (denied, installer denied, while the grader installs it) and `perl`
(installed, denied) — leaving only awk; 16 denials and 28-45% of calls went to the search.
Limitation: no primer was ever constructed, so removing the block is necessary, not proven
sufficient.

---

## 3. dna-insert

### 3.1 What did the agent build?

**Nothing at the required path. `primers.fasta` was never written, in either arm.**
Verifier: `E AssertionError: File /app/primers.fasta does not exist.`

| arm | writes | targets |
|---|---:|---|
| faithful | 5 | `diff.py` (L14), `diff.pl` (L39), `t.sh` (L61), `diff.awk` (L109), `d2.awk` (L183) |
| envelope | 24 | `inspect.pl` ×2, `t.sh` ×2, `len.awk`, `diff.awk`, `ctx.awk`, `ctx.pl` ×2, `ctx2..ctx5.awk`, `show.awk`, `verify.awk`, `tm.awk`, `dbg.awk`, `t2..t5.awk`, `analyze.awk` ×2 |

The last write is a hand-built oligotm substitute (envelope L419): SantaLucia 1998 unified
parameters (`dh["AA"]=-7.6`, `ds["AA"]=-21.3`), primer3 defaults `mv = 50.0`, `dv = 2.0`,
`dntp = 0.8`, `dnac = 500.0`.

### 3.2 Reached or blocked?

| arm | is_error | denied | binary absent | shell-op guard |
|---|---:|---:|---:|---:|
| faithful | 21 | 16 | 2 (`python3`, `gcc`) | 0 |
| envelope | 22 | 13 | 5 (`python3`, `python`, `gcc`, `git` ×2) | 2 (L69, L270) |

Faithful denials (16): `which python3 oligotm primer3_core` (L13), `oligotm` (L21),
`perl diff.pl`, `/usr/bin/perl diff.pl` (L49), `bash -c "perl diff.pl"`, `bash t.sh`,
`perl ./diff.pl`, `perl -e 'print "hi\n"'`, `perl -v`, `mkfifo`,
`getent hosts deb.debian.org`, `env`, `apt-get download primer3`, `apt-get install -y primer3`,
`curl --version`, `apt list --installed`.
Envelope (13): `which oligotm primer3_core python3` (L13), `perl -ne '…'`, `perl inspect.pl`,
`perl -v` (L66), `bash t.sh`, `sh t.sh`, `perl -e "print 6*7"`, `apt-get install -y --no-download primer3`,
`apt-get install -y primer3`, `perl ctx.pl`, `du -r /etc`, **`echo hello` (L318 → `denied: command not allowlisted: echo hello`)**,
`apt-get download primer3` (L408).

Zero timeouts. Same image as dna-assembly: `perl`/`bash`/`sh` present (`ls /usr/bin`,
faithful L28), `python3`/`gcc`/`node`/`oligotm` absent. The agent hunted for the policy
itself — faithful call 25 `find / -name "*allow*" -not -path "/proc/*"` — a signature of
allowlist confusion, not of a missing tool.

**Toolchain: blocked on the same three fronts as §2.2**, and biting hardest — 59% of
faithful calls (29/49) and 41% of envelope calls (41/99) are recon.

### 3.3 Where did the tokens go?

Faithful 49 calls: `ls` 10, `awk` 6, `find` 5, `perl` 4, `bash` 2, `apt-get` 2, others 1
each. Envelope 99: `awk` 19, `ls` 16, `find` 13, `perl` 5, `apt-get` 3, `grep` 2, `git` 2,
`cat` 2, others 1. Repeated commands: `ls /usr/bin` ×3 (faithful) / ×4 (envelope),
`ls -la /app` ×2, `awk -f ctx.awk /app/sequences.fasta` ×2,
`awk -f ctx5.awk /app/sequences.fasta` ×2.

Dominant: **environment search** (16 `ls`, 13 `find`; the filesystem-wide hunt for
`oligotm`/`primer3`/`python` runs 5 times), then awk sequence analysis. No compile phase,
no test loop. Mean input/step 11,059, last step 22,015; 51,580 bytes of tool output total —
again nearly all budget is re-sent context.

### 3.4 Did it converge?

Yes, but only in the last handful of calls, too late to emit the file. Last 10 (90-99):
`awk t3`, `write t4.awk`, `awk t4`, `write t5.awk`, `awk t5`, `ls -la /app`,
`apt-get download primer3` (denied), `write analyze.awk`, `write analyze.awk`,
`awk analyze.awk`. Last 3:

```
[097] L414 write analyze.awk
[098] L419 write analyze.awk     (rewrite: SantaLucia parameters, mv/dv/dntp/dnac = primer3 defaults)
[099] L424 exec  awk -f analyze.awk /app/sequences.fasta
      -> insertion_after=215  insert_len=39  insert=tagattagaagaagaattaagaagaagattaacagaaag
         UP100: cgaaaagtgctagtggtgctagccccgcgaaattaatacgactcactatagggtctagaaataattttgtttaactttaagaaggagatatacatatgag
         DOWN100: caagggcgaggagctgttcaccggggtggtgcccatcctggtcgagctggacggcgacgtaaacggccacaagttcagcgtgtccggcgagggtgagggc
         --- FORWARD annealing (top strand downstream of insert) ---  15..23 nt with Tm / Tm_mv / Tm_noCT per length
```

A located insertion site plus a candidate forward-primer Tm table — the task's actual work
— at call 99 of 99, with 2 of the last 10 calls still spent probing the environment.
Whether it would have written `primers.fasta` given another 20-30 calls: **trace does not
say**; the trend says one artefact-writing step away, not one insight away.

### 3.5 What does the verifier actually require?

CTRF failing name: `test_outputs.py::test_primers`. Verifier message:
`File /app/primers.fasta does not exist.`

One-line requirement: *emit `primers.fasta` with the minimum number of Q5
site-directed-mutagenesis primer pairs converting input to output, annealing length 15-45
nt, Tm 58-72 °C by oligotm with the stated flags, pair Tm spread ≤5 °C, forward primer
first in each pair.* Complete public mapping: the failing assertion names the file the
statement names.

### 3.6 Verdict

**`toolchain-blocked`.** The designated Tm ground truth was denied and absent, `perl` and
`bash` were denied although installed, and `echo` itself was denied; 41% of the envelope
budget went to finding a usable interpreter, and it was writing its own SantaLucia Tm model
when the cap hit — right answer shape in hand at the final call, no budget to write it.

---

## 4. Summary (deterministic, sorted)

| task | verdict | dominant token sink | denied | binary absent | deliverable written |
|---|---|---:|---|---:|---|
| dna-assembly | `toolchain-blocked` | environment search (`ls`/`find`/`which` = 45% F / 28% E) then awk fragment alignment | 16 | 6 | no |
| dna-insert | `toolchain-blocked` | environment search (`ls`/`find` = 59% F / 41% E) then awk Tm analysis | 29 | 7 | no |
| make-mips-interpreter | `budget-loop` | re-reading `doomgeneric_img.c` (8×), `my_stdlib.c` (9×), `.map` (15×) | 1 | 3 | no |

Denial census over the six runs: 46 total — `perl` 18, `apt-get`/`apt` 8, `bash`/`sh` 6,
`oligotm` 3, `which` 4, `echo`/`env`/`getent`/`curl`/`du`/`mkfifo` 6, `readelf` 1. 24 of
the 46 refuse a binary `ls /usr/bin` proves is installed in the dna images; 11 more refuse
the task's declared ground-truth tool or the only installer for it.

Step-vs-token: all six halted on **tokens**, at 48-62% of the step cap. Raising steps alone
(already 120 in the envelope arm) cannot help; only shrinking per-step cost, or ending the
run earlier with an artefact, can.

## 5. The one change

**Drop the static exec allowlist for this slice (`--exec-policy any`).** One flag.

From the traces: 24 of the 46 denials name `perl`, `bash` or `sh`, which `ls /usr/bin`
shows present in both dna images — the harness refused tools the image ships, including by
absolute path (`/usr/bin/perl -v`) and after the agent routed around it
(`bash -c "perl diff.pl"`). 11 more name `oligotm` / `apt-get`: the declared ground truth
and its only installer, while the verifier's own `test.sh` installs primer3 in the same
container (primer3 2.6.1-4 in `verifier-stdout.txt`) — so network and package are there and
only agent-side policy stands in the way. Removing it would have returned ~20-40% of both
dna budgets to the task: dna-insert reached candidate primers with a hand-rolled SantaLucia
model at its last call and would likely have written `primers.fasta`; dna-assembly would at
least have stopped hunting for an interpreter.

> Now: the flag exists (`crates/rof/src/eval_cmd.rs:542`) and eval still defaults to
> `Allowlist` (`crates/rof/src/eval_cmd.rs:652`). The arm has since run on two other slices
> with no capability gain — denials 3-9 per row to 0, same tokens, same named tests failing
> ([decisions](../decisions.md), closed 2026-10-18, retested 2026-11-06) — so this stays an untested hypothesis
> for *these* cells.

**What this change does not fix:** `make-mips-interpreter` was not blocked. `node` worked,
no compiler was needed, its single denial (`readelf`) cost one call. Its 99 calls produced
zero writes of `vm.js` — a different cause the allowlist cannot touch. Maximising coverage
would need a second change (mandatory early write of the required artefact path); this
report does not claim it, because the trace shows an agent that never stopped reading, not
one that tried to write and was prevented.

## 6. What the traces do not say

- Why the model never wrote the required file. Prose per run is 43-121 characters and no
  reasoning text is captured (`reasoning_tokens` counts are populated, content is empty);
  intent claims would be speculation.
- Whether `dna-insert` would have passed with a working `oligotm`: it never emitted a
  primer, so homology/overhang correctness is unproven.
- Whether an unmodified run would have written to `/app/primers.fasta` given a hint: the
  agents worked at the `/app` root (`ls -la /app` shows the scripts beside
  `sequences.fasta`), so a relative write would have landed correctly. Untested.
- Why quoted `grep` alternation returned empty under the exec tool (11 instances, §1.2) —
  measured behaviour only, mechanism in the exec argument path, not in this trace (since
  fixed: see the `> Now:` line in §1.2).
