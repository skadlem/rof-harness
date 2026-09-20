import json, sys

log = sys.argv[1]
req = None
calls = []
for line in open(log):
    try:
        e = json.loads(line)
    except Exception:
        continue
    if e.get("dir") == "req":
        req = e
    elif e.get("dir") == "resp" and req is not None:
        calls.append((req, e))
        req = None

if not calls:
    print("no calls captured")
    sys.exit(0)


def cache_of(u):
    """The cache figure moves between spellings: top-level
    `prompt_cache_hit_tokens`, nested `prompt_tokens_details.cached_tokens`, or
    absent. Prefer the same chain rof's parser uses."""
    c = u.get("prompt_cache_hit_tokens")
    if c is None:
        details = u.get("prompt_tokens_details") or {}
        c = details.get("cached_tokens")
    return c or 0


pin = pout = preason = pcached = 0
print(f"calls: {len(calls)}")
for i, (q, r) in enumerate(calls):
    u = r.get("usage", {}) or {}
    pin += u.get("prompt_tokens", 0) or 0
    pout += u.get("completion_tokens", 0) or 0
    preason += u.get("reasoning_tokens", 0) or 0
    pcached += cache_of(u)
    print(f"  {i}: max_tokens={q.get('max_tokens')} msgs={q.get('n_messages')} "
          f"system={q.get('system_len')} user={q.get('user_len')} "
          f"finish={r.get('finish')} in={u.get('prompt_tokens')} "
          f"out={u.get('completion_tokens')} reason={u.get('reasoning_tokens')} "
          f"cached={cache_of(u)}")

print(f"TOTAL input={pin} output={pout} reasoning={preason} cached={pcached}")
if pin:
    print(f"  cache hit: {100 * pcached / pin:.0f}%")
print(f"  billed (uncached in + out): {pin - pcached + pout}")
