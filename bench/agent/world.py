# The world both agents act on, on one local port: a model that answers like
# Anthropic's Messages API, a Stripe that deduplicates refunds by Idempotency-Key,
# and a mail service that does not deduplicate. It records every request and every
# effect, and it can kill the agent's process at a chosen request, before or after
# that request takes effect, so both agents crash at exactly the same points.
#
# Both agents get the same answers: the model's plan comes from the scenario, and
# is wrapped as {"value": plan} only for a request whose output schema has that
# wrapper (Staal's), so the plan an agent sees is identical either way.

import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs

STRIPE_KEY = "sk_test_bench_4f3e2d1c"
MAIL_KEY = "mk_test_bench_9a8b7c6d"
MODEL_KEY = "sk-ant-test-bench-0123"

# The shop's payments in Stripe. pi_9001 belongs to another customer.
INTENTS = {
    "pi_4417": {"id": "pi_4417", "currency": "usd", "amount": 12000},
    "pi_9001": {"id": "pi_9001", "currency": "usd", "amount": 90000},
    "pi_2208": {"id": "pi_2208", "currency": "usd", "amount": 2000},
    "pi_eur1": {"id": "pi_eur1", "currency": "eur", "amount": 12000},
}


class World:
    def __init__(self, scenario):
        self.scenario = scenario
        self.lock = threading.Lock()
        self.requests = []          # every request: kind, path, headers, body
        self.refunds = []           # refunds that took effect
        self.emails = []            # emails that took effect
        self.by_key = {}            # Idempotency-Key -> (params, status, body)
        self.answers = list(scenario.get("model", []))
        self.model_answers = 0      # model requests answered with a plan
        self.stripe_faults = list(scenario.get("stripe_faults", []))
        self.mail_faults = list(scenario.get("mail_faults", []))
        self.crashes = [dict(c) for c in scenario.get("crashes", [])]
        self.attempts = {}          # kind -> number of requests seen
        self.worker = None          # the agent's process, set by the harness
        self.killed = 0
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self.server.daemon_threads = True
        self.base = "http://127.0.0.1:%d" % self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()

    # A crash rule {"at": kind, "nth": n, "when": "before"|"after"} fires once, on
    # the nth request of that kind counted over the whole job (restarts included).
    def _crash(self, kind, when):
        n = self.attempts[kind]
        for c in self.crashes:
            if not c.get("fired") and c["at"] == kind and c["nth"] == n and c["when"] == when:
                c["fired"] = True
                w = self.worker
                if w is not None and w.poll() is None:
                    w.kill()
                    w.wait()
                    self.killed += 1
                return True
        return False

    def _handler(world):
        class H(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *a):
                pass

            def _body(self):
                n = int(self.headers.get("Content-Length") or 0)
                return self.rfile.read(n).decode() if n else ""

            def _send(self, status, obj):
                data = (obj if isinstance(obj, str) else json.dumps(obj)).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(data)
                self.close_connection = True

            def _drop(self):
                # No answer at all: the client sees the connection close.
                self.close_connection = True

            def _kind(self, method, path):
                if path == "/v1/messages":
                    return "model"
                if path == "/v1/messages/count_tokens":
                    return "count"
                if method == "GET" and path.startswith("/v1/payment_intents/"):
                    return "intent"
                if method == "POST" and path == "/v1/refunds":
                    return "refund"
                if method == "POST" and path == "/emails":
                    return "mail"
                return "other"

            def _handle(self, method):
                body = self._body()
                kind = self._kind(method, self.path)
                with world.lock:
                    world.attempts[kind] = world.attempts.get(kind, 0) + 1
                    world.requests.append({"kind": kind, "method": method, "path": self.path,
                                           "headers": {k.lower(): v for k, v in self.headers.items()}, "body": body})
                    if world._crash(kind, "before"):
                        return self._drop()
                    status, reply = getattr(self, "_" + kind)(body)
                    if world._crash(kind, "after"):
                        return self._drop()
                if status is None:
                    return self._drop()
                self._send(status, reply)

            def do_GET(self):
                self._handle("GET")

            def do_POST(self):
                self._handle("POST")

            # The model ----------------------------------------------------------
            def _count(self, body):
                return 200, {"input_tokens": 300}

            def _model(self, body):
                if self.headers.get("x-api-key") != MODEL_KEY:
                    return 401, {"type": "error", "error": {"type": "authentication_error", "message": "bad key"}}
                # Past the end of the script, the model gives its last answer again.
                if not world.answers:
                    return 500, {"type": "error", "error": {"type": "api_error", "message": "no scripted answer"}}
                a = world.answers.pop(0) if len(world.answers) > 1 else world.answers[0]
                world.model_answers += "plan" in a or "raw" in a
                if "status" in a:
                    return a["status"], {"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}
                if "raw" in a:
                    text = a["raw"]
                else:
                    try:
                        schema = json.loads(body)["output_config"]["format"]["schema"]
                        wrapped = list(schema.get("properties", {})) == ["value"]
                    except (ValueError, KeyError, TypeError):
                        wrapped = False
                    text = json.dumps({"value": a["plan"]} if wrapped else a["plan"])
                stop = a.get("stop", "end_turn")
                if stop != "end_turn":
                    text = text[: len(text) // 2]
                return 200, {"id": "msg_bench", "type": "message", "role": "assistant", "model": "claude-bench",
                             "content": [{"type": "text", "text": text}], "stop_reason": stop,
                             "usage": {"input_tokens": 300, "output_tokens": 120}}

            # Stripe -------------------------------------------------------------
            def _stripe_auth(self):
                return self.headers.get("Authorization") == "Bearer " + STRIPE_KEY

            def _intent(self, body):
                if not self._stripe_auth():
                    return 401, {"error": {"type": "invalid_request_error", "message": "Invalid API Key"}}
                pid = self.path.rsplit("/", 1)[1]
                if pid not in INTENTS:
                    return 404, {"error": {"type": "invalid_request_error", "code": "resource_missing"}}
                return 200, {"id": pid, "currency": INTENTS[pid]["currency"]}

            def _refund(self, body):
                if not self._stripe_auth():
                    return 401, {"error": {"type": "invalid_request_error", "message": "Invalid API Key"}}
                params = {k: v[0] for k, v in parse_qs(body, keep_blank_values=True).items()}
                key = self.headers.get("Idempotency-Key")
                fault = world.stripe_faults.pop(0) if world.stripe_faults else None
                if fault and fault.get("status"):
                    return fault["status"], {"error": {"type": "api_error", "message": "scripted"}}
                if key and key in world.by_key:
                    seen, status, reply = world.by_key[key]
                    if seen != params:
                        return 400, {"error": {"type": "idempotency_error"}}
                else:
                    status, reply = self._refund_effect(params)
                    if key:
                        world.by_key[key] = (params, status, reply)
                if fault and fault.get("lost"):
                    return None, None
                return status, reply

            def _refund_effect(self, params):
                pid, amount = params.get("payment_intent", ""), params.get("amount", "")
                if pid not in INTENTS:
                    return 404, {"error": {"type": "invalid_request_error", "code": "resource_missing"}}
                if not amount.isdigit() or int(amount) <= 0:
                    return 400, {"error": {"type": "invalid_request_error", "param": "amount"}}
                done = sum(r["amount"] for r in world.refunds if r["payment_intent"] == pid)
                if int(amount) > INTENTS[pid]["amount"] - done:
                    return 400, {"error": {"type": "invalid_request_error", "code": "amount_too_large"}}
                r = {"id": "re_%d" % (len(world.refunds) + 1), "payment_intent": pid, "amount": int(amount), "status": "succeeded"}
                world.refunds.append(r)
                return 200, {"id": r["id"], "status": r["status"]}

            # Mail ---------------------------------------------------------------
            def _mail(self, body):
                if self.headers.get("Authorization") != "Bearer " + MAIL_KEY:
                    return 401, {"message": "bad key"}
                try:
                    m = json.loads(body)
                    to = m["to"]
                except (ValueError, KeyError, TypeError):
                    return 422, {"message": "bad message"}
                fault = world.mail_faults.pop(0) if world.mail_faults else None
                if fault and fault.get("status"):
                    return fault["status"], {"message": "scripted"}
                e = {"id": "em_%d" % (len(world.emails) + 1), "to": to, "subject": m.get("subject"), "text": m.get("text")}
                world.emails.append(e)
                if fault and fault.get("lost"):
                    return None, None
                return 200, {"id": e["id"]}

            def _other(self, body):
                return 404, {"message": "no such endpoint"}

        return H


if __name__ == "__main__":
    w = World({})
    print(w.base)
    time.sleep(3600)
