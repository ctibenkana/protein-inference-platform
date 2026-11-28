#!/usr/bin/env python3
"""Draw p99 latency against offered load for fifo and length-aware batching.

Reads the E1 load-test results and writes an SVG (no plotting library
needed), so the figure in the write-up always matches the data.

    python3 scripts/plot_batching_latency.py
"""

import json
import math
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
RESULTS = ROOT / "services/gateway/bench/results"
OUT = ROOT / "docs/writeups/img/p99-vs-load.svg"

SERIES = [
    ("fifo", "e1-fifo.jsonl", "#c2410c", "FIFO batching"),
    ("aware", "e1-length-aware.jsonl", "#1d4ed8", "Length-aware batching"),
]

W, H = 720, 420
L, R, T, B = 70, 30, 50, 60  # margins
X0, X1 = 0, 32  # requests per second
Y0, Y1 = 0.1, 40  # seconds, log scale


def x(v):
    return L + (v - X0) / (X1 - X0) * (W - L - R)


def y(v):
    v = min(max(v, Y0), Y1)
    return T + (math.log10(Y1) - math.log10(v)) / (math.log10(Y1) - math.log10(Y0)) * (H - T - B)


def load(name):
    return [json.loads(line) for line in (RESULTS / name).read_text().splitlines()]


parts = [
    f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" font-family="system-ui, -apple-system, Segoe UI, sans-serif" font-size="13">',
    "<title>p99 latency against offered load, FIFO vs length-aware batching</title>",
    f'<rect width="{W}" height="{H}" fill="#ffffff"/>',
    f'<text x="{L}" y="28" font-size="15" font-weight="600" fill="#111827">p99 latency vs. offered load (log scale)</text>',
]

# grid and axes
for v in (0.1, 0.3, 1, 3, 10, 30):
    parts.append(f'<line x1="{L}" x2="{W - R}" y1="{y(v):.1f}" y2="{y(v):.1f}" stroke="#e5e7eb"/>')
    parts.append(f'<text x="{L - 8}" y="{y(v) + 4:.1f}" text-anchor="end" fill="#4b5563">{v:g} s</text>')
for v in range(0, 33, 5):
    parts.append(f'<line x1="{x(v):.1f}" x2="{x(v):.1f}" y1="{T}" y2="{H - B}" stroke="#f3f4f6"/>')
    parts.append(f'<text x="{x(v):.1f}" y="{H - B + 20}" text-anchor="middle" fill="#4b5563">{v}</text>')
parts.append(f'<text x="{(L + W - R) / 2}" y="{H - 15}" text-anchor="middle" fill="#374151">offered load (requests per second, one protein each)</text>')
parts.append(f'<line x1="{L}" x2="{W - R}" y1="{H - B}" y2="{H - B}" stroke="#9ca3af"/>')
parts.append(f'<line x1="{L}" x2="{L}" y1="{T}" y2="{H - B}" stroke="#9ca3af"/>')

# the 1 s objective and the 30 s deadline
parts.append(f'<line x1="{L}" x2="{W - R}" y1="{y(1):.1f}" y2="{y(1):.1f}" stroke="#16a34a" stroke-dasharray="6 4" stroke-width="1.5"/>')
parts.append(f'<text x="{W - R - 4}" y="{y(1) - 6:.1f}" text-anchor="end" fill="#15803d">latency objective: 1 s</text>')
parts.append(f'<line x1="{L}" x2="{W - R}" y1="{y(30):.1f}" y2="{y(30):.1f}" stroke="#9ca3af" stroke-dasharray="2 4"/>')
parts.append(f'<text x="{W - R - 4}" y="{y(30) - 6:.1f}" text-anchor="end" fill="#6b7280">request deadline: 30 s</text>')

legend_y = y(0.27)  # bottom left, below both lines
for key, name, color, label in SERIES:
    rows = load(name)
    pts = [(r["offered_rps"], r["p99_s"]) for r in rows if r["p99_s"] is not None]
    path = " ".join(f'{"M" if i == 0 else "L"}{x(a):.1f},{y(b):.1f}' for i, (a, b) in enumerate(pts))
    parts.append(f'<path d="{path}" fill="none" stroke="{color}" stroke-width="2.5"/>')
    for r in rows:
        if r["p99_s"] is None:
            continue
        failed = (r["timed_out"] + r["overloaded"] + r["other_errors"]) / max(r["sent"], 1)
        cx, cy = x(r["offered_rps"]), y(r["p99_s"])
        # hollow markers where requests also failed (timed out or refused)
        fill = "#ffffff" if failed > 0.01 else color
        parts.append(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="5" fill="{fill}" stroke="{color}" stroke-width="2"/>')
        if key == "fifo" and failed > 0.01:
            parts.append(f'<text x="{cx:.1f}" y="{cy + 20:.1f}" text-anchor="middle" font-size="11" fill="{color}">{failed:.0%} failed</text>')
    parts.append(f'<line x1="{L + 16}" x2="{L + 40}" y1="{legend_y}" y2="{legend_y}" stroke="{color}" stroke-width="2.5"/>')
    parts.append(f'<text x="{L + 48}" y="{legend_y + 4}" fill="#111827">{label}</text>')
    legend_y += 20
parts.append(f'<text x="{L + 16}" y="{legend_y + 4}" font-size="11" fill="#6b7280">hollow marker: some requests also timed out or were refused</text>')

parts.append("</svg>")
OUT.parent.mkdir(parents=True, exist_ok=True)
OUT.write_text("\n".join(parts) + "\n")
print(f"wrote {OUT.relative_to(ROOT)}")
