#!/usr/bin/env python3
"""Generate the README's visuals from committed data, so they cannot drift.

Two subcommands:

  chart  <rows.json> <out.svg>   spend-vs-work scatter, one point per graded run
  crates                          mermaid crate map from the workspace manifests

The rows file is a snapshot of one graded arm (see docs/assets/arm-envelope-rows.json);
it is committed next to the SVG so the picture is reproducible with:

    python3 tools/readme_chart.py chart docs/assets/arm-envelope-rows.json \
                                     docs/assets/arm-envelope.svg

No third-party imports: the SVG is composed by hand so the file stays diffable.
"""
import json
import pathlib
import sys

# --- chart -----------------------------------------------------------------

W, H = 880, 400
M_L, M_R, M_T, M_B = 58, 18, 86, 78
GAP = 46
INK, MUTED, GRID = "#1a202c", "#64748b", "#dfe4ea"
SERIES = {  # arm -> legend label
    "C": "control · 300k gross · 60 steps",
    "S": "shape · 300k miss+out · 60 steps",
    "J": "joint · 300k miss+out · 120 steps",
}
COLOUR = {"C": "#3f5b78", "S": "#a86b12", "J": "#96242a"}
CELLS = {"mcmc-sampling-stan": "mcmc", "video-processing": "video"}


