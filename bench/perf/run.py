"""Staal's speed beside Rust, Go, Python and TypeScript, on the same workloads.

Each workload is written once per language, the way that language is usually written
(see README.md for each choice). Every program is built, then run once with each size
to check that all of them give the same answer; only then is each timed. A timing is
one whole process, from start to exit: starting the runtime, reading and checking the
program where the language does that at run time, and the work. Each is run several
times; the median is reported, with the fastest and slowest, and the most memory the
process held (its peak resident set).

Usage:
    python3 run.py [--runs N] [--only fib,loop,...] [--staalc PATH]

Writes results/results.json and results/RESULTS.md. Needs: a release staalc (built
with native code), rustc, go, python3, node and tsc; bun if present.
"""

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent

# Each workload and its size. Sizes are chosen so the slowest Staal engine takes a few
# seconds at most; every language gets the same size.
WORKLOADS = {
    "startup": None,
    "fib": 32,
    "loop": 10_000_000,
    "money": 1_000_000,
    "lists": 10_000,
    "maps": 20_000,
    "text": 50_000,
}

# How the cost of building a list and a text grows with its size.
SCALING = {"lists": [2_500, 5_000, 10_000, 20_000], "text": [12_500, 25_000, 50_000, 100_000]}


PEAK = None  # the launcher, built by build()


def run(cmd):
    """Runs cmd to completion through the launcher (peak.rs): its stdout, wall time in
    seconds, and its peak resident set in KiB."""
    cmd = [str(c) for c in cmd]
    with tempfile.NamedTemporaryFile() as report:
        p = subprocess.run([str(PEAK), report.name, *cmd], capture_output=True)
        if p.returncode != 0:
            raise RuntimeError(f"{' '.join(cmd)} exited {p.returncode}: {p.stderr.decode()[-2000:]}")
        code, nanos, rss = Path(report.name).read_text().split()
        return p.stdout.decode().strip(), int(nanos) / 1e9, int(rss)


def version(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True).stdout.strip().splitlines()[0]
    except (OSError, IndexError):
        return None


def build(staalc, out):
    """Builds every program; gives the runners, as name -> function(workload, n) -> cmd."""
    global PEAK
    out.mkdir(parents=True, exist_ok=True)
    PEAK = out / "peak"
    subprocess.run(["rustc", "-C", "opt-level=3", "--edition", "2024", "-o", PEAK, HERE / "peak.rs"], check=True)
    names = list(WORKLOADS)
    for w in names:
        subprocess.run(["rustc", "-C", "opt-level=3", "-C", "overflow-checks=on", "-C", "codegen-units=1", "--edition", "2024",
                        "-o", out / f"rust-{w}", HERE / "rust" / f"{w}.rs"], check=True)
        subprocess.run(["go", "build", "-o", out / f"go-{w}", HERE / "go" / f"{w}.go"], check=True,
                       env={**os.environ, "GOFLAGS": "-trimpath", "GO111MODULE": "off"})
        subprocess.run([staalc, "build", HERE / "staal" / f"{w}.staal", "-o", out / f"staal-{w}"], check=True, capture_output=True)
    subprocess.run(["tsc", "-p", HERE / "ts", "--outDir", out / "ts"], check=True)

    configs = out / "configs"
    configs.mkdir(exist_ok=True)

    def config(w, n):
        # Staal's inputs come in the run config; max_steps raised, which bounds a run's
        # length and is a safeguard, not part of the work.
        f = configs / f"{w}-{n}.json"
        inputs = {} if n is None else {"n": n}
        f.write_text(json.dumps({"entry": "main", "inputs": inputs, "limits": {"max_steps": 10**15}}))
        return f

    arg = lambda n: [] if n is None else [str(n)]
    runners = {
        "Staal interpreter": lambda w, n: [staalc, "run", HERE / "staal" / f"{w}.staal", "--config", config(w, n), "--engine", "interpreter"],
        "Staal native": lambda w, n: [staalc, "run", HERE / "staal" / f"{w}.staal", "--config", config(w, n), "--engine", "native"],
        "Staal executable": lambda w, n: [out / f"staal-{w}", "run", "--config", config(w, n)],
        "Rust": lambda w, n: [out / f"rust-{w}", *arg(n)],
        "Go": lambda w, n: [out / f"go-{w}", *arg(n)],
        "Python": lambda w, n: [sys.executable, HERE / "python" / f"{w}.py", *arg(n)],
        "TypeScript (Node.js)": lambda w, n: ["node", out / "ts" / f"{w}.js", *arg(n)],
    }
    if shutil.which("bun"):
        runners["TypeScript (Bun)"] = lambda w, n: ["bun", HERE / "ts" / f"{w}.ts", *arg(n)]
    return runners


def answer(out):
    # Staal prints a Text result as JSON; the others print the same quoted text.
    return out.strip()


