"""A local stand-in for the OpenAI Responses API (POST /v1/responses), for tests that
drive the OpenAI Agents SDK's own agent loop with a scripted model.

    server = ResponsesServer([("refund_order", {...}), ...])
    env = server.env()          # OPENAI_BASE_URL and a dummy OPENAI_API_KEY

Each request is answered with a function_call for each call in turn, one per model
turn, then a message joining the tool outputs so far. Every request is recorded in
`seen` as (tools shown, tool outputs so far). Only non-streamed requests are answered.
"""

import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class ResponsesServer:
    def __init__(self, calls):
        self.calls = list(calls)
        self.seen = []
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
                if not self.path.rstrip("/").endswith("/responses") or body.get("stream"):
                    self._json({"error": {"message": f"the stand-in does not answer {self.path}", "type": "invalid_request_error"}}, 404)
                    return
                items = body.get("input") if isinstance(body.get("input"), list) else []
                outputs = [i["output"] for i in items if isinstance(i, dict) and i.get("type") == "function_call_output"]
                outer.seen.append((body.get("tools", []), outputs))
                step = len(outputs)
                if step < len(outer.calls):
                    name, arguments = outer.calls[step]
                    output = [{"type": "function_call", "id": f"fc_{step}", "call_id": f"call_{step}", "name": name,
                               "arguments": json.dumps(arguments), "status": "completed"}]
                else:
                    text = " | ".join(o if isinstance(o, str) else json.dumps(o) for o in outputs)
                    output = [{"type": "message", "id": f"msg_{step}", "role": "assistant", "status": "completed",
                               "content": [{"type": "output_text", "text": text, "annotations": []}]}]
                self._json({
                    "id": f"resp_{step}", "object": "response", "created_at": int(time.time()), "model": body.get("model", "scripted"),
                    "status": "completed", "output": output, "parallel_tool_calls": False, "tool_choice": "auto",
                    "tools": body.get("tools", []), "error": None, "incomplete_details": None, "instructions": None,
                    "metadata": {}, "temperature": None, "top_p": None,
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2,
                              "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}},
                })

            def _json(self, value, status=200):
                data = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self._server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self._server.server_address[1]}/v1"

    def env(self):
        return {"OPENAI_BASE_URL": self.url, "OPENAI_API_KEY": "sk-test-not-a-key"}

    def close(self):
        self._server.shutdown()
        self._server.server_close()
