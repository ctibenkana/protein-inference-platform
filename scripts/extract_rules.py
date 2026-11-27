#!/usr/bin/env python3
"""Extract PrometheusRule manifests into plain Prometheus rule files.

promtool can't read the Kubernetes PrometheusRule wrapper, so this writes
each manifest's `spec` (its `groups`) to OUT_DIR/<metadata.name>.yaml for
`promtool check rules` and `promtool test rules`.

Usage: extract_rules.py OUT_DIR MANIFEST...
"""

import pathlib
import sys

import yaml


def main() -> None:
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    out = pathlib.Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for path in sys.argv[2:]:
        for doc in yaml.safe_load_all(pathlib.Path(path).read_text()):
            if not doc or doc.get("kind") != "PrometheusRule":
                continue
            target = out / f"{doc['metadata']['name']}.yaml"
            target.write_text(yaml.safe_dump({"groups": doc["spec"]["groups"]}, sort_keys=False))
            print(f"{path} -> {target}")


if __name__ == "__main__":
    main()
