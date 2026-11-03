"""The framework adapters, driven by each framework's own agent loop with a scripted
model: the model asks for a refund over the customer's limit and is refused with the
broker's message, then asks within it and the refund is made. The examples in
examples/frameworks/ are imported as written; only their model is replaced.

Usage: STAAL_LIBRARY=/path/to/libstaalc.so python3 python/tests/test_adapters.py <repo root> <scratch dir>
(with the frameworks in python/tests/frameworks.txt installed).
"""

import asyncio
import importlib.util
import json
import os
import shutil
import sys

root, scratch = sys.argv[1], sys.argv[2]
sys.path.insert(0, os.path.join(root, "python"))
os.environ["STAAL_JOURNALS"] = os.path.join(scratch, "runs")
shutil.rmtree(os.environ["STAAL_JOURNALS"], ignore_errors=True)

checks = 0


def expect(cond, what):
    global checks
    if not cond:
        raise SystemExit(f"FAILED: {what}")
    checks += 1


def example(name):
    path = os.path.join(root, "examples", "frameworks", name + ".py")
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


REFUSED = "'payments.refund' allows at most 50.00 USD per action; this action needs 80.00 USD"
# A result is given to the model as its JSON, so a text result keeps its quotes.
DONE = '"Refunded 35.00 USD for order 4417"'
ASK = [{"order": "4417", "amount": {"amount": "80.00", "currency": "USD"}},
       {"order": "4417", "amount": {"amount": "35.00", "currency": "USD"}}]


# ---------------------------------------------------------------------------
# OpenAI Agents SDK

from agents import ModelResponse, Runner, Usage, set_tracing_disabled  # noqa: E402
from agents.models.interface import Model  # noqa: E402
from openai.types.responses import ResponseFunctionToolCall, ResponseOutputMessage, ResponseOutputText  # noqa: E402


class ScriptedModel(Model):
    """Asks for each refund in ASK in turn, then answers with what the tool said."""

    def __init__(self):
        self.seen_tools = None
        self.outputs = []

    async def get_response(self, system_instructions, input, model_settings, tools, output_schema, handoffs, tracing, **kw):
        self.seen_tools = tools
        items = input if isinstance(input, list) else []
        self.outputs = [i["output"] for i in items if isinstance(i, dict) and i.get("type") == "function_call_output"]
        step = len(self.outputs)
        if step < len(ASK):
            call = ResponseFunctionToolCall(type="function_call", call_id=f"call_{step}", name="refund_order",
                                            arguments=json.dumps(ASK[step]))
            return ModelResponse(output=[call], usage=Usage(), response_id=None)
        text = ResponseOutputText(type="output_text", text=" | ".join(self.outputs), annotations=[])
        msg = ResponseOutputMessage(id="msg_1", type="message", role="assistant", status="completed", content=[text])
        return ModelResponse(output=[msg], usage=Usage(), response_id=None)

    def stream_response(self, *a, **kw):
        raise NotImplementedError("the test does not stream")


set_tracing_disabled(True)  # nothing leaves the machine in a test
oa = example("openai_agents_refund")
model = ScriptedModel()
result = asyncio.run(Runner.run(oa.agent(model), "Order 4417 arrived broken. Please refund it.", context={"tier": "standard"}))
tool = model.seen_tools[0]
expect(tool.name == "refund_order" and "order" in tool.params_json_schema["properties"], "the model sees the tool and its schema")
expect("pay" not in tool.params_json_schema["properties"], "a grant is never an argument")
expect(model.outputs == [REFUSED, DONE], f"refused with the broker's message, then done: {model.outputs}")
expect(result.final_output == f"{REFUSED} | {DONE}", f"the final answer: {result.final_output}")
oa.refund.close()

# ---------------------------------------------------------------------------
# LangGraph

from langchain_core.language_models.chat_models import BaseChatModel  # noqa: E402
from langchain_core.messages import AIMessage, ToolMessage  # noqa: E402
from langchain_core.outputs import ChatGeneration, ChatResult  # noqa: E402


class ScriptedChat(BaseChatModel):
    """Asks for each refund in ASK in turn, then answers with what the tool said."""

    bound: list = []

    @property
    def _llm_type(self):
        return "scripted"

    def bind_tools(self, tools, **kwargs):
        self.bound = list(tools)
        return self

    def _generate(self, messages, stop=None, run_manager=None, **kwargs):
        outputs = [m.content for m in messages if isinstance(m, ToolMessage)]
        step = len(outputs)
        if step < len(ASK):
            msg = AIMessage(content="", tool_calls=[{"name": "refund_order", "args": ASK[step], "id": f"call_{step}"}])
        else:
            msg = AIMessage(content=" | ".join(outputs))
        return ChatResult(generations=[ChatGeneration(message=msg)])


