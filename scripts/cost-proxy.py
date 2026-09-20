# Reproduce the like-for-like cost comparison in README.md / STATUS.md:
#   1. `python3 scripts/cost-proxy.py` in one terminal (bills what the
#      endpoint bills; logs to /tmp/cost-wire.jsonl)
#   2. point both harnesses at http:#127.0.0.1:8099/v1
#        rof:     ROF_CHAT_BASE=http:#127.0.0.1:8099/v1
#        hermes:  redirect custom_providers[].base_url in ~/.hermes/config.yaml
#                 and restore it after -- a crash must not leave it dead
#   3. run the same task with both, then `python3 scripts/cost-sum.py
#      /tmp/cost-wire.jsonl`
# Both agents must hit the same endpoint through the same proxy, otherwise
# neither agent's self-reported usage is comparable: hermes's own usage file
# is internally inconsistent (input 945 + output 259 vs total_tokens 80,948).
# The `usage` block on the response is the only shared, billed ground truth.
import http.server, socketserver, json, urllib.request, os

# Measures what the endpoint actually bills: the `usage` block on every
# chat-completions response. Both agents talk to the same endpoint through
# this proxy, so their totals are directly comparable — neither agent's own
# self-report is trusted, because hermes's usage file is internally
# inconsistent (input 945 + output 259 vs total_tokens 80,948).
ATRIA = "https://api.atria-asi.ai/v1/chat/completions"
LOG = os.environ.get("COST_WIRE_LOG", "/tmp/cost-wire.jsonl")

USAGE_KEYS = ("prompt_tokens", "completion_tokens", "reasoning_tokens",
              "prompt_cache_hit_tokens", "cached_tokens", "total_tokens")


class P(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n)
        try:
            body = json.loads(raw)
        except Exception:
            body = {}
        msgs = body.get("messages", [])
        req_summary = {
            "dir": "req",
            "max_tokens": body.get("max_tokens"),
            "n_messages": len(msgs),
            "system_len": len((msgs[0] or {}).get("content", "") or "") if msgs else 0,
            "user_len": len("".join(
                m.get("content", "") or "" for m in msgs[1:])) if len(msgs) > 1 else 0,
            "reasoning": body.get("reasoning"),
            "chat_template_kwargs": body.get("chat_template_kwargs"),
        }
        with open(LOG, "a") as f:
            f.write(json.dumps(req_summary) + "\n")

        req = urllib.request.Request(ATRIA, data=raw, headers={
            "Content-Type": "application/json",
            "Authorization": self.headers.get("Authorization", ""),
        }, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                payload = r.read()
            status = r.status
        except urllib.error.HTTPError as e:
            payload = e.read()
            status = e.code

        usage = {}
        finish = None
        try:
            resp = json.loads(payload)
            # Log the whole usage block verbatim: the cache figure lives in a
            # nested `prompt_tokens_details.cached_tokens` on some responses,
            # and the point is to stop guessing which spelling the endpoint
            # chose.
            usage = resp.get("usage", {}) or {}
            choices = resp.get("choices") or []
            if choices:
                finish = choices[0].get("finish_reason")
        except Exception:
            pass
        with open(LOG, "a") as f:
            f.write(json.dumps({"dir": "resp", "status": status,
                                "finish": finish, "usage": usage}) + "\n")

        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *a):
        pass


socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", 8099), P) as httpd:
    httpd.serve_forever()
