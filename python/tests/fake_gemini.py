"""A local stand-in for the Gemini API's generateContent, for tests that drive Google
ADK's own agent loop with a scripted model.

    server = GeminiServer([("refund_order", {...}), ...])
    env = server.env()          # GOOGLE_GEMINI_BASE_URL and a dummy GOOGLE_API_KEY

POST /<version>/models/<model>:generateContent is answered with a functionCall for each
call in turn, one per model turn, then a text joining the function responses so far.
Every request is recorded in `seen` as (function declarations shown, responses so far).
Only non-streamed requests are answered.
"""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class GeminiServer:
    def __init__(self, calls):
        self.calls = list(calls)
        self.seen = []
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
                if ":generateContent" not in self.path:
                    self._json({"error": {"code": 404, "message": f"the stand-in does not answer {self.path}"}}, 404)
                    return
                declarations = [d for t in body.get("tools", []) for d in t.get("functionDeclarations", [])]
                responses = [p["functionResponse"]["response"] for c in body.get("contents", [])
                             for p in c.get("parts", []) if "functionResponse" in p]
                outer.seen.append((declarations, responses))
                step = len(responses)
                if step < len(outer.calls):
                    name, arguments = outer.calls[step]
                    part = {"functionCall": {"name": name, "args": arguments, "id": f"call_{step}"}}
                else:
                    part = {"text": " | ".join(r.get("result", r.get("error", json.dumps(r))) for r in responses)}
                self._json({"candidates": [{"content": {"role": "model", "parts": [part]}, "finishReason": "STOP", "index": 0}],
                            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1, "totalTokenCount": 2},
                            "modelVersion": "scripted"})

            def _json(self, value, status=200):
                data = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self._server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self._server.server_address[1]}"

    def env(self):
        return {"GOOGLE_GEMINI_BASE_URL": self.url, "GOOGLE_API_KEY": "test-not-a-key", "GOOGLE_GENAI_USE_VERTEXAI": "false"}

    def close(self):
        self._server.shutdown()
        self._server.server_close()
