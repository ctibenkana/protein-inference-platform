"""Batching strategies and their padding cost.

A transformer processes each batch as a rectangle: every sequence is padded
to the longest one in the batch. Padding positions cost the same compute as
real residues but produce nothing. How sequences are grouped into batches
therefore decides how much of the hardware's work is wasted.

Strategies:
- fixed:        input order, a fixed number of sequences per batch.
                The common default; mixes short and long sequences.
- token_budget: sort by length, then pack each batch up to a limit on
                padded tokens (batch size x longest sequence). Similar
                lengths end up together, so little is padded.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable, Sequence

# ESM-2 adds a start (<cls>) and end (<eos>) token to every sequence.
SPECIAL_TOKENS = 2


@dataclass(frozen=True)
class Batch:
    indices: tuple[int, ...]  # positions in the original input
    lengths: tuple[int, ...]  # residue counts, same order as indices

    @property
    def padded_tokens(self) -> int:
        """Tokens actually computed: batch size x (longest + special tokens)."""
        return len(self.lengths) * (max(self.lengths) + SPECIAL_TOKENS)

    @property
    def real_tokens(self) -> int:
        return sum(self.lengths) + SPECIAL_TOKENS * len(self.lengths)

    @property
    def pad_tokens(self) -> int:
        return self.padded_tokens - self.real_tokens


def fixed(lengths: Sequence[int], batch_size: int) -> list[Batch]:
    if batch_size < 1:
        raise ValueError("batch_size must be at least 1")
    batches = []
    for start in range(0, len(lengths), batch_size):
        idx = tuple(range(start, min(start + batch_size, len(lengths))))
        batches.append(Batch(idx, tuple(lengths[i] for i in idx)))
    return batches


def token_budget(lengths: Sequence[int], max_tokens: int) -> list[Batch]:
    """Longest first, packed while batch_size x longest stays within max_tokens.

    A single sequence longer than the budget still gets its own batch, so no
    input is ever dropped.
    """
    if max_tokens < 1:
        raise ValueError("max_tokens must be at least 1")
    order = sorted(range(len(lengths)), key=lambda i: lengths[i], reverse=True)
    batches: list[Batch] = []
    current: list[int] = []
    for i in order:
        # Sorted descending, so the first member is the batch's longest.
        longest = lengths[current[0]] if current else lengths[i]
        if current and (len(current) + 1) * (longest + SPECIAL_TOKENS) > max_tokens:
            batches.append(Batch(tuple(current), tuple(lengths[j] for j in current)))
            current = []
        current.append(i)
    if current:
        batches.append(Batch(tuple(current), tuple(lengths[j] for j in current)))
    return batches


STRATEGIES: dict[str, Callable[..., list[Batch]]] = {
    "fixed": fixed,
    "token_budget": token_budget,
}


def padding_fraction(batches: Sequence[Batch]) -> float:
    computed = sum(b.padded_tokens for b in batches)
    return sum(b.pad_tokens for b in batches) / computed if computed else 0.0
