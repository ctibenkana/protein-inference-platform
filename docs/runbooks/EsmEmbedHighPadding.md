# EsmEmbedHighPadding

**Severity:** info · **Source:** `esm_embed_pad_tokens_total`, `esm_embed_tokens_computed_total`

## Meaning
A running embedding job is spending more than 25% of its compute on padding.
This is informational: the run is correct, just wasteful.

## Impact
This run costs more than necessary. Many such runs breach the
[batching-efficiency SLO](../slos.md#2-batching-efficiency).

## Diagnose
The alert's `strategy` label shows how the run batches. `fixed` keeps input
order, so short and long proteins share batches and short ones are padded.

## Fix
Nothing to do for the current run. For future runs, use
`--strategy token_budget` (the default in the Job template).

## Afterwards
If this fires for `token_budget` runs, see
[EsmEmbedPaddingBudgetExceeded](EsmEmbedPaddingBudgetExceeded.md#fix).
