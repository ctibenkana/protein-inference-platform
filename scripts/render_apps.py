#!/usr/bin/env python3
"""Render every Helm-based Argo CD Application in deploy/apps.

For each Application with a chart source, run `helm template` with the
pinned chart version and the repository's values files, exactly as Argo CD
would, and write the result to OUT_DIR/<app>.yaml. Plain-manifest
applications are copied as-is. Exits non-zero on the first failure.

Usage: render_apps.py OUT_DIR
"""

import pathlib
import subprocess
import sys

import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
APPS_DIR = REPO_ROOT / "deploy" / "apps"


def render_chart(name: str, spec: dict, out: pathlib.Path) -> None:
    sources = spec.get("sources") or [spec["source"]]
    chart_src = next(s for s in sources if "chart" in s)
    helm = chart_src.get("helm", {})
    namespace = spec["destination"].get("namespace", "default")

    cmd = [
        "helm", "template", helm.get("releaseName", name), chart_src["chart"],
        "--repo", chart_src["repoURL"],
        "--version", chart_src["targetRevision"],
        "--namespace", namespace,
    ]
    for vf in helm.get("valueFiles", []):
        # "$values/<path>" refers to this repository (the `ref: values` source).
        cmd += ["--values", str(REPO_ROOT / vf.removeprefix("$values/"))]

    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"{name}: helm template failed\n{result.stderr}")
    out.write_text(result.stdout)


def copy_manifests(name: str, spec: dict, out: pathlib.Path) -> None:
    path = REPO_ROOT / spec["source"]["path"]
    docs = [p.read_text() for p in sorted(path.glob("*.yaml"))]
    out.write_text("\n---\n".join(docs))


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out_dir = pathlib.Path(sys.argv[1])
    out_dir.mkdir(parents=True, exist_ok=True)

    for app_file in sorted(APPS_DIR.glob("*.yaml")):
        app = yaml.safe_load(app_file.read_text())
        name, spec = app["metadata"]["name"], app["spec"]
        out = out_dir / f"{name}.yaml"
        sources = spec.get("sources") or [spec["source"]]
        if any("chart" in s for s in sources):
            render_chart(name, spec, out)
        else:
            copy_manifests(name, spec, out)
        kinds = out.read_text().count("\nkind: ") + out.read_text().startswith("kind: ")
        print(f"{name}: rendered {kinds} objects")


if __name__ == "__main__":
    main()
