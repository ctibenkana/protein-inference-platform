# PossibleSequenceInLogs

**Severity:** critical · **Source:** `loki_process_custom_possible_sequence_lines_total` (Alloy, per source stream)

## Meaning
A service wrote a log line containing a run of 30 or more amino-acid letters,
which looks like a protein sequence. Alloy dropped the line before storage, so
**nothing was saved**, but a service is logging data it must never log.

Sequences can be unpublished lab data. The filter is the second line of
defence; the first is that services never log sequences. This alert means the
first line failed.

## Impact
No data has leaked into log storage. But:
- the same service may be leaking the data elsewhere (error reports,
  responses, files), which the filter doesn't cover
- the filter is a pattern match; a differently formatted leak could pass it

Treat it as a data-protection incident until the cause is found.

## Diagnose
1. **Find the source.** The alert's `namespace`, `pod` and `container`
   labels name it. Alloy counts matches per source stream without recording
   the matched text. For the history across all sources:
   ```promql
   sum by (namespace, pod, container) (increase(loki_process_custom_possible_sequence_lines_total[1h]))
   ```
   Read the pod's stored log lines around that time for context (the
   matching lines themselves were dropped):
   ```logql
   {namespace="<namespace>", pod="<pod>"}
   ```
2. **Find the code path.** Look for the log, print or exception that included
   the sequence: typically an error message that echoes the input.
3. **Rule out a false positive.** Long upper-case strings made only of the
   letters `ACDEFGHIKLMNPQRSTVWY` (some IDs or hashes) can match. If it's one
   of those, it still shouldn't be in logs at that length.

## Fix
- Remove the sequence from the log statement and log a count or record
  index instead.
- Add a test that fails if the code path logs input content (see
  `test_logs_never_contain_sequences_or_ids` in `workloads/esm-embed/tests/test_cli.py`).
- Deploy the fix.

## Afterwards
- Check the service's other outputs for the same leak.
- Tell the data owner (the lab) what happened, even though nothing was stored.
- Record the incident: cause, exposure, fix.
