#!/usr/bin/env python3
"""Writes the agent.json fixtures of tests/agent_check_test.rs (AGENT_MANIFEST.md
section 16, R8) into tests/fixtures/agent/manifests/: one file per case, each the valid
manifest of tests/fixtures/agent/valid/refunds/ with one change, and expected.json,
which says for each whether docs/schemas/agent.schema.json accepts it and whether
`staalc agent check` does (and if not, a part of its message).

    python3 scripts/agent_fixtures.py

Some cases are accepted by the schema and refused by staalc: those are the value rules
a schema cannot state (AGENT_MANIFEST.md section 4, "Value rules"). No case is refused
by the schema and accepted by staalc.
"""
import copy
import json
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VALID = json.load(open(os.path.join(ROOT, "tests/fixtures/agent/valid/refunds/agent.json")))
OUT = os.path.join(ROOT, "tests/fixtures/agent/manifests")


def tool(m):
    return m["tools"][0]


def pay(m):
    return tool(m)["grants"]["pay"]


def post(m):
    return pay(m)["needs"]["post"]


def usd(a, c="USD"):
    return {"amount": a, "currency": c}


def setter(path, value):
    """A change that sets the value at `path` (a list of keys and indexes)."""
    def f(m):
        x = m
        for k in path[:-1]:
            x = x[k]
        x[path[-1]] = value
    return f


def both(a, b):
    def f(m):
        a(m)
        b(m)
    return f


def remover(path):
    def f(m):
        x = m
        for k in path[:-1]:
            x = x[k]
        del x[path[-1]]
    return f


T = ["tools", 0]
G = T + ["grants", "pay"]
N = G + ["needs", "post"]
Q = T + ["quota", "suggested"]

