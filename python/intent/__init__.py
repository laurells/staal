"""Run Intent programs from Python, through Intent's C interface (libintentc).

This package holds no Intent logic: it passes strings to the C library and parses
the JSON it returns. Intent itself contains no Python.

    import intent
    lib = intent.Library("/path/to/libintentc.so")   # or set INTENT_LIBRARY
    out = lib.run("clinic.int", {"entry": "book", ...}, journal="run.journal")
    if out["status"] == "done":
        print(out["result"])

Every call returns the JSON object described in include/intent.h, as a dict.
"""

import ctypes
import json
import os

__all__ = ["Library"]


class Library:
    def __init__(self, path=None):
        path = path or os.environ.get("INTENT_LIBRARY")
        if not path:
            raise ValueError("give the path to libintentc, or set INTENT_LIBRARY")
        self._lib = ctypes.CDLL(path)
        # Returned strings are kept as raw pointers so they can be given back to
        # intent_free; c_char_p would copy them and lose the pointer.
        for name, nargs in [("intent_check", 1), ("intent_run", 4), ("intent_resume", 4), ("intent_resolve", 2)]:
            fn = getattr(self._lib, name)
            fn.argtypes = [ctypes.c_char_p] * nargs
            fn.restype = ctypes.c_void_p
        self._lib.intent_free.argtypes = [ctypes.c_void_p]
        self._lib.intent_free.restype = None
        self._lib.intent_version.argtypes = []
        self._lib.intent_version.restype = ctypes.c_char_p

    @property
    def version(self):
        return self._lib.intent_version().decode("utf-8")

    def _call(self, name, *args):
        encoded = [None if a is None else str(a).encode("utf-8") for a in args]
        ptr = getattr(self._lib, name)(*encoded)
        if not ptr:
            raise RuntimeError(f"{name} returned NULL")
        try:
            return json.loads(ctypes.string_at(ptr).decode("utf-8"))
        finally:
            self._lib.intent_free(ptr)

    def check(self, program):
        return self._call("intent_check", program)

    def run(self, program, config, journal=None, engine=None):
        return self._call("intent_run", program, json.dumps(config), journal, engine)

    def resume(self, program, config, journal, engine=None):
        return self._call("intent_resume", program, json.dumps(config), journal, engine)

    def resolve(self, journal, **resolution):
        """resolve(journal, performed="<receipt>") | (status=200, body="...") |
        (failed="<reason>") | (retry=True)"""
        return self._call("intent_resolve", journal, json.dumps(resolution))
