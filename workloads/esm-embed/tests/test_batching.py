import random

import pytest

from esm_embed import batching
from esm_embed.batching import SPECIAL_TOKENS, Batch


def covered(batches):
    return sorted(i for b in batches for i in b.indices)


def test_padding_arithmetic():
    b = Batch(indices=(0, 1), lengths=(10, 4))
    assert b.padded_tokens == 2 * (10 + SPECIAL_TOKENS)
    assert b.real_tokens == 14 + 2 * SPECIAL_TOKENS
    assert b.pad_tokens == 6


def test_fixed_keeps_order_and_sizes():
    batches = batching.fixed([5, 6, 7, 8, 9], batch_size=2)
    assert [b.indices for b in batches] == [(0, 1), (2, 3), (4,)]


def test_token_budget_covers_every_input_once():
    lengths = [random.Random(1).randint(30, 1000) for _ in range(500)]
    batches = batching.token_budget(lengths, max_tokens=4096)
    assert covered(batches) == list(range(500))


def test_token_budget_respects_budget_unless_single_sequence():
    rng = random.Random(2)
    lengths = [rng.randint(30, 1000) for _ in range(300)] + [5000]
    for b in batching.token_budget(lengths, max_tokens=2048):
        assert b.padded_tokens <= 2048 or len(b.indices) == 1


def test_token_budget_pads_less_than_fixed_on_mixed_lengths():
    rng = random.Random(3)
    lengths = [rng.randint(30, 1000) for _ in range(300)]
    fixed = batching.padding_fraction(batching.fixed(lengths, batch_size=8))
    budget = batching.padding_fraction(batching.token_budget(lengths, max_tokens=8192))
    assert budget < fixed / 3


def test_uniform_lengths_have_no_padding():
    assert batching.padding_fraction(batching.fixed([100] * 16, batch_size=4)) == 0


@pytest.mark.parametrize("fn,arg", [(batching.fixed, 0), (batching.token_budget, 0)])
def test_rejects_non_positive_limits(fn, arg):
    with pytest.raises(ValueError):
        fn([10, 20], arg)