# name: (change, schema accepts, staalc's message or None if staalc accepts)
CASES = {
    "valid": (lambda m: None, True, None),
    "valid-empty-suggested": (setter(G + ["suggested"], {}), True, None),
    "valid-no-suggested": (remover(G + ["suggested"]), True, None),
    "valid-yen-quota": (setter(Q + ["money"], usd("1000", "JPY")), True, None),
    "valid-leading-zero-money": (setter(G + ["suggested", "each"], usd("0100.00")), True, None),
    "valid-one-decimal": (setter(G + ["suggested", "each"], usd("100.5")), True, None),
    "valid-license-expression": (setter(["license"], "Apache-2.0 OR MIT"), True, None),
    "valid-range-wildcard": (setter(["requires_staalc"], ">=0.27"), True, None),
    "valid-reach-port": (setter(N + ["reach"], ["https://{{BANK}}:8443/refunds"]), True, None),
    # Unknown keys, at every depth.
    "unknown-key-top": (setter(["homepage"], "https://example.com"), False, "unknown key 'homepage'"),
    "unknown-key-publisher": (setter(["publisher", "email"], "a@example.com"), False, "unknown key 'email'"),
    "unknown-key-tool": (setter(T + ["title"], "Refund"), False, "unknown key 'title'"),
    "unknown-key-grant": (setter(G + ["limits"], {}), False, "unknown key 'limits'"),
    "unknown-key-suggested": (setter(G + ["suggested", "per_day"], 1), False, "unknown key 'per_day'"),
    "unknown-key-need": (setter(N + ["wait_ms"], 100), False, "unknown key 'wait_ms'"),
    "unknown-key-money": (setter(G + ["suggested", "each", "minor"], 1), False, "unknown key 'minor'"),
    "unknown-key-once": (setter(T + ["once", "scope"], "x"), False, "unknown key 'scope'"),
    "unknown-key-quota": (setter(T + ["quota", "window"], "24h"), False, "unknown key 'window'"),
    "unknown-key-quota-suggested": (setter(Q + ["per_scope"], True), False, "unknown key 'per_scope'"),
    "unknown-key-data": (setter(["data", "audits"], []), False, "unknown key 'audits'"),
    # Versions.
    "manifest-unknown": (setter(["manifest"], "0.2"), False, "is not one this staalc knows"),
    "version-short": (setter(["version"], "0.1"), False, "is not a version"),
    "version-leading-zero": (setter(["version"], "01.0.0"), False, "leading zero"),
    "version-build": (setter(["version"], "0.1.0+build.1"), False, "build metadata"),
    "version-overflow": (setter(["version"], "0.1.99999999999999999999"), True, "too large"),
    "requires-bad-grammar": (setter(["requires_staalc"], "~>0.27"), True, "requires_staalc"),
    "requires-excludes-this": (setter(["requires_staalc"], ">=99.0.0"), True, "does not include this staalc"),
    # Money.
    "money-three-decimals": (setter(G + ["suggested", "each"], usd("100.001")), False, "at most 2 decimal place"),
    "money-yen-decimals": (setter(Q + ["money"], usd("1000.5", "JPY")), False, "at most 0 decimal place"),
    "money-unknown-currency": (setter(G + ["suggested", "each"], usd("1.00", "XYZ")), False, "unsupported currency"),
    "money-lowercase-currency": (setter(G + ["suggested", "each"], usd("1.00", "usd")), False, "unsupported currency"),
    "money-number": (setter(G + ["suggested", "each"], {"amount": 100, "currency": "USD"}), False, "expected a decimal string"),
    "money-sign": (setter(G + ["suggested", "each"], usd("+1.00")), False, "invalid USD amount"),
    "money-exponent": (setter(G + ["suggested", "each"], usd("1e2")), False, "invalid USD amount"),
    "money-space": (setter(G + ["suggested", "each"], usd(" 1.00")), False, "invalid USD amount"),
    "money-trailing-point": (setter(G + ["suggested", "each"], usd("1.")), False, "invalid USD amount"),
    "money-overflow": (setter(G + ["suggested", "each"], usd("99999999999999999999")), True, "too large"),
    # Durations, per field.
    "deadline-zero": (setter(T + ["deadline"], "0s"), False, "expected a whole number and a unit"),
    "deadline-days": (setter(T + ["deadline"], "1d"), False, "expected a whole number and a unit"),
    "deadline-overflow": (setter(T + ["deadline"], "99999999999999999999h"), True, "deadline"),
    "keep-seconds": (setter(T + ["once", "keep"], "30s"), False, "expected a whole number and a unit"),
    "window-seconds": (setter(Q + ["window"], "60s"), False, "expected a whole number and a unit"),
    "window-too-long": (setter(Q + ["window"], "31d"), True, "quota.suggested.window: at most 30d"),
    # Identity and text.
    "agent-uppercase": (setter(["agent"], "Support-refunds"), False, "agent"),
    "agent-double-hyphen": (setter(["agent"], "support--refunds"), False, "agent"),
    "publisher-not-github": (setter(["publisher", "id"], "gitlab:example"), False, "github:"),
    "publisher-hyphen-start": (setter(["publisher", "id"], "github:-example"), False, "github:"),
    "contact-http": (setter(["publisher", "security_contact"], "http://example.com/security"), False, "https URL"),
    "contact-credentials": (setter(["publisher", "security_contact"], "https://user:pw@example.com/security"), True, "no user name or password"),
    "title-control": (setter(["title"], "Support\nrefunds"), False, "control character"),
    "title-long": (setter(["title"], "x" * 81), False, "1 to 80 characters"),
    "license-case": (setter(["license"], "apache-2.0"), True, "not an SPDX license identifier"),
    "license-ref": (setter(["license"], "LicenseRef-mine"), True, "names a license only the publisher has"),
    "license-deprecated": (setter(["license"], "GPL-2.0"), True, "not an SPDX license identifier"),
    "license-operator-case": (setter(["license"], "MIT or Apache-2.0"), True, "license"),
    "category-unknown": (setter(["categories"], ["travel"]), False, "'travel' is not one of"),
    "category-four": (setter(["categories"], ["finance", "sales", "legal", "data"]), False, "1 to 3 items"),
    "category-twice": (setter(["categories"], ["finance", "finance"]), False, "listed twice"),
    "data-missing": (remover(["data", "regions"]), False, "data.regions: missing"),
    "tool-name-twice": (lambda m: m["tools"].append(copy.deepcopy(m["tools"][0])), True, "names another tool too"),
    "program-outside": (setter(T + ["program"], "../refund.staal"), False, "must be the name of a .staal file"),
    "program-missing": (setter(T + ["program"], "missing.staal"), True, "missing.staal"),
    "entry-missing": (setter(T + ["entry"], "nope"), True, "has no function 'nope'"),
    "quota-empty": (setter(Q, {"window": "24h"}), False, "give \"calls\", \"money\" or both"),
    # App inputs and once.
    "app-inputs-no-requirements": (remover(T + ["app_input_requirements"]), False, "app_input_requirements: missing"),
    "app-input-not-a-parameter": (both(setter(T + ["app_inputs"], ["nobody"]), setter(T + ["app_input_requirements"], {"nobody": "x"})), True, "nobody"),
    "requirement-without-input": (setter(T + ["app_input_requirements", "other"], "x"), True, "is not one of app_inputs"),
    "once-by-unknown": (setter(T + ["once", "by"], ["nothing"]), True, "nothing"),
    # Grants against the program.
    "grant-missing": (setter(T + ["grants"], {}), True, "grants.pay: missing"),
    "grant-extra": (setter(T + ["grants", "other"], {}), True, "has no capability parameter 'other'"),
    "need-missing": (remover(G + ["needs"]), True, "needs.post: missing"),
    "need-extra": (setter(G + ["needs", "key"], {}), True, "has no capability need 'key'"),
    "reach-missing": (remover(N + ["reach"]), True, "reach: missing"),
    "reach-placeholder-part-of-host": (setter(N + ["reach"], ["https://{{BANK}}.example.com/refunds"]), True, "a placeholder must be the whole host"),
    "reach-placeholder-lowercase": (setter(N + ["reach"], ["https://{{bank}}/refunds"]), True, "a placeholder's name"),
    "reach-placeholder-in-path": (setter(N + ["reach"], ["https://bank.example.com/{{PATH}}"]), True, "a placeholder must be the whole host"),
    "reach-star-in-host": (setter(N + ["reach"], ["https://*.example.com/refunds"]), True, "the `*` is in the host"),
    "reach-not-canonical": (setter(N + ["reach"], ["https://Bank.example.com/refunds"]), True, "not in canonical form"),
    "reach-http": (setter(N + ["reach"], ["http://bank.example.com/refunds"]), True, "post.reach[0]: 'http://bank.example.com/refunds' uses http://"),
    "reach-on-library": (setter(G + ["reach"], ["https://bank.example.com/"]), True, "a library capability's targets"),
    "only-on-runtime": (setter(N + ["suggested"], {"only": ["https://bank.example.com/refunds"]}), True, "are its \"reach\""),
    "money-on-runtime": (setter(N + ["suggested"], {"max": usd("1.00")}), True, "max and each do not apply"),
    "suggestion-over-ceiling": (setter(G + ["suggested", "max"], usd("600.00")), True, "more than the program's ceiling"),
    "suggestion-other-currency": (setter(G + ["suggested", "max"], usd("100.00", "EUR")), True, "suggested in EUR"),
    "suggestion-currencies-differ": (setter(G + ["suggested"], {"max": usd("100.00"), "each": usd("10.00", "EUR")}), True, "different currencies"),
    "idempotency-on-library": (setter(G + ["idempotency_key"], True), True, "idempotency_key: only for"),
    "uses-zero": (setter(G + ["suggested", "uses"], 0), False, "at least 1"),
}


def main():
    os.makedirs(OUT, exist_ok=True)
    for f in os.listdir(OUT):
        os.remove(os.path.join(OUT, f))
    expected = {}
    for name, (change, schema, staalc) in CASES.items():
        m = copy.deepcopy(VALID)
        change(m)
        with open(os.path.join(OUT, f"{name}.json"), "w") as f:
            json.dump(m, f, indent=2, ensure_ascii=False)
            f.write("\n")
        expected[name] = {"schema": schema, "staalc": staalc}
    with open(os.path.join(OUT, "expected.json"), "w") as f:
        json.dump({"generated_by": "scripts/agent_fixtures.py", "cases": expected}, f, indent=1, ensure_ascii=False, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
