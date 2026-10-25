"""The `staalc` command of the staal package: the staalc installed with it."""

import os
import subprocess
import sys


def executable():
    name = "staalc.exe" if os.name == "nt" else "staalc"
    return os.path.join(os.path.dirname(os.path.abspath(__file__)), "_bin", name)


def main():
    exe = executable()
    if not os.path.isfile(exe):
        sys.stderr.write(f"staalc: this staal package has no staalc executable ({exe} is missing)\n")
        sys.exit(2)
    args = [exe] + sys.argv[1:]
    if os.name == "nt":
        sys.exit(subprocess.call(args))
    # The process becomes staalc: its input, output and exit code are staalc's own.
    os.execv(exe, args)
