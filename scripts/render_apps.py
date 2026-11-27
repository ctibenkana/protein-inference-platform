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

# Argo CD renders charts with the live cluster's API list, and several charts
# only emit ServiceMonitors when the Prometheus Operator API is present.
# Declare the same APIs here so CI renders what the cluster will get.
API_VERSIONS = [
    "monitoring.coreos.com/v1",
    "monitoring.coreos.com/v1/ServiceMonitor",
    "monitoring.coreos.com/v1/PodMonitor",
    "monitoring.coreos.com/v1/PrometheusRule",
]


class UniqueKeyLoader(yaml.SafeLoader):
    """SafeLoader that rejects duplicate mapping keys.

    Plain YAML silently keeps the last duplicate, so a second `controller:`
    block in a values file would quietly discard the first one.
    """

    def construct_mapping(self, node, deep=False):
        seen = set()
        for key_node, _ in node.value:
            key = self.construct_object(key_node, deep=deep)
            if key in seen:
                raise yaml.constructor.ConstructorError(
                    None, None, f"duplicate key {key!r}", key_node.start_mark
                )
            seen.add(key)
        return super().construct_mapping(node, deep=deep)


def check_values_file(path: pathlib.Path) -> None:
    try:
        yaml.load(path.read_text(), Loader=UniqueKeyLoader)
    except yaml.YAMLError as exc:
        sys.exit(f"{path.relative_to(REPO_ROOT)}: {exc}")


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
    for api in API_VERSIONS:
        cmd += ["--api-versions", api]
    for vf in helm.get("valueFiles", []):
        # "$values/<path>" refers to this repository (the `ref: values` source).
        values_path = REPO_ROOT / vf.removeprefix("$values/")
        check_values_file(values_path)
        cmd += ["--values", str(values_path)]

    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"{name}: helm template failed\n{result.stderr}")
    out.write_text(result.stdout)


def copy_manifests(name: str, spec: dict, out: pathlib.Path) -> None:
    path = REPO_ROOT / spec["source"]["path"]
    if (path / "kustomization.yaml").exists():
        # Argo CD builds directories with a kustomization.yaml via Kustomize.
        result = subprocess.run(["kubectl", "kustomize", str(path)], capture_output=True, text=True)
        if result.returncode != 0:
            sys.exit(f"{name}: kustomize build failed\n{result.stderr}")
        out.write_text(result.stdout)
        return
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
