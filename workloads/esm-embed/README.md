# esm-embed

Batch protein embeddings with [ESM-2](https://github.com/facebookresearch/esm),
built to measure what batching costs.

A transformer processes each batch as a rectangle: every sequence is padded to
the longest one in its batch. Padding costs the same compute as real residues
and produces nothing. Protein lengths vary widely, so how sequences are grouped
decides how much of the hardware's work is wasted. This job reports that waste
for every run.

## Usage

```bash
esm-embed --input proteins.fasta --output out/ --strategy token_budget
```

| Output | Contents |
|---|---|
| `embeddings.npy` | float32 array (N × 320 for the 8M model), rows in input order |
| `ids.txt` | record ids, same order |
| `summary.json` | parameters, padding cost, throughput, timings |

| Option | Meaning |
|---|---|
| `--strategy fixed --batch-size N` | input order, N sequences per batch (the common default) |
| `--strategy token_budget --max-tokens T` | sorted by length, each batch packed up to T padded tokens |
| `--plan-only` | compute batches and padding cost without loading the model |
| `--metrics-port P` | serve Prometheus metrics while running |

Each embedding is the mean of the final-layer residue representations;
start, end and padding tokens are excluded. A test checks that **batching
never changes an embedding**: the same protein gets the same vector whether
it's embedded alone or packed with others.

## First result

300 reviewed human proteins from UniProt (47–991 residues, median 420),
ESM-2 8M, laptop CPU with 4 threads, **single run**:

| Strategy | Batches | Padding | Compute | Residues/s |
|---|---|---|---|---|
| fixed, 8 per batch | 38 | 42.4% | 14.8 s | 8,877 |
| token budget, 8,192 tokens | 18 | 5.2% | 7.4 s | 17,848 |

Sorting and packing by length cut wasted computation from 42% to 5% and
doubled throughput, with identical embeddings. This is one run on one machine;
repeated runs, larger models and GPUs come next.

## Model server

The same image also runs as a long-lived server for on-request embedding. It
sits behind the platform's [inference gateway](../../docs/design/gateway.md),
which decides which sequences are batched together; the server only runs the
batches it is given, one at a time.

```bash
esm-embed-server --port 8000 --metrics-port 9100
```

| Endpoint | Meaning |
|---|---|
| `POST /v1/batch` | `{"sequences": [...]}` → `{"model", "dim", "count", "embeddings"}`; embeddings are base64 float32 (little-endian, one row per sequence, request order) |
| `GET /healthz` | process is up |
| `GET /readyz` | model loaded; 503 while loading |

Invalid batches get 400, 413 or 422, with errors naming the sequence's
position, never its content. On SIGTERM the server finishes the batch in
progress before exiting.

## Privacy

Input sequences may be unpublished research data. The job never logs
sequences or record ids, only counts and timings; a test enforces this. On the
platform, the log collector also drops any line that looks like a protein
sequence.

## Container

- CPU-only PyTorch; model weights baked in at a pinned revision, so jobs run
  fully offline and every image tag is reproducible
- runs as a non-root user with a read-only root filesystem and no Linux
  capabilities, as the platform's `restricted` namespace requires
- built for linux/amd64 and linux/arm64, signed with Sigstore keyless
  signing, with SBOM and build provenance attached

## Data and model licences

- Sample sequences: [UniProt](https://www.uniprot.org/) (UniProtKB/Swiss-Prot),
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
- Model: `facebook/esm2_t6_8M_UR50D` by Meta AI, from
  [facebookresearch/esm](https://github.com/facebookresearch/esm) (MIT licence).
