"""Run Staal programs from Python, through Staal's C interface (libstaalc).

This package holds no Staal logic: it passes strings to the C library and parses
the JSON it returns. Staal itself contains no Python.

    import staal
    lib = staal.Library("/path/to/libstaalc.so")   # or set STAAL_LIBRARY
    out = lib.run("clinic.staal", {"entry": "book", ...}, journal="run.journal")
    if out["status"] == "done":
        print(out["result"])

Every call returns the JSON object described in include/staal.h, as a dict.
"""

import ctypes
import json
import os

__all__ = ["Library"]


class Library:
    def __init__(self, path=None):
        path = path or os.environ.get("STAAL_LIBRARY")
        if not path:
            raise ValueError("give the path to libstaalc, or set STAAL_LIBRARY")
        self._lib = ctypes.CDLL(path)
        # Returned strings are kept as raw pointers so they can be given back to
        # staal_free; c_char_p would copy them and lose the pointer.
        for name, nargs in [("staal_check", 1), ("staal_run", 4), ("staal_resume", 4), ("staal_resolve", 2)]:
            fn = getattr(self._lib, name)
            fn.argtypes = [ctypes.c_char_p] * nargs
            fn.restype = ctypes.c_void_p
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
