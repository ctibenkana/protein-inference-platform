#!/usr/bin/env bash
# One benchmark run: start a gateway with the given flags, drive it with
# loadgen, save the per-step results as JSON lines in bench/results/.
#
#   bench/run.sh <name> "<gateway flags>" "<loadgen flags>"
#   e.g. bench/run.sh fifo "--batching fifo" "--rates 10,20,30 --duration 30"
#
# Needs a model server at $BACKEND (default http://127.0.0.1:8000), e.g.
#   docker run --rm -p 8000:8000 --entrypoint esm-embed-server \
#     ghcr.io/ctibenkana/esm-embed:main --threads 2
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
name=$1 gateway_flags=$2 loadgen_flags=$3
backend=${BACKEND:-http://127.0.0.1:8000}
out=bench/results
mkdir -p "$out"

cargo build -q --release
curl -sf "$backend/readyz" >/dev/null || { echo "no model server ready at $backend" >&2; exit 1; }

# shellcheck disable=SC2086 # flags are intentionally word-split
RUST_LOG=warn ./target/release/gateway --backend-url "$backend" \
  --listen 127.0.0.1:8088 --metrics-listen 127.0.0.1:9188 --shutdown-delay 0 \
  --log-format text $gateway_flags >"$out/$name.gateway.log" 2>&1 &
gateway=$!
# Wait for the gateway to finish draining, so leftover queued work can't
# load the model server during the next run.
trap 'kill $gateway 2>/dev/null || true; wait $gateway 2>/dev/null || true' EXIT
sleep 1

echo "== $name: gateway $gateway_flags" >&2
# shellcheck disable=SC2086
./target/release/loadgen --url http://127.0.0.1:8088/v1/embeddings \
  --metrics-url http://127.0.0.1:9188/metrics --label "$name" $loadgen_flags >"$out/$name.jsonl"
