#!/usr/bin/env python3
"""Repository hygiene: ASCII file names, no local absolute paths, size limit."""

import re
import subprocess
import sys

MAX_LINES = 600
SOURCE_SUFFIXES = (".rs", ".py", ".ps1", ".js", ".css", ".html")
# User-profile paths that identify a developer machine.
LOCAL_PATH = re.compile(r"[A-Za-z]:\\{1,2}Users\\{1,2}[A-Za-z0-9._-]+|/Users/[A-Za-z0-9._-]+/")
EXEMPT = ("app/ui/i18n.js",)


def tracked_files() -> list[str]:
    output = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return [line for line in output.splitlines() if line]


def main() -> int:
    failures = []
    for path in tracked_files():
        if not path.isascii():
            failures.append(f"{path}: non-ASCII file name")
        if not path.endswith(SOURCE_SUFFIXES + (".md", ".toml", ".json", ".yml")):
            continue
        try:
            text = open(path, encoding="utf-8").read()
        except (OSError, UnicodeDecodeError):
            continue
        if LOCAL_PATH.search(text):
            failures.append(f"{path}: contains a local user path")
        if path.endswith(SOURCE_SUFFIXES) and path not in EXEMPT:
            lines = text.count("\n") + 1
            if lines > MAX_LINES:
                failures.append(f"{path}: {lines} lines (limit {MAX_LINES})")
    for failure in failures:
        print(failure)
    if not failures:
        print("Hygiene checks passed.")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
