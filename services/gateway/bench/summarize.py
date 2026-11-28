"""Print Markdown tables from loadgen results (bench/results/*.jsonl).

    python3 bench/summarize.py bench/results/e1-fifo.jsonl bench/results/e1-length-aware.jsonl
"""

import json
import sys


def fmt_s(v):
    return "-" if v is None else f"{v:.2f} s"


def pct(v):
    return "-" if v is None else f"{v * 100:.1f}%"


print("| Run | Offered req/s | Served req/s | Refused | Timed out | p50 | p90 | p99 | Padding | Mean batch |")
print("|---|---|---|---|---|---|---|---|---|---|")
for path in sys.argv[1:]:
    for line in open(path):
        r = json.loads(line)
        sent = r["sent"] or 1
        print(
            f"| {r['label']} | {r['offered_rps']:g} | {r['ok_rps']:.1f} "
            f"| {r['overloaded'] / sent:.0%} | {(r['timed_out'] + r['other_errors']) / sent:.0%} "
            f"| {fmt_s(r['p50_s'])} | {fmt_s(r['p90_s'])} | {fmt_s(r['p99_s'])} "
            f"| {pct(r['padding'])} | {'-' if r['mean_batch_sequences'] is None else round(r['mean_batch_sequences'], 2)} |"
        )
