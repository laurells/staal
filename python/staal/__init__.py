"""Run Staal programs from Python, through Staal's C interface (libstaalc).

This package holds no Staal logic: it passes strings to the C library and parses
the JSON it returns. Staal itself contains no Python.

    import staal
    lib = staal.Library()   # the library installed with this package; or give a
                            # path, or set STAAL_LIBRARY
    out = lib.run("clinic.staal", {"entry": "book", ...}, journal="run.journal")
    if out["status"] == "done":
        print(out["result"])

Every call returns the JSON object described in include/staal.h, as a dict.

A program's entry function can also be a tool an app (or an agent framework) calls
again and again, with quotas, `once` and crash recovery across calls (Linux and macOS):

    refund = staal.Tool("refund.staal", name="refund_order",
                        description="Refund part of an order, within the shop's limits.",
                        grants=lambda ctx: {"pay": {"cap": "payments.refund", "each": usd(ctx.limit)}},
                        journals="/var/lib/staal/runs")
    refund.schema                    # the JSON Schema of its arguments
    r = refund({"order": "4417", "amount": {"amount": "35.00", "currency": "USD"}}, context=ctx)
    r.status, r.value, r.text        # "done", the result, what to show a model
"""

import ctypes
import json
import os
import sys
from collections.abc import Mapping

__all__ = ["Library", "Host", "Tool", "Result", "StaalError"]


class StaalError(Exception):
    """A tool or host could not be opened, or a call was malformed: nothing ran."""


def bundled_library():
    """The libstaalc installed with this package, or None."""
    name = {"nt": "staalc.dll"}.get(os.name, "libstaalc.dylib" if sys.platform == "darwin" else "libstaalc.so")
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "_bin", name)
    return path if os.path.isfile(path) else None


class Library:
    def __init__(self, path=None):
        path = path or os.environ.get("STAAL_LIBRARY") or bundled_library()
        if not path:
            raise ValueError("give the path to libstaalc, or set STAAL_LIBRARY: this staal package has none of its own")
        self._lib = ctypes.CDLL(path)
        # Returned strings are kept as raw pointers so they can be given back to
        # staal_free; c_char_p would copy them and lose the pointer.
        for name, nargs in [("staal_check", 1), ("staal_run", 4), ("staal_resume", 4), ("staal_resolve", 2)]:
            fn = getattr(self._lib, name)
            fn.argtypes = [ctypes.c_char_p] * nargs
            fn.restype = ctypes.c_void_p
        # Tools exist on Linux and macOS only.
        self.has_tools = hasattr(self._lib, "staal_tool_open")
        if self.has_tools:
            for name, nargs in [("staal_host_open", 1), ("staal_host_close", 1), ("staal_tool_open", 2),
                                ("staal_tool_call", 4), ("staal_tool_close", 1), ("staal_run_status", 3)]:
                fn = getattr(self._lib, name)
                fn.argtypes = [ctypes.c_char_p] * nargs
                fn.restype = ctypes.c_void_p
        # App inputs came in 0.26.0: an older libstaalc runs every other tool.
        self.has_app_inputs = self.has_tools and hasattr(self._lib, "staal_tool_call_with_inputs")
        if self.has_app_inputs:
            self._lib.staal_tool_call_with_inputs.argtypes = [ctypes.c_char_p] * 5
            self._lib.staal_tool_call_with_inputs.restype = ctypes.c_void_p
        self._lib.staal_free.argtypes = [ctypes.c_void_p]
        self._lib.staal_free.restype = None
        self._lib.staal_version.argtypes = []
        self._lib.staal_version.restype = ctypes.c_char_p

    @property
    def version(self):
        return self._lib.staal_version().decode("utf-8")

    def _call(self, name, *args):
        encoded = [None if a is None else str(a).encode("utf-8") for a in args]
        ptr = getattr(self._lib, name)(*encoded)
        if not ptr:
            raise RuntimeError(f"{name} returned NULL")
        try:
            return json.loads(ctypes.string_at(ptr).decode("utf-8"))
        finally:
            self._lib.staal_free(ptr)

    def check(self, program):
        return self._call("staal_check", program)

    def run(self, program, config, journal=None, engine=None):
        return self._call("staal_run", program, json.dumps(config), journal, engine)

    def resume(self, program, config, journal, engine=None):
        return self._call("staal_resume", program, json.dumps(config), journal, engine)

    def resolve(self, journal, **resolution):
        """resolve(journal, performed="<receipt>") | (status=200, body="...") |
        (failed="<reason>") | (retry=True)"""
        return self._call("staal_resolve", journal, json.dumps(resolution))


