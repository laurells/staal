"""The framework tutorials in docs/frameworks/, followed as written: every block marked
`file=NAME` is saved into an empty folder, and each Python file that a block marked
`output=NAME` follows is run there; its standard output must be exactly that block.
A program that talks to Claude runs against a local stand-in for the Messages API
(fake_messages.py) whose scripted model asks for the refunds the page describes; its
final line, Claude's own reply, is the stand-in's and is checked only to be there.

Usage: STAAL_LIBRARY=... python3 python/tests/test_tutorials.py <repo root> <scratch dir>
(with the frameworks in python/tests/frameworks.txt installed).
"""

import os
import re
import shutil
import subprocess
import sys

root, scratch = sys.argv[1], sys.argv[2]
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fake_messages import MessagesServer  # noqa: E402

checks = 0


def expect(cond, what):
    global checks
    if not cond:
        raise SystemExit(f"FAILED: {what}")
    checks += 1


FENCE = re.compile(r"^```(\S*)((?: \S+)*)\n(.*?)^```$", re.M | re.S)


def blocks(page):
    """(info words, text) of every fenced block of the page."""
    text = open(page, encoding="utf-8").read()
    return [(m.group(2).split(), m.group(3)) for m in FENCE.finditer(text)]


# What the scripted model asks for, by tutorial: the calls the page's text describes.
REFUND = "mcp__staal__refund_order"
SCRIPTS = {
    "claude-agent-sdk.md": [(REFUND, {"order": "4417", "amount": {"amount": "80.00", "currency": "USD"}}),
                            (REFUND, {"order": "4417", "amount": {"amount": "35.00", "currency": "USD"}})],
}

pages = sorted(f for f in os.listdir(os.path.join(root, "docs", "frameworks")) if f.endswith(".md"))
expect(pages, "docs/frameworks has tutorials")
for page in pages:
    work = os.path.join(scratch, page[:-3])
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    outputs = []
    for words, body in blocks(os.path.join(root, "docs", "frameworks", page)):
        for w in words:
            if w.startswith("file="):
                name = w[len("file="):]
                expect(".." not in name and not name.startswith("/"), f"{page}: a file inside the folder: {name}")
                with open(os.path.join(work, name), "w", encoding="utf-8") as f:
                    f.write(body)
            if w.startswith("output="):
                outputs.append((w[len("output="):], body))
    expect(outputs, f"{page} shows what its programs print")
    for program, want in outputs:
        if not program.endswith(".py"):
            continue
        env = {k: os.environ[k] for k in ("PATH", "LANG", "STAAL_LIBRARY") if k in os.environ}
        env["PYTHONPATH"] = os.path.join(root, "python")
        talks_to_claude = "claude_agent_sdk" in open(os.path.join(work, program), encoding="utf-8").read()
        server = None
        if talks_to_claude:
            server = MessagesServer(SCRIPTS[page])
            home = os.path.join(work, ".home")
            os.makedirs(home, exist_ok=True)
            env.update(server.env(home))
        else:
            env["HOME"] = work
        out = subprocess.run([sys.executable, program], cwd=work, env=env, capture_output=True, text=True, timeout=300)
        if server:
            server.close()
        expect(out.returncode == 0, f"{page}: {program} failed:\n{out.stdout}\n{out.stderr}")
        got = out.stdout
        if talks_to_claude:
            lines = got.splitlines(keepends=True)
            expect(lines and lines[-1].startswith("Claude: "), f"{page}: {program} ends with Claude's reply:\n{got}")
            got = "".join(lines[:-1])
        # `<time>` in a page's output stands for a UTC time the program prints, which changes
        # from run to run; everything else must be exactly as shown.
        pattern = "".join(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z" if part == "<time>" else re.escape(part)
                          for part in re.split(r"(<time>)", want))
        expect(re.fullmatch(pattern, got) is not None, f"{page}: {program} printed\n{got}\nwhere the page shows\n{want}")
        if server:
            # The model was really shown the tool, and asked for each call in the script.
            expect(len(server.seen) == len(SCRIPTS[page]) + 1, f"{page}: {program}: {len(server.seen)} model turns")
        print(f"{page}: {program}:\n{out.stdout}", end="")

print(f"{checks} checks passed")
