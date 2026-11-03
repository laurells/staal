"""Stand-ins for the bank and the SMS service, for the demo and its tests.

Each keeps its own ledger of what it did, which is what the attacks are judged by:
not what Staal reports, but what the service says happened.

    bank = Service("refunds")          # POST /refunds  -> {"id": "re_1"}
    sms = Service("messages")          # POST /messages -> {"id": "sm_1"}
    bank.url, bank.ledger, bank.close()

Both deduplicate by Idempotency-Key, as payment and messaging APIs do: a request
sent again with a key already seen is answered with the first answer and does
nothing. Ways to misbehave, set before or between requests:

    dedup=False     forget keys: a request sent again is done again
    delay=s         wait s seconds after doing the work, before answering
    hold=True       after doing the work, wait until release() before answering;
                    `done` is set when the work is done, so a test can kill its
                    client at exactly that moment
    drop=n          for the next n requests, do the work, then close the
                    connection without answering: the answer is lost
"""

import json
import threading
from decimal import Decimal
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Service:
    def __init__(self, path, *, dedup=True, delay=0.0, hold=False, drop=0):
        self.path = "/" + path
        self.prefix = {"refunds": "re", "messages": "sm"}.get(path, "op")
        self.dedup, self.delay, self.hold, self.drop = dedup, delay, hold, drop
        self.ledger = []
        self.requests = 0  # every request received, a repeated one included
        self.keys = {}
        self.done = threading.Event()
        self._release = threading.Event()
        self._lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
                if self.path != outer.path:
                    return self._send(404, {"error": f"no {self.path}"})
                key = self.headers.get("Idempotency-Key")
                with outer._lock:
                    outer.requests += 1
                    if outer.dedup and key and key in outer.keys:
                        answer = outer.keys[key]
                    else:
                        answer = {"id": f"{outer.prefix}_{len(outer.ledger) + 1}"}
                        outer.ledger.append({"id": answer["id"], "key": key, **body})
                        if key:
                            outer.keys[key] = answer
                    drop = outer.drop > 0
                    if drop:
                        outer.drop -= 1
                outer.done.set()
                if outer.hold:
                    outer._release.wait(60)
                if outer.delay:
                    threading.Event().wait(outer.delay)
                if drop:
                    # The work is done and the answer is lost: the client sees the
                    # connection close.
                    self.close_connection = True
                    self.connection.shutdown(2)
                    return
                self._send(200, answer)

            def _send(self, status, value):
                data = json.dumps(value).encode()
                try:
                    self.send_response(status)
                    self.send_header("content-type", "application/json")
                    self.send_header("content-length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass  # the client is gone (a test killed it); the work stands

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = True
        threading.Thread(target=self._server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self._server.server_address[1]}"

    def release(self):
        self._release.set()

    def total_cents(self):
        """What the bank refunded in all, in cents."""
        return sum(int(Decimal(e["amount"]["amount"]) * 100) for e in self.ledger)

    def close(self):
        self._release.set()
        self._server.shutdown()
        self._server.server_close()
