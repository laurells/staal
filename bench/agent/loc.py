# Counts lines of code in each agent: lines that are not blank and not only a
# comment. For the Python agent, also by section (the "# Name ---" headings in it).
#
#   python loc.py [--json loc.json]

import argparse
import json
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
FILES = {
    "python": [("app.py", "the agent, its policy, journal, retries and HTTP")],
    "staal": [("agent.staal", "the agent"), ("app.py", "the policy, as grants, and run/resume/resolve"),
              ("stripe.staal", "Stripe refunds library (from examples/capabilities)"),
              ("mail.staal", "mail library")],
}


def code_lines(path):
    comment = "#" if path.endswith(".py") else "//"
    lines, in_doc = [], False
    with open(path) as f:
        for raw in f:
            s = raw.strip()
            if path.endswith(".py") and (s.startswith('"""') or in_doc):
                # A docstring line: count it as documentation, not code.
                quotes = s.count('"""')
                in_doc = in_doc != (quotes % 2 == 1)
                continue
            if s and not s.startswith(comment):
                lines.append(raw.rstrip("\n"))
    return lines


def python_sections(path):
    out, name = {}, "header"
    comment_head = re.compile(r"^# (.+?) -{5,}$")
    with open(path) as f:
        for raw in f:
            m = comment_head.match(raw.strip())
            if m:
                name = m.group(1).rstrip(". ")
                continue
            s = raw.strip()
            if s and not s.startswith("#"):
                out[name] = out.get(name, 0) + 1
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json")
    a = ap.parse_args()
    result = {}
    for impl, files in FILES.items():
        rows = []
        for name, what in files:
            rows.append({"file": name, "what": what, "lines": len(code_lines(os.path.join(HERE, impl, name)))})
        result[impl] = rows
        print("%s: %d lines" % (impl, sum(r["lines"] for r in rows)))
        for r in rows:
            print("  %-14s %4d  %s" % (r["file"], r["lines"], r["what"]))
    result["python_sections"] = python_sections(os.path.join(HERE, "python", "app.py"))
    print("python by section:")
    for k, v in result["python_sections"].items():
        print("  %-40s %4d" % (k, v))
    if a.json:
        with open(a.json, "w") as f:
            json.dump(result, f, indent=1)


if __name__ == "__main__":
    main()
