"""Model server: embeds exactly the batches it is given, over HTTP.

The gateway decides which sequences are batched together; this process only
runs the model. Batches run one at a time, because a CPU replica gains
nothing from running two at once.

  POST /v1/batch   {"sequences": ["MKT...", ...]}
                   -> {"model", "dim", "count",
                       "embeddings": base64 of float32 little-endian, row-major}
  GET  /healthz    the process is up
  GET  /readyz     the model is loaded and requests are accepted

Prometheus metrics are served on a separate port (--metrics-port).

Privacy: requests are described in logs and errors by counts and positions
only, never by content.
"""

from __future__ import annotations

import argparse
import base64
import json
import logging
import os
import signal
import sys
import threading
import time
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Protocol, Sequence

import numpy as np

from . import __version__, batching, metrics
from .fasta import VALID_RESIDUES

log = logging.getLogger("esm_embed.server")

LABEL = "served"  # value of the "strategy" label: batches chosen by the caller


class Model(Protocol):
    dim: int
    max_residues: int

    def embed(self, sequences: Sequence[str]) -> np.ndarray: ...


@dataclass(frozen=True)
class Limits:
    max_sequences: int = 256
    max_batch_tokens: int = 65_536  # padded: batch size x (longest + 2)
    max_body_bytes: int = 4 * 1024 * 1024


