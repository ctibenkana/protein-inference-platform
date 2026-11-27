# EsmEmbedPaddingBudgetExceeded

**Severity:** warning · **SLO:** [batching efficiency](../slos.md#2-batching-efficiency), at most 10% of computed tokens are padding over 7 days

## Meaning
Across all embedding jobs in the last 7 days, more than 10% of the compute
was spent on padding: positions that produce nothing. This is a **cost**
objective: padding is paid for in GPU or CPU time like real work.

## Impact
Screens cost more and take longer than they need to. Results aren't affected:
embeddings don't depend on batching (a test enforces this).

## Diagnose
Which strategy is responsible?
```promql
sum by (strategy) (max_over_time(esm_embed_pad_tokens_total[7d]))
/
sum by (strategy) (max_over_time(esm_embed_tokens_computed_total[7d]))
```
And which runs?
```promql
max_over_time(esm_embed_pad_tokens_total[7d]) / max_over_time(esm_embed_tokens_computed_total[7d]) > 0.10
```

## Fix
- Runs using `--strategy fixed`: switch to `--strategy token_budget`. In the
  first benchmark this cut padding from 42% to 5%.
- High padding with `token_budget`: the input mixes very different lengths
  in small numbers, or `--max-tokens` is too small to group similar lengths.
  Raise `--max-tokens` if memory allows.

## Afterwards
If a workload legitimately can't meet 10%, record why and consider a separate
objective for it rather than loosening the global one.
