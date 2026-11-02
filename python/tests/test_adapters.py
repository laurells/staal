"""The framework adapters, driven by each framework's own agent loop with a scripted
model: the model asks for a refund over the customer's limit and is refused with the
broker's message, then asks within it and the refund is made. The examples in
examples/frameworks/ are imported as written; only their model is replaced.

Usage: STAAL_LIBRARY=/path/to/libstaalc.so python3 python/tests/test_adapters.py <repo root> <scratch dir>
(with openai-agents and langgraph installed: python/tests/frameworks.txt).
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

print(f"{checks} checks passed")
