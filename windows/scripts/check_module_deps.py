#!/usr/bin/env python3
"""Fail if a workspace crate depends on a local crate it must not use."""

import json
import subprocess
import sys

ALLOWED = {
    "askkey-protocol": set(),
    "askkey-system": set(),
    "askkey-vault": {"askkey-system"},
    "askkey-broker": {"askkey-protocol", "askkey-system"},
    "askkey-helper": {"askkey-protocol", "askkey-system"},
    "askkey-app": {"askkey-protocol", "askkey-system", "askkey-vault", "askkey-broker"},
}


def main() -> int:
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    local = {package["name"] for package in metadata["packages"]}
    failures = []
    for package in metadata["packages"]:
        name = package["name"]
        if name not in ALLOWED:
            failures.append(f"{name}: not listed in scripts/check_module_deps.py")
            continue
        for dependency in package["dependencies"]:
            if dependency["name"] in local and dependency["kind"] in (None, "build") \
                    and dependency["name"] not in ALLOWED[name]:
                failures.append(f"{name} must not depend on {dependency['name']}")
    for failure in failures:
        print(failure)
    if not failures:
        print(f"Module dependencies passed for {len(local)} crates.")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