class RequestError(Exception):
    def __init__(self, status: HTTPStatus, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.message = message


def validate(body: bytes, limits: Limits, max_residues: int) -> list[str]:
    """Parse and check a batch request. Errors name positions, never content."""
    try:
        payload = json.loads(body)
    except (json.JSONDecodeError, UnicodeDecodeError):
        raise RequestError(HTTPStatus.BAD_REQUEST, "body is not valid JSON") from None
    if not isinstance(payload, dict) or set(payload) != {"sequences"}:
        raise RequestError(HTTPStatus.BAD_REQUEST, 'expected exactly {"sequences": [...]}')
    seqs = payload["sequences"]
    if not isinstance(seqs, list) or not seqs:
        raise RequestError(HTTPStatus.BAD_REQUEST, '"sequences" must be a non-empty list')
    if len(seqs) > limits.max_sequences:
        raise RequestError(
            HTTPStatus.REQUEST_ENTITY_TOO_LARGE,
            f"{len(seqs)} sequences; at most {limits.max_sequences} per batch",
        )
    for i, s in enumerate(seqs):
        if not isinstance(s, str) or not s:
            raise RequestError(HTTPStatus.UNPROCESSABLE_ENTITY, f"sequence {i} is empty or not a string")
        if len(s) > max_residues:
            raise RequestError(
                HTTPStatus.UNPROCESSABLE_ENTITY,
                f"sequence {i} has {len(s)} residues; the model accepts at most {max_residues}",
            )
        if not set(s) <= VALID_RESIDUES:
            raise RequestError(
                HTTPStatus.UNPROCESSABLE_ENTITY,
                f"sequence {i} contains characters outside the amino-acid alphabet",
            )
    padded = len(seqs) * (max(map(len, seqs)) + batching.SPECIAL_TOKENS)
    if padded > limits.max_batch_tokens:
        raise RequestError(
            HTTPStatus.REQUEST_ENTITY_TOO_LARGE,
            f"batch needs {padded} padded tokens; at most {limits.max_batch_tokens}",
        )
    return seqs


class Service:
    """Model lifecycle and batch execution, independent of HTTP."""

    def __init__(self, model_name: str, limits: Limits) -> None:
        self.model_name = model_name
        self.limits = limits
        self.model: Model | None = None
        self._run_lock = threading.Lock()

    @property
    def ready(self) -> bool:
        return self.model is not None

    def load(self, factory) -> None:
        t0 = time.perf_counter()
        self.model = factory()
        log.info("model loaded in %.1fs (dim=%d)", time.perf_counter() - t0, self.model.dim)

    def run(self, body: bytes) -> dict:
        model = self.model
        if model is None:
            raise RequestError(HTTPStatus.SERVICE_UNAVAILABLE, "model is still loading")
        seqs = validate(body, self.limits, model.max_residues)
        lengths = tuple(map(len, seqs))
        batch = batching.Batch(tuple(range(len(seqs))), lengths)

        with self._run_lock:
            t0 = time.perf_counter()
            vectors = model.embed(seqs)
            dt = time.perf_counter() - t0

        metrics.batch_seconds.labels(LABEL).observe(dt)
        metrics.sequences.labels(LABEL).inc(len(seqs))
        metrics.residues.labels(LABEL).inc(sum(lengths))
        metrics.tokens_computed.labels(LABEL).inc(batch.padded_tokens)
        metrics.pad_tokens.labels(LABEL).inc(batch.pad_tokens)
        metrics.residues_per_second.labels(LABEL).set(sum(lengths) / dt if dt else 0)
        log.info(
            "batch sequences=%d padded_tokens=%d pad_tokens=%d seconds=%.3f",
            len(seqs), batch.padded_tokens, batch.pad_tokens, dt,
        )
        data = np.ascontiguousarray(vectors, dtype="<f4")
        return {
            "model": self.model_name,
            "dim": int(data.shape[1]),
            "count": int(data.shape[0]),
            "embeddings": base64.b64encode(data.tobytes()).decode("ascii"),
        }


def make_handler(service: Service) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = f"esm-embed-server/{__version__}"
        protocol_version = "HTTP/1.1"  # keep-alive for the gateway's connection pool

        def _send(self, status: HTTPStatus, payload: dict) -> None:
            body = json.dumps(payload, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:  # noqa: N802 (http.server naming)
            if self.path == "/healthz":
                self._send(HTTPStatus.OK, {"status": "ok"})
            elif self.path == "/readyz":
                if service.ready:
                    self._send(HTTPStatus.OK, {"status": "ready", "model": service.model_name})
                else:
                    self._send(HTTPStatus.SERVICE_UNAVAILABLE, {"status": "loading"})
            else:
                self._send(HTTPStatus.NOT_FOUND, {"error": "not found"})

        def do_POST(self) -> None:  # noqa: N802
            if self.path != "/v1/batch":
                self._send(HTTPStatus.NOT_FOUND, {"error": "not found"})
                return
            try:
                length = int(self.headers.get("Content-Length", "-1"))
            except ValueError:
                length = -1
            if length < 0:
                self._send(HTTPStatus.LENGTH_REQUIRED, {"error": "Content-Length required"})
                return
            if length > service.limits.max_body_bytes:
                # Don't read the body; close so the connection isn't left mid-request.
                self.close_connection = True
                self._send(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, {"error": "body too large"})
                return
            body = self.rfile.read(length)
            try:
                self._send(HTTPStatus.OK, service.run(body))
            except RequestError as exc:
                self._send(exc.status, {"error": exc.message})
            except Exception:
                # The traceback could include tensors built from input, so log
                # only the exception type.
                log.error("batch failed: %s", sys.exc_info()[0].__name__)
                self._send(HTTPStatus.INTERNAL_SERVER_ERROR, {"error": "internal error"})

        def log_request(self, code="-", size="-") -> None:
            log.debug("%s %s %s", self.command, self.path, code)

        def log_message(self, format, *args) -> None:  # noqa: A002
            log.debug("http: " + format, *args)

    return Handler


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="esm-embed-server", description=__doc__.splitlines()[0])
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument("--port", type=int, default=8000)
    p.add_argument("--metrics-port", type=int, default=9100, help="0 = off")
    p.add_argument("--model", default=os.environ.get("ESM_MODEL", "facebook/esm2_t6_8M_UR50D"))
    p.add_argument(
        "--model-name",
        default=os.environ.get("ESM_MODEL_NAME"),
        help="name reported to clients (default: --model)",
    )
    p.add_argument("--threads", type=int, default=None, help="torch CPU threads")
    p.add_argument("--max-sequences", type=int, default=Limits.max_sequences)
    p.add_argument("--max-batch-tokens", type=int, default=Limits.max_batch_tokens)
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
        stream=sys.stderr,
    )
    args = parse_args(argv)
    service = Service(
        args.model_name or args.model,
        Limits(max_sequences=args.max_sequences, max_batch_tokens=args.max_batch_tokens),
    )
    metrics.serve(args.metrics_port)
    metrics.run_info.labels(LABEL, service.model_name, __version__).set(1)

    def load() -> None:
        from .model import Embedder

        try:
            service.load(lambda: Embedder(args.model, threads=args.threads))
        except Exception:
            log.critical("model failed to load: %s", sys.exc_info()[0].__name__)
            os._exit(1)

    # Load in the background so /healthz answers during start-up.
    threading.Thread(target=load, name="model-load", daemon=True).start()

    httpd = ThreadingHTTPServer((args.host, args.port), make_handler(service))
    # On shutdown, wait for requests in progress instead of killing them.
    httpd.daemon_threads = False
    httpd.block_on_close = True

    def stop(signum, _frame) -> None:
        log.info("received signal %d; finishing requests in progress", signum)
        # shutdown() blocks until serve_forever() returns, so call it off this thread.
        threading.Thread(target=httpd.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    log.info("listening on %s:%d", args.host, args.port)
    httpd.serve_forever()
    httpd.server_close()
    log.info("stopped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