def measure(runners, w, n, runs):
    # First, the same answer from every language: a workload that differs anywhere is
    # not timed.
    answers = {name: answer(run(cmd(w, n))[0]) for name, cmd in runners.items()}
    if len(set(answers.values())) != 1:
        raise SystemExit(f"{w} n={n}: the programs disagree: {json.dumps(answers, indent=2)}")
    row = {"workload": w, "n": n, "answer": next(iter(answers.values())), "results": {}}
    for name, cmd in runners.items():
        times, peaks = [], []
        for _ in range(runs):
            out, t, rss = run(cmd(w, n))
            if answer(out) != row["answer"]:
                raise SystemExit(f"{w} n={n}: {name} gave {out!r}, then {row['answer']!r}")
            times.append(t)
            peaks.append(rss)
        row["results"][name] = {
            "median_s": statistics.median(times), "min_s": min(times), "max_s": max(times),
            "peak_rss_kib": max(peaks), "runs": runs,
        }
        print(f"  {w:8} n={n!s:>9} {name:22} {statistics.median(times) * 1000:10.1f} ms  {max(peaks) / 1024:7.1f} MiB", flush=True)
    return row


def machine():
    cpu = next((l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")), platform.processor())
    mem = next((int(l.split()[1]) // 1024 for l in open("/proc/meminfo") if l.startswith("MemTotal")), None)
    return {"cpu": cpu, "cores": os.cpu_count(), "memory_mib": mem, "kernel": platform.release(), "os": platform.platform()}


def ms(s):
    return f"{s * 1000:.1f}" if s < 10 else f"{s:.2f} s"


def markdown(data):
    names = list(data["rows"][0]["results"])
    lines = ["# Results", "", f"Measured {data['date']} on {data['machine']['cpu']}, {data['machine']['cores']} cores, "
             f"{data['machine']['memory_mib']} MiB, Linux {data['machine']['kernel']}. Median of {data['runs']} runs, in "
             "milliseconds, of the whole process; in brackets, times the Rust time. A workload's size is n.", "",
             "| Workload | n | " + " | ".join(names) + " |", "| --- | ---: | " + " | ".join("---:" for _ in names) + " |"]
    for r in data["rows"]:
        base = r["results"]["Rust"]["median_s"]
        cells = [f"{ms(v['median_s'])} ({v['median_s'] / base:.1f}×)" for v in r["results"].values()]
        lines.append(f"| {r['workload']} | {r['n'] if r['n'] is not None else ''} | " + " | ".join(cells) + " |")
    lines += ["", f"Peak memory (MiB, the most any run held; a process that does nothing shows {data['floor_rss_kib'] / 1024:.1f}, "
              "the launcher's floor):", "", "| Workload | " + " | ".join(names) + " |",
              "| --- | " + " | ".join("---:" for _ in names) + " |"]
    for r in data["rows"]:
        lines.append(f"| {r['workload']} | " + " | ".join(f"{v['peak_rss_kib'] / 1024:.1f}" for v in r["results"].values()) + " |")
    if data.get("scaling"):
        lines += ["", "How building a list and a text grows with n (median, milliseconds):", "",
                  "| Workload | n | " + " | ".join(names) + " |", "| --- | ---: | " + " | ".join("---:" for _ in names) + " |"]
        for r in data["scaling"]:
            lines.append(f"| {r['workload']} | {r['n']} | " + " | ".join(ms(v["median_s"]) for v in r["results"].values()) + " |")
    lines += ["", "Versions:", ""] + [f"- {k}: {v}" for k, v in data["versions"].items()] + [""]
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--only", default=",".join(WORKLOADS))
    ap.add_argument("--staalc", default=str(REPO / "target" / "release" / "staalc"))
    ap.add_argument("--no-scaling", action="store_true")
    a = ap.parse_args()
    staalc = Path(a.staalc).resolve()
    with tempfile.TemporaryDirectory(prefix="staal-perf-") as tmp:
        runners = build(staalc, Path(tmp))
        data = {
            "date": time.strftime("%Y-%m-%d"), "runs": a.runs, "machine": machine(),
            "versions": {
                "staalc": version([staalc, "--version"]), "rustc": version(["rustc", "--version"]),
                "go": version(["go", "version"]), "python": sys.version.split()[0],
                "node": version(["node", "--version"]), "tsc": version(["tsc", "--version"]),
                **({"bun": version(["bun", "--version"])} if shutil.which("bun") else {}),
            },
            "rows": [], "scaling": [],
        }
        # The smallest peak this launcher can report: a program that does nothing.
        data["floor_rss_kib"] = run(["/bin/true"])[2]
        for w in a.only.split(","):
            data["rows"].append(measure(runners, w, WORKLOADS[w], a.runs))
        if not a.no_scaling:
            for w, sizes in SCALING.items():
                for n in sizes:
                    data["scaling"].append(measure(runners, w, n, max(1, a.runs // 2 + 1)))
    results = HERE / "results"
    results.mkdir(exist_ok=True)
    (results / "results.json").write_text(json.dumps(data, indent=2) + "\n")
    (results / "RESULTS.md").write_text(markdown(data))
    print(f"wrote {results / 'RESULTS.md'}")


if __name__ == "__main__":
    main()
