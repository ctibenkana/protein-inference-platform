# Gateway load tests

**Date:** 2026-11-28 · **Gateway:** `f66c56a` · **Raw results:**
[`services/gateway/bench/results/`](../../services/gateway/bench/results/)

Questions:

1. Does length-aware batching matter under realistic, continuous traffic, or
   only for bursts?
2. What should the queue limit and the batching wait be?
3. What latency can the service promise, and at what load?

## Method

- **Open-loop load.** [`loadgen`](../../services/gateway/src/bin/loadgen/main.rs)
  sends requests at Poisson-distributed times at a fixed average rate,
  whether or not earlier requests have finished, like independent users.
  Latency is measured from each request's *scheduled* send time. A
  closed-loop tester (N clients, each waiting for its last answer) slows
  down exactly when the system does and under-reports latency; this design
  avoids that ("coordinated omission").
- **Requests.** One sequence per request. Sequences are random amino acids
  whose lengths are drawn from 300 reviewed human proteins (47–991
  residues, median 420). Cost depends on length, not content, so no real
  sequences are used.
- **Steps.** 5 s warm-up (sent, not measured), 30 s measured, 10 s pause.
  The gateway is restarted between runs and the runner waits for it to
  finish draining, so leftover work can't leak into the next run (an early
  run that didn't wait is discarded; it made the second policy look slow).
- **Setup.** One MacBook (10 cores: 4 performance, 6 efficiency). ESM-2 8M
  model server with 2 CPU threads, as deployed; one gateway with backend
  concurrency 1; deadline 30 s. Gateway, model server and load generator
  share the machine.

Reproduce: start a model server, then
`services/gateway/bench/run.sh <name> "<gateway flags>" "<loadgen flags>"`;
`bench/summarize.py` prints the tables below from the result files.

## E1: fifo vs length-aware under rising load

| Run | Offered req/s | Served req/s | Refused | Timed out | p50 | p90 | p99 | Padding | Mean batch |
|---|---|---|---|---|---|---|---|---|---|
| fifo | 5 | 4.8 | 0% | 0% | 0.22 s | 4.95 s | 5.75 s | 21.7% | 1.76 |
| fifo | 10 | 9.2 | 0% | 0% | 7.87 s | 10.77 s | 11.96 s | 44.2% | 9.38 |
| fifo | 15 | 8.8 | 0% | 46% | 15.31 s | 26.68 s | 29.97 s | 49.5% | 9.3 |
| fifo | 20 | 13.0 | 31% | 3% | 25.42 s | 28.88 s | 29.94 s | 43.5% | 9.37 |
| fifo | 25 | 21.1 | 17% | 0% | 19.69 s | 24.75 s | 25.19 s | 45.9% | 9.55 |
| fifo | 30 | 20.4 | 31% | 0% | 19.64 s | 22.60 s | 23.83 s | 45.8% | 9.47 |
| length-aware | 5 | 4.8 | 0% | 0% | 0.05 s | 0.11 s | 0.76 s | 0.3% | 1.04 |
| length-aware | 10 | 9.2 | 0% | 0% | 0.06 s | 0.15 s | 0.55 s | 0.9% | 1.08 |
| length-aware | 15 | 16.2 | 0% | 0% | 0.06 s | 0.15 s | 0.33 s | 1.0% | 1.12 |
| length-aware | 20 | 19.6 | 0% | 0% | 0.08 s | 0.34 s | 0.68 s | 2.1% | 1.26 |
| length-aware | 25 | 25.5 | 0% | 0% | 0.24 s | 1.16 s | 1.79 s | 5.0% | 1.83 |
| length-aware | 30 | 29.6 | 0% | 0% | 0.43 s | 3.08 s | 5.57 s | 5.9% | 2.49 |

(Served can exceed offered slightly: Poisson arrivals vary around the mean.)

**Length-aware batching carries 3–4× the load at the same latency.** It
keeps p99 under 1 s up to 20 requests/s; fifo is already at 5.8 s at
5 requests/s and collapses (46% timed out) at 15.