def _default_library(library):
    lib = library or Library()
    if not lib.has_tools:
        raise StaalError("this libstaalc has no tools: they need Linux or macOS")
    return lib


def _ok(out):
    if out.get("status") == "error":
        raise StaalError(out.get("error", "unknown error"))
    return out


class Host:
    """Where tools keep their runs: one journal directory, its ledger and its quotas.

    Opening it resumes or settles whatever an earlier process left there. One process
    uses a directory at a time; opening the same directory again in this process gives
    the same host (with the same settings).
    """

    def __init__(self, journals, *, keep=90, max_concurrent_runs=8, notify=None, notify_env=None, library=None):
        self._lib = _default_library(library)
        settings = {"journals": os.fspath(journals), "keep": keep, "max_concurrent_runs": max_concurrent_runs}
        if notify is not None:
            settings["notify"] = list(notify)
        if notify_env is not None:
            settings["notify_env"] = list(notify_env)
        self._handle = _ok(self._lib._call("staal_host_open", json.dumps(settings)))["host"]
        self.journals = os.fspath(journals)

    def status(self, run, *, scope=None):
        """What happened to a run, by its id: running, waiting_for_operator, done (with
        its result), failed, not_started, expired or unknown. Changes nothing. A run
        made in a scope is described only when the same scope is given; otherwise it is
        unknown."""
        return _ok(self._lib._call("staal_run_status", self._handle, run, _scope(scope)))

    def close(self):
        if self._handle is not None:
            _ok(self._lib._call("staal_host_close", self._handle))
            self._handle = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


def _scope(scope):
    if scope is not None and not isinstance(scope, str):
        raise TypeError("a scope must be a string (a tenant's or a user's id, from the app's own records)")
    return scope


class Result:
    """The answer to one call.

    status is "done", "failed", "stopped" (waiting for an operator), "in_progress" (an
    operation still running, called again) or "not_run" (refused before any run). text
    is what to show a model; value is the program's result when done; run is the run's
    id, when there is one."""

    def __init__(self, out):
        self.status = out["status"]
        self.is_error = out["is_error"]
        self.text = out["text"]
        self.structured = out.get("structured")
        self.run = out.get("run")
        self.value = self.structured.get("result") if self.status == "done" and self.structured else None

    def __repr__(self):
        return f"Result(status={self.status!r}, run={self.run!r}, text={self.text!r})"


_SPEC_KEYS = ("title", "entry", "quota", "once", "deadline", "max_concurrent_runs", "engine",
              "model", "secrets", "net", "fs", "adapters", "limits")