lg = example("langgraph_refund")
chat = ScriptedChat()
out = lg.graph(chat).invoke({"messages": [("user", "Order 4417 arrived broken. Please refund it.")]},
                            config={"configurable": {"staal_context": {"tier": "standard"}}})
tool_outputs = [m.content for m in out["messages"] if isinstance(m, ToolMessage)]
expect(tool_outputs == [REFUSED, DONE], f"refused with the broker's message, then done: {tool_outputs}")
expect(out["messages"][-1].content == f"{REFUSED} | {DONE}", f"the final answer: {out['messages'][-1].content}")
expect(lg.tools[0].name == "refund_order", "the tool's name")
# The async path, as an async graph runs it.
out = asyncio.run(lg.graph(ScriptedChat()).ainvoke({"messages": [("user", "Refund it.")]},
                                                   config={"configurable": {"staal_context": {"tier": "gold"}}}))
tool_outputs = [m.content for m in out["messages"] if isinstance(m, ToolMessage)]
expect(tool_outputs == ['"Refunded 80.00 USD for order 4417"', DONE],
       f"a gold customer's limit lets both through: {tool_outputs}")
lg.refund.close()

# ---------------------------------------------------------------------------
# Claude Agent SDK: the SDK's own agent loop, in the Claude Code CLI it bundles, against
# a local server answering as the Anthropic Messages API does, with a scripted model.

import threading  # noqa: E402
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer  # noqa: E402

from claude_agent_sdk import AssistantMessage, ToolUseBlock  # noqa: E402


class ScriptedMessages(BaseHTTPRequestHandler):
    """POST /v1/messages, streamed as server-sent events: a tool_use for each refund in
    ASK in turn, then a text joining what the tool said. Records what the model was shown."""

    seen = []  # one entry per request: (tools, tool results)

    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        if not self.path.startswith("/v1/messages") or self.path.startswith("/v1/messages/count_tokens"):
            self._json({"input_tokens": 1})
            return
        results = [b for m in body.get("messages", []) if m["role"] == "user" and isinstance(m["content"], list)
                   for b in m["content"] if b.get("type") == "tool_result"]
        outputs = [("".join(c["text"] for c in r["content"] if c["type"] == "text") if isinstance(r["content"], list)
                    else r["content"], bool(r.get("is_error"))) for r in results]
        ScriptedMessages.seen.append((body.get("tools", []), outputs))
        step = len(outputs)
        if step < len(ASK):
            block = {"type": "tool_use", "id": f"toolu_{step}", "name": "mcp__staal__refund_order", "input": ASK[step]}
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
            event("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
            event("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": block["text"]}})
        else:
            start = dict(block, input={})
            event("content_block_start", {"type": "content_block_start", "index": 0, "content_block": start})
            event("content_block_delta", {"type": "content_block_delta", "index": 0,
                                          "delta": {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}})
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