def esc(v):
    return str(v).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def chart(rows, out):
    rows = [r for r in rows if r.get("tin") and r.get("steps") is not None]
    pw = (W - M_L - M_R - GAP) / 2
    ph = H - M_T - M_B
    x_max = 1500.0

    def sx(tok, x0):
        return x0 + min(tok / 1000.0, x_max) / x_max * pw

    s = ["<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 %d %d\" width=\"%d\" height=\"%d\" "
         "role=\"img\" aria-label=\"Spend against work for 18 graded runs: more budget buys steps, "
         "not passing tests\">" % (W, H, W, H)]
    css = (
        ".bg{fill:#fff;stroke:" + GRID + "}"
        ".ttl{font:600 15px/1.25 ui-sans-serif,system-ui,-apple-system,'Segoe UI',sans-serif;fill:" + INK + "}"
        ".sub{font:400 11.5px/1.3 ui-sans-serif,system-ui,-apple-system,'Segoe UI',sans-serif;fill:" + MUTED + "}"
        ".lb{font:600 12px/1.2 ui-sans-serif,system-ui,-apple-system,sans-serif;fill:" + INK + "}"
        ".ax{font:500 10.5px/1.2 ui-monospace,SFMono-Regular,Menlo,monospace;fill:" + MUTED + "}"
        ".an{font:600 10px/1.2 ui-monospace,SFMono-Regular,Menlo,monospace;fill:" + INK + "}"
        ".g{stroke:" + GRID + ";stroke-width:1}"
        "@media (prefers-color-scheme:dark){"
        ".bg{fill:#0d1117;stroke:#30363d}"
        ".ttl{fill:#e6edf3}.sub{fill:#9198a1}.lb{fill:#e6edf3}.ax{fill:#9198a1}.an{fill:#e6edf3}.g{stroke:#30363d}"
        "}"
    )
    s.append("<style>" + css + "</style>")
    s.append('<rect class="bg" x="0.5" y="0.5" width="%d" height="%d" rx="10"/>' % (W - 1, H - 1))
    s.append('<text class="ttl" x="%d" y="26">Same cap, more budget: steps climb, passing tests do not</text>' % M_L)
    s.append('<text class="sub" x="%d" y="44">18 graded runs of one arm family, two reps per cell · '
             'deepseek-v4.1-flash, in-container, verdict from the task-written reward file</text>' % M_L)
    s.append('<text class="sub" x="%d" y="60">filled marker = rep 1, hollow = rep 2 · '
             'source: docs/assets/arm-envelope-rows.json</text>' % M_L)

    panels = [
        ("steps taken before the run stops", 0.0, 120.0, 30.0, "steps", 0),
        ("verifier tests passed", 0.0, 6.0, 1.0, "passed", 1),
    ]
    for title, lo, hi, tick, field, idx in panels:
        x0 = M_L + idx * (pw + GAP)
        y0 = M_T
        s.append('<text class="lb" x="%.1f" y="%.1f">%s</text>' % (x0, y0 - 9, esc(title)))
        for t in range(int(lo), int(hi) + 1, int(tick)):
            y = y0 + ph - (t - lo) / (hi - lo) * ph
            s.append('<line class="g" x1="%.1f" y1="%.1f" x2="%.1f" y2="%.1f" opacity="%s"/>'
                     % (x0, y, x0 + pw, y, "0.95" if t == lo else "0.5"))
            s.append('<text class="ax" x="%.1f" y="%.1f" text-anchor="end">%d</text>' % (x0 - 8, y + 3.5, t))
        # x ticks (shared scale, both panels)
        for k in range(0, int(x_max) + 1, 250):
            x = x0 + k / x_max * pw
            s.append('<line class="g" x1="%.1f" y1="%.1f" x2="%.1f" y2="%.1f" opacity="0.5"/>' % (x, y0, x, y0 + ph))
            s.append('<text class="ax" x="%.1f" y="%.1f" text-anchor="middle">%d</text>' % (x, y0 + ph + 14, k))
        s.append('<text class="ax" x="%.1f" y="%.1f" text-anchor="middle">thousand input tokens</text>'
                 % (x0 + pw / 2, y0 + ph + 30))
        for r in rows:
            if r["task"] not in CELLS:
                continue
            v = r.get(field)
            if v is None:
                continue
            cx = sx(r["tin"], x0)
            cy = y0 + ph - (v - lo) / (hi - lo) * ph
            col = COLOUR[r["arm"]]
            fill = "none" if r["rep"] == "r2" else col
            shape = (f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="5" fill="{fill}" stroke="{col}" stroke-width="1.8"/>'
                     if CELLS[r["task"]] == "mcmc" else
                     f'<rect x="{cx-4.6:.1f}" y="{cy-4.6:.1f}" width="9.2" height="9.2" rx="1.6" fill="{fill}" '
                     f'stroke="{col}" stroke-width="1.8"/>')
            s.append(shape[:-2] + f'><title>{esc(r["task"])} {esc(r["rep"])} · arm {esc(r["arm"])} · '
                                  f'{r["tin"]//1000}k tokens · {field} {v}</title></{shape[-6:-1]}>'
                     if False else shape)
        if idx == 0:
            # label the two extremes only: the control's spend and the joint arm's.
            for arm, dx, dy, anchor in (("J", 12, -9, "start"),):
                for r in rows:
                    if r["task"] != "mcmc-sampling-stan" or r["arm"] != arm or r["rep"] != "r1":
                        continue
                    cx, cy = sx(r["tin"], x0), y0 + ph - r["steps"] / 120.0 * ph
                    s.append('<text class="an" x="%.1f" y="%.1f" text-anchor="%s">'
                             '%s arm · %dk tok · %d steps</text>'
                             % (cx + dx, cy + dy, anchor, arm, r["tin"] // 1000, r["steps"]))

    # legend: colour = arm, shape = cell
    lx, ly = M_L, H - 30
    for arm, label in SERIES.items():
        s.append('<circle cx="%.1f" cy="%.1f" r="5" fill="%s"/>' % (lx + 5, ly - 4, COLOUR[arm]))
        s.append('<text class="ax" x="%.1f" y="%.1f">%s</text>' % (lx + 16, ly, esc(label)))
        lx += 20 + 6.1 * len(label) + 14
    ly2 = H - 13
    s.append('<circle cx="%.1f" cy="%.1f" r="5" fill="none" stroke="#64748b" stroke-width="1.8"/>' % (M_L + 5, ly2 - 4))
    s.append('<text class="ax" x="%.1f" y="%.1f">circle = mcmc-sampling-stan (6 tests)</text>' % (M_L + 16, ly2))
    s.append('<rect x="%.1f" y="%.1f" width="9.2" height="9.2" rx="1.6" fill="none" stroke="#64748b" stroke-width="1.8"/>'
             % (M_L + 336, ly2 - 8.6))
    s.append('<text class="ax" x="%.1f" y="%.1f">square = video-processing (5 tests)</text>' % (M_L + 352, ly2))
    s.append("</svg>")
    pathlib.Path(out).write_text("\n".join(s) + "\n")
    print(f"wrote {out} ({len(rows)} rows)")


# --- crate map -------------------------------------------------------------
#
# A crate-to-crate graph of this workspace is a hairball (27 edges, most of them
# crossing), so the README shows LAYERS instead: one node per band, arrows only
# between bands. That statement is only honest if it is checked, so this script
# derives every crate-to-crate edge from the manifests and fails loudly if an
# edge points backwards through the layering or a crate is unbanded.

BANDS = [
    ("primitives", ["agent-event", "agent-log", "agent-budget", "tool-core", "provider-core", "context", "snapshot"], "vocabulary · durable log · caps · tool and provider traits · prompt fold · git overlay"),
    ("evidence", ["trace", "bets"], "ordered sinks · commit proofs and ablation counters"),
    ("loop", ["agent-loop"], "the loop: request fold, tool dispatch, verdict, termination"),
    ("implementations", ["tools-std", "provider-openai"], "six tools behind the policy · chat-completions adapter, retry ladder, pricing"),
    ("assembly", ["eval", "rof"], "slices, gates, reports · the CLI driver"),
]


def _deps_of(manifest, names):
    head, _, tail = manifest.read_text().partition("[dependencies]")
    body = tail.split("\n[")[0]
    return {n for n in names if n != manifest.parent.name and _uses(body, n)}


def crates():
    root = pathlib.Path(__file__).resolve().parent.parent
    manifests = {p.parent.name: p for p in root.glob("crates/*/Cargo.toml")}
    names = set(manifests)
    band_of = {}
    for i, (_, members, _) in enumerate(BANDS):
        for m in members:
            if m not in names:
                sys.exit(f"band lists unknown crate: {m}")
            band_of[m] = i
    missing = names - set(band_of)
    if missing:
        sys.exit(f"unbanded crates (add them to BANDS): {sorted(missing)}")
    deps = {n: _deps_of(manifests[n], names) for n in names}
    for n, ds in deps.items():
        for d in ds:
            if band_of[d] > band_of[n]:
                sys.exit(f"edge {n} -> {d} points backwards through the bands")
    loc = {n: sum(len(f.read_text().splitlines()) for f in manifests[n].parent.rglob("*.rs")) for n in names}

    used = set()
    print("```mermaid")
    print("flowchart BT")
    for i, (band, members, role) in enumerate(BANDS):
        members = [m for m in members if m in names]
        lines = " · ".join(f"{m} {loc[m] / 1000:.1f}k" for m in members)
        print(f'  B{i}["<b>{band}</b><br/>{lines}<br/><i>{role}</i>"]')
        used |= set(members)
    for i in range(len(BANDS)):
        for j in range(i + 1, len(BANDS)):
            if any(band_of[d] == i for n in used if band_of[n] == j for d in deps[n]):
                print(f"  B{j} -.-> B{i}")
    print("```")


def _uses(body, dep):
    for line in body.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key = line.split("=")[0].strip().strip('"')
        if key.split(".")[0] == dep or line.split("=")[0].strip() == dep:
            return True
        if f'{dep}' in line and "workspace" in line:
            return True
    return False


if __name__ == "__main__":
    if len(sys.argv) >= 2 and sys.argv[1] == "chart":
        doc = json.load(open(sys.argv[2]))
        chart(doc["rows"] if isinstance(doc, dict) else doc, sys.argv[3])
    elif len(sys.argv) >= 2 and sys.argv[1] == "crates":
        crates()
    else:
        sys.exit(__doc__)