The mechanism is a feedback loop. When two requests are waiting, fifo puts
them in one batch whatever their lengths, so a short protein is padded to a
long one's length. Padding is extra work, extra work lengthens the queue, a
longer queue makes bigger and more mixed batches, and those carry more
padding: fifo's padding climbs to 44–50% and its batches to ~9 sequences. A
length-aware batch only combines similar lengths (here, a 10% padding cap),
so its work stays close to the real residue count and the queue doesn't run
away. At moderate load its batches are mostly single sequences: it batches
when batching is free, not by default.

Above saturation, fifo's numbers are erratic (served rate rises from 15 to
25 requests/s): with a large queue, requests expire after compute has been
spent on them (congestion collapse) until refusals at the queue limit start
shedding load.

## E2: the queue limit under overload (35 requests/s, length-aware)

| Queue limit (tokens) | Offered req/s | Served req/s | Refused | Timed out | p50 | p90 | p99 | Padding | Mean batch |
|---|---|---|---|---|---|---|---|---|---|
| 131,072 | 35 | 33.9 | 0% | 0% | 2.80 s | 5.70 s | 7.14 s | 6.6% | 10.43 |
| 32,768 | 35 | 32.8 | 3% | 0% | 1.74 s | 3.83 s | 5.06 s | 7.9% | 8.88 |
| 16,384 | 35 | 31.5 | 7% | 0% | 0.65 s | 2.02 s | 3.02 s | 7.8% | 4.19 |

The queue limit is a dial between latency and accepted work: 16,384 tokens
(about 1.2 s of work at this throughput) cut p99 from 7.1 s to 3.0 s for 7%
of requests refused at once. Clients get a 503 with `Retry-After` in
milliseconds instead of a slow answer.

## E3 and E4: the batching wait

At 15 requests/s:

| `max_wait` | Offered req/s | Served req/s | Refused | Timed out | p50 | p90 | p99 | Padding | Mean batch |
|---|---|---|---|---|---|---|---|---|---|
| 0 ms | 15 | 14.0 | 0% | 0% | 0.04 s | 0.09 s | 0.16 s | 0.3% | 1.03 |
| 10 ms | 15 | 14.0 | 0% | 0% | 0.06 s | 0.16 s | 0.32 s | 1.0% | 1.1 |
| 50 ms | 15 | 14.0 | 0% | 0% | 0.10 s | 0.18 s | 0.30 s | 1.9% | 1.18 |

Near capacity (E4: 25 and 30 requests/s, 0 vs 10 ms, two arrival seeds
each), p99 varied more between seeds (1.2 s vs 4.0 s at 30 requests/s, same
settings) than between waits. No run showed a benefit from waiting.

Waiting only helps a scheduler that would otherwise send half-empty batches
to an idle model. Here, nothing is sent while the model server is busy, so
batches fill by themselves under load; a wait only adds delay when it is
idle. **Default changed from 10 ms to 0.**

## Decisions

| Setting | Before | After | Evidence |
|---|---|---|---|
| `--max-wait-ms` | 10 | **0** | E3, E4 |
| `--max-queue-tokens` | 131,072 | **16,384** | E2: p99 under overload 7.1 s → 3.0 s |
| `--max-request-residues` | 65,536 | **8,192** | must fit the smaller queue; ~8 full-length proteins; bulk work goes to the batch job |
| Latency SLO | not set | **95% of requests answered within 1 s** | E1: p99 0.68 s at 20 requests/s |
| Planned capacity | not set | **20 requests/s** of single proteins per model-server replica (2 CPU threads) | E1: degrades from 25 |

## Limits of these results

- One machine, shared by load generator, gateway and model server; efficiency
  cores make timings noisy. Single runs except E4. Near capacity, results
  depend strongly on the arrival pattern (E4).
- One sequence per request; multi-sequence requests not yet measured.
- The queue limit is in tokens, so it is specific to this hardware: a GPU
  would justify a far larger one. Expressing it as *seconds of work*, from
  the measured drain rate, would carry across hardware.
- CPU only, ESM-2 8M. Larger models and GPUs come with the GKE stage.
