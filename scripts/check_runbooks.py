#!/usr/bin/env python3
"""Check that every alert has a runbook, and every runbook link resolves.

Fails if an alert rule lacks a runbook_url annotation, if the URL doesn't
point into this repository's docs/runbooks/, or if the file doesn't exist.

Usage: check_runbooks.py RULE_FILE...   (plain Prometheus rule files)
"""

import pathlib
import sys

import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
PREFIX = "https://github.com/ctibenkana/protein-inference-platform/blob/main/"


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    problems, alerts = [], 0
    for rule_file in sys.argv[1:]:
        for group in yaml.safe_load(pathlib.Path(rule_file).read_text())["groups"]:
            for rule in group["rules"]:
                if "alert" not in rule:
                    continue
                alerts += 1
                name, url = rule["alert"], rule.get("annotations", {}).get("runbook_url")
                if not url:
                    problems.append(f"{name}: no runbook_url")
                elif not url.startswith(PREFIX + "docs/runbooks/"):
                    problems.append(f"{name}: runbook_url outside docs/runbooks: {url}")
                elif not (REPO_ROOT / url.removeprefix(PREFIX)).is_file():
                    problems.append(f"{name}: runbook missing: {url.removeprefix(PREFIX)}")
    if problems:
        sys.exit("\n".join(problems))
    print(f"{alerts} alerts, every runbook present")


if __name__ == "__main__":
    main()
