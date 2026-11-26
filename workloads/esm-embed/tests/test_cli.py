import json
from pathlib import Path

import numpy as np
import pytest

from esm_embed import cli

SAMPLE = Path(__file__).parent.parent / "samples" / "uniprot_human_300.fasta"


def run(tmp_path, *extra):
    out = tmp_path / "out"
    code = cli.main(["--input", str(SAMPLE), "--output", str(out), *extra])
    assert code == 0
    return out, json.loads((out / "summary.json").read_text())


def test_plan_only_reports_padding_without_model(tmp_path):
    _, fixed = run(tmp_path / "a", "--plan-only", "--strategy", "fixed", "--batch-size", "8")
    _, budget = run(tmp_path / "b", "--plan-only", "--strategy", "token_budget")
    assert fixed["sequences"] == budget["sequences"] == 300
    assert budget["padding_fraction"] < fixed["padding_fraction"]


def test_logs_never_contain_sequences_or_ids(tmp_path, caplog):
    caplog.set_level("DEBUG")
    run(tmp_path, "--plan-only")
    first_record = SAMPLE.read_text().split("\n>")[0].splitlines()
    first_id = first_record[0][1:].split()[0]
    first_seq_line = first_record[1]
    assert first_id not in caplog.text
    assert first_seq_line not in caplog.text


def test_invalid_input_exits_with_code_2(tmp_path):
    bad = tmp_path / "bad.fasta"
    bad.write_text(">P1\nMK1\n")
    assert cli.main(["--input", str(bad), "--output", str(tmp_path / "o"), "--plan-only"]) == 2


@pytest.mark.model
def test_embeddings_do_not_depend_on_batching(tmp_path):
    """Batching is an optimisation: it must not change any protein's embedding."""
    small = tmp_path / "small.fasta"
    small.write_text("\n>".join(SAMPLE.read_text().split("\n>")[:24]) + "\n")
    outs = []
    for strategy, extra in [("fixed", ["--batch-size", "1"]), ("token_budget", ["--max-tokens", "6000"])]:
        out = tmp_path / strategy
        assert cli.main(["--input", str(small), "--output", str(out), "--strategy", strategy, *extra]) == 0
        outs.append(np.load(out / "embeddings.npy"))
        assert (out / "ids.txt").read_text().count("\n") == 24
    one_by_one, packed = outs
    assert one_by_one.shape == packed.shape == (24, 320)
    np.testing.assert_allclose(one_by_one, packed, rtol=1e-4, atol=1e-4)
