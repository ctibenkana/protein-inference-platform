"""Command-line entry point: FASTA in, embeddings and a run summary out.

Outputs in --output:
  embeddings.npy  float32 (N, dim), rows in input order
  ids.txt         record ids, one per line, same order
  summary.json    run parameters, padding cost and throughput

Privacy: logs contain counts and timings only, never sequences or ids.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import platform
import sys
import time
from pathlib import Path

from . import __version__, batching, fasta, metrics

DEFAULT_MODEL = os.environ.get("ESM_MODEL", "facebook/esm2_t6_8M_UR50D")

log = logging.getLogger("esm_embed")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="esm-embed", description=__doc__.splitlines()[0])
    p.add_argument("--input", required=True, help="FASTA file")
    p.add_argument("--output", required=True, help="output directory")
    p.add_argument("--model", default=DEFAULT_MODEL, help="model id or local path")
    p.add_argument("--strategy", choices=sorted(batching.STRATEGIES), default="token_budget")
    p.add_argument("--batch-size", type=int, default=8, help="for --strategy fixed")
    p.add_argument("--max-tokens", type=int, default=8192, help="for --strategy token_budget")
    p.add_argument("--threads", type=int, default=None, help="torch CPU threads")
    p.add_argument("--metrics-port", type=int, default=0, help="serve Prometheus metrics; 0 = off")
    p.add_argument(
        "--linger",
        type=float,
        default=0,
        help="seconds to keep the metrics endpoint up after finishing, so the final values get scraped",
    )
    p.add_argument(
        "--plan-only",
        action="store_true",
        help="compute batches and padding cost without loading the model",
    )
    return p.parse_args(argv)


def plan(records: list[fasta.Record], args: argparse.Namespace, max_residues: int):
    lengths = [min(len(r), max_residues) for r in records]
    if args.strategy == "fixed":
        return batching.fixed(lengths, args.batch_size)
    return batching.token_budget(lengths, args.max_tokens)


def run(args: argparse.Namespace) -> dict:
    out = Path(args.output)
    out.mkdir(parents=True, exist_ok=True)
    started = time.perf_counter()

    records = fasta.read(args.input)
    log.info("read %d sequences", len(records))

    embedder = None
    load_seconds = 0.0
    max_residues = 1022  # ESM-2's limit; refined from the model config when loaded
    if not args.plan_only:
        t0 = time.perf_counter()
        from .model import Embedder

        embedder = Embedder(args.model, threads=args.threads)
        load_seconds = time.perf_counter() - t0
        max_residues = embedder.max_residues
        log.info("model loaded in %.1fs (dim=%d)", load_seconds, embedder.dim)

    truncated = sum(1 for r in records if len(r) > max_residues)
    if truncated:
        log.warning("%d sequences exceed %d residues and were truncated", truncated, max_residues)

    batches = plan(records, args, max_residues)
    pad_fraction = batching.padding_fraction(batches)
    log.info(
        "strategy=%s batches=%d padding_fraction=%.3f",
        args.strategy, len(batches), pad_fraction,
    )

    s = args.strategy
    metrics.run_info.labels(s, args.model, __version__).set(1)
    compute_seconds = 0.0

    if embedder is not None:
        import numpy as np

        vectors = np.zeros((len(records), embedder.dim), dtype=np.float32)
        done = 0
        for n, b in enumerate(batches, 1):
            seqs = [records[i].sequence[:max_residues] for i in b.indices]
            t0 = time.perf_counter()
            vectors[list(b.indices)] = embedder.embed(seqs)
            dt = time.perf_counter() - t0
            compute_seconds += dt
            done += len(b.indices)

            metrics.batch_seconds.labels(s).observe(dt)
            metrics.sequences.labels(s).inc(len(b.indices))
            metrics.residues.labels(s).inc(sum(b.lengths))
            metrics.tokens_computed.labels(s).inc(b.padded_tokens)
            metrics.pad_tokens.labels(s).inc(b.pad_tokens)
            metrics.residues_per_second.labels(s).set(sum(b.lengths) / dt if dt else 0)
            metrics.progress.labels(s).set(done / len(records))
            if n % 10 == 0 or n == len(batches):
                log.info("batch %d/%d done (%d/%d sequences)", n, len(batches), done, len(records))

        np.save(out / "embeddings.npy", vectors)
        (out / "ids.txt").write_text("".join(f"{r.id}\n" for r in records))

    total_residues = sum(sum(b.lengths) for b in batches)
    summary = {
        "version": __version__,
        "model": args.model,
        "strategy": s,
        "batch_size": args.batch_size if s == "fixed" else None,
        "max_tokens": args.max_tokens if s == "token_budget" else None,
        "plan_only": args.plan_only,
        "sequences": len(records),
        "residues": total_residues,
        "truncated": truncated,
        "batches": len(batches),
        "tokens_computed": sum(b.padded_tokens for b in batches),
        "pad_tokens": sum(b.pad_tokens for b in batches),
        "padding_fraction": round(pad_fraction, 4),
        "model_load_seconds": round(load_seconds, 3),
        "compute_seconds": round(compute_seconds, 3),
        "wall_seconds": round(time.perf_counter() - started, 3),
        "residues_per_second": round(total_residues / compute_seconds, 1) if compute_seconds else None,
        "sequences_per_second": round(len(records) / compute_seconds, 2) if compute_seconds else None,
        "threads": args.threads,
        "python": platform.python_version(),
        "machine": platform.machine(),
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    return summary


def main(argv: list[str] | None = None) -> int:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
        stream=sys.stderr,
    )
    args = parse_args(argv)
    metrics.serve(args.metrics_port)
    try:
        summary = run(args)
    except fasta.FastaError as exc:
        log.error("invalid input: %s", exc)
        return 2
    log.info(
        "finished: %d sequences, padding_fraction=%.3f, residues/s=%s",
        summary["sequences"], summary["padding_fraction"], summary["residues_per_second"],
    )
    # One machine-readable line so results outlive the pod (counts and
    # timings only; the summary contains no sequences or ids).
    log.info("summary %s", json.dumps(summary, separators=(",", ":")))
    if args.linger > 0:
        time.sleep(args.linger)
    return 0


if __name__ == "__main__":
    sys.exit(main())