class Tool:
    """A program's entry function as a tool.

    grants is a dict, the same for every call, or a function of the call's context
    (whatever the app passes as context=) that returns the grants for that call, so the
    app's policy decides each call's limits. scope, if given, is a function of the same
    context returning the call's trusted scope as a string (a tenant's or a user's id,
    from the app's own records, never from the arguments): a `once` key is held within
    its scope, and a run's status is given only in its scope. An app serving more than
    one tenant or user from one journal directory gives a scope. app_inputs, if given,
    maps names of the entry function's inputs to functions of the same context that
    give their values: inputs only the app supplies, such as the customer a call is
    for. They are not in the schema a model sees, and a call whose arguments name one
    is refused. Either journals (a directory) or host is
    required: every call is journaled, and quotas and `once` are kept there. The other
    keyword arguments are those of a tool in an MCP manifest (docs/MCP.md section 2).
    """

    def __init__(self, program, *, name, description, grants=None, scope=None, app_inputs=None, journals=None, host=None,
                 library=None, **spec):
        if scope is not None and not callable(scope):
            raise TypeError("Tool: scope is a function of the call's context")
        self._scope = scope
        if app_inputs is not None and (not isinstance(app_inputs, Mapping) or not app_inputs
                                       or not all(isinstance(k, str) and callable(f) for k, f in app_inputs.items())):
            raise TypeError("Tool: app_inputs maps input names to functions of the call's context")
        self._app_inputs = dict(app_inputs) if app_inputs is not None else None
        unknown = [k for k in spec if k not in _SPEC_KEYS]
        if unknown:
            raise TypeError(f"Tool: unknown argument {unknown[0]!r}")
        if (journals is None) == (host is None):
            raise TypeError("Tool: give journals (a directory) or host, not both and not neither")
        self._own_host = host is None
        self.host = Host(journals, library=library) if host is None else host
        self._lib = self.host._lib
        if self._app_inputs is not None and not self._lib.has_app_inputs:
            if self._own_host:
                self.host.close()
            raise StaalError("this libstaalc has no app inputs: they need libstaalc 0.26.0 or newer")
        self._grants = grants if callable(grants) else None
        body = {"name": name, "description": description, "program": os.fspath(program)}
        if self._app_inputs is not None:
            body["app_inputs"] = list(self._app_inputs)
        if grants is not None and not callable(grants):
            body["grants"] = grants
        body.update({k: v for k, v in spec.items() if v is not None})
        try:
            out = _ok(self._lib._call("staal_tool_open", self.host._handle, json.dumps(body)))
        except Exception:
            if self._own_host:
                self.host.close()
            raise
        self._handle = out["tool"]
        self.name = out["name"]
        self.title = out.get("title")
        self.description = out["description"]
        self.schema = out["input_schema"]
        self.output_schema = out["output_schema"]
        self.annotations = out["annotations"]
        self.warnings = out["warnings"]
        self.per_call_grants = out["grants"] == "per_call"
        if self.per_call_grants and self._grants is None:
            self.close()
            raise TypeError("Tool: the program needs grants; give grants= (a dict or a function of the context)")
        if not self.per_call_grants and self._grants is not None:
            self.close()
            raise TypeError("Tool: the program's entry function takes no capabilities, so it takes no grants")

    def __call__(self, arguments=None, *, context=None, **kwargs):
        """One call: arguments as a dict (or as keyword arguments). Returns a Result;
        a refusal or failure is a Result too, so a model can be told why."""
        if arguments is not None and kwargs:
            raise TypeError("give the arguments as a dict or as keyword arguments, not both")
        if arguments is not None and not isinstance(arguments, Mapping):
            raise TypeError(f"the arguments are a dict (a JSON object), not {type(arguments).__name__}")
        args = dict(arguments if arguments is not None else kwargs)
        grants = None
        if self.per_call_grants:
            grants = self._grants(context)
            if not isinstance(grants, dict):
                raise TypeError("the grants function must return a dict")
            grants = json.dumps(grants)
        scope = _scope(self._scope(context)) if self._scope is not None else None
        if self._app_inputs is None:
            return Result(_ok(self._lib._call("staal_tool_call", self._handle, json.dumps(args), grants, scope)))
        inputs = json.dumps({k: f(context) for k, f in self._app_inputs.items()})
        return Result(_ok(self._lib._call("staal_tool_call_with_inputs", self._handle, json.dumps(args), inputs, grants,
                                          scope)))

    def status(self, run, *, context=None):
        """A run's status, in the scope the tool's scope function gives for context."""
        scope = _scope(self._scope(context)) if self._scope is not None else None
        return self.host.status(run, scope=scope)

    def close(self):
        if self._handle is not None:
            _ok(self._lib._call("staal_tool_close", self._handle))
            self._handle = None
        if self._own_host:
            self.host.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