server = ThreadingHTTPServer(("127.0.0.1", 0), ScriptedMessages)
threading.Thread(target=server.serve_forever, daemon=True).start()
home = os.path.join(scratch, "claude-home")
os.makedirs(home, exist_ok=True)
# The CLI is pointed at the local server and given a home of its own, so nothing of
# the machine's own Claude Code settings or MCP servers comes in.
cli_env = {"ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_address[1]}", "ANTHROPIC_API_KEY": "sk-test-not-a-key",
           "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "HOME": home}

ca = example("claude_agent_sdk_refund")


async def run_session(customer):
    ScriptedMessages.seen = []
    used = []
    answer = None
    async for message in ca.query(prompt="Order 4417 arrived broken. Please refund it.",
                                  options=ca.options(customer, env=cli_env, cwd=home, max_turns=6)):
        if isinstance(message, AssistantMessage):
            used += [b.name for b in message.content if isinstance(b, ToolUseBlock)]
        if isinstance(message, ca.ResultMessage):
            answer = message.result
    return used, answer


used, answer = asyncio.run(run_session({"tier": "standard"}))
tools, outputs = ScriptedMessages.seen[-1]
# The CLI may add its own placeholders (newer ones add one for tool search); what
# matters is the refund is there and no built-in tool is.
names = [t["name"] for t in tools]
expect("mcp__staal__refund_order" in names and not {"Bash", "Read", "Write", "Edit", "WebFetch"} & set(names),
       f"the model is shown the refund and no built-in tool: {names}")
shown = next(t for t in tools if t["name"] == "mcp__staal__refund_order")
expect("pay" not in shown["input_schema"]["properties"], "a grant is never an argument")
expect(shown["description"] == ca.refund.description, "the tool's description")
expect(used == ["mcp__staal__refund_order"] * 2, f"the model called the tool twice: {used}")
expect(outputs == [(REFUSED, True), (DONE, False)], f"refused as an error with the broker's message, then done: {outputs}")
expect(answer == f"{REFUSED} | {DONE}", f"the final answer: {answer}")
# A gold customer's session, through a server made for that customer.
used, answer = asyncio.run(run_session({"tier": "gold"}))
expect(ScriptedMessages.seen[-1][1] == [('"Refunded 80.00 USD for order 4417"', False), (DONE, False)],
       f"a gold customer's limit lets both through: {ScriptedMessages.seen[-1][1]}")
from staal.claude_agent_sdk import allowed_tools, mcp_server  # noqa: E402
for bad, why in [([], "a list of staal.Tool"), ([ca.refund, ca.refund], "two tools are named"), (["x"], "a list of staal.Tool")]:
    try:
        mcp_server(bad)
        expect(False, why)
    except TypeError as e:
        expect(why in str(e), str(e))
expect(allowed_tools("billing", [ca.refund]) == ["mcp__billing__refund_order"], "allowed_tools")
ca.refund.close()
server.shutdown()

# ---------------------------------------------------------------------------
# Google ADK: the Runner's own agent loop, with a scripted model.

from google.adk.models.base_llm import BaseLlm  # noqa: E402
from google.adk.models.llm_response import LlmResponse  # noqa: E402
from google.genai import types as genai  # noqa: E402


class ScriptedGemini(BaseLlm):
    """Asks for each refund in ASK in turn, then answers with what the tool said, and
    records what ADK showed it."""

    model: str = "scripted"
    seen: dict = {}

    async def generate_content_async(self, llm_request, stream=False):
        self.seen["declarations"] = [f for t in (llm_request.config.tools or []) for f in (t.function_declarations or [])]
        responses = [p.function_response.response for c in llm_request.contents for p in (c.parts or []) if p.function_response]
        self.seen["responses"] = responses
        step = len(responses)
        if step < len(ASK):
            part = genai.Part(function_call=genai.FunctionCall(name="refund_order", args=ASK[step], id=f"call_{step}"))
        else:
            part = genai.Part(text=" | ".join(r.get("result", r.get("error", "")) for r in responses))
        yield LlmResponse(content=genai.Content(role="model", parts=[part]))


adk = example("google_adk_refund")
model = ScriptedGemini()
answer = asyncio.run(adk.support(model, "Order 4417 arrived broken. Please refund it.", {"tier": "standard"}))
decl = model.seen["declarations"]
expect([d.name for d in decl] == ["refund_order"], f"the model is shown the tool: {[d.name for d in decl]}")
expect("pay" not in decl[0].parameters_json_schema["properties"] and "$schema" not in decl[0].parameters_json_schema,
       "a grant is never an argument, and the schema is JSON Schema without $schema")
expect(model.seen["responses"] == [{"error": REFUSED}, {"result": DONE}], f"refused as an error, then done: {model.seen['responses']}")
expect(answer == f"{REFUSED} | {DONE}", f"the final answer: {answer}")
model = ScriptedGemini()
asyncio.run(adk.support(model, "Refund it.", {"tier": "gold"}))
expect(model.seen["responses"] == [{"result": '"Refunded 80.00 USD for order 4417"'}, {"result": DONE}],
       f"a gold customer's session state lets both through: {model.seen['responses']}")
model = ScriptedGemini()
asyncio.run(adk.support(model, "Refund it.", None))
expect("error" in model.seen["responses"][0], f"no context in the session: an error, never a refund: {model.seen['responses']}")
from staal.google_adk import adk_tool  # noqa: E402
for bad in ("x", None):
    try:
        adk_tool(bad)
        expect(False, "adk_tool takes a staal.Tool")
    except TypeError as e:
        expect("staal.Tool" in str(e), str(e))
adk.refund.close()

print(f"{checks} checks passed")
