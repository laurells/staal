"""A local stand-in for the Anthropic Messages API, for tests that drive the Claude
Agent SDK's own agent loop (in the Claude Code CLI it ships) with a scripted model.

    server = MessagesServer([("mcp__staal__refund_order", {...}), ...])
    env = server.env(home)       # for the CLI: this server, a dummy key, its own home

POST /v1/messages is answered (streamed as server-sent events when the request asks):
a tool_use for each call in turn, one per model turn, then a text joining what the
tools said. Every request is recorded in `seen` as (tools shown, tool results so far,
each as (text, is_error)).
"""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class MessagesServer:
    def __init__(self, calls):
        self.calls = list(calls)
        self.seen = []
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
                if not self.path.startswith("/v1/messages") or self.path.startswith("/v1/messages/count_tokens"):
                    self._json({"input_tokens": 1})
                    return
                results = [b for m in body.get("messages", []) if m["role"] == "user" and isinstance(m["content"], list)
                           for b in m["content"] if b.get("type") == "tool_result"]
                outputs = [("".join(c["text"] for c in r["content"] if c["type"] == "text")
                            if isinstance(r["content"], list) else r["content"], bool(r.get("is_error")))
                           for r in results]
                outer.seen.append((body.get("tools", []), outputs))
                step = len(outputs)
                if step < len(outer.calls):
                    name, arguments = outer.calls[step]
                    block = {"type": "tool_use", "id": f"toolu_{step}", "name": name, "input": arguments}
                    stop = "tool_use"
                else:
                    block = {"type": "text", "text": " | ".join(o[0] for o in outputs)}
                    stop = "end_turn"
                message = {"id": f"msg_{step}", "type": "message", "role": "assistant", "model": body.get("model"),
                           "content": [], "stop_reason": None, "stop_sequence": None,
                           "usage": {"input_tokens": 1, "output_tokens": 1}}
                if not body.get("stream"):
                    self._json(dict(message, content=[block], stop_reason=stop))
                    return
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.end_headers()

                def event(kind, data):
                    self.wfile.write(f"event: {kind}\ndata: {json.dumps(data)}\n\n".encode())

                event("message_start", {"type": "message_start", "message": message})
                if block["type"] == "text":
                    event("content_block_start", {"type": "content_block_start", "index": 0,
                                                  "content_block": {"type": "text", "text": ""}})
                    event("content_block_delta", {"type": "content_block_delta", "index": 0,
                                                  "delta": {"type": "text_delta", "text": block["text"]}})
                else:
                    event("content_block_start", {"type": "content_block_start", "index": 0,
                                                  "content_block": dict(block, input={})})
                    event("content_block_delta", {"type": "content_block_delta", "index": 0,
                                                  "delta": {"type": "input_json_delta",
                                                            "partial_json": json.dumps(block["input"])}})
                event("content_block_stop", {"type": "content_block_stop", "index": 0})
                event("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None},
                                        "usage": {"output_tokens": 1}})
                event("message_stop", {"type": "message_stop"})

            def _json(self, value):
                data = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self._server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self._server.server_address[1]}"

    def env(self, home):
        """What the CLI needs to use this server, with a home of its own, so nothing of
        the machine's own Claude Code settings or MCP servers comes in."""
        return {"ANTHROPIC_BASE_URL": self.url, "ANTHROPIC_API_KEY": "sk-test-not-a-key",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "HOME": home}

    def close(self):
        self._server.shutdown()
        self._server.server_close()
