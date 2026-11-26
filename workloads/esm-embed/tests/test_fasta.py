import pytest

from esm_embed import fasta


def parse(text: str) -> list[fasta.Record]:
    return list(fasta.parse(iter(text.splitlines())))


def test_multiline_records_and_ids():
    recs = parse(">sp|P1|A desc here\nMKT\nAYI\n\n>P2\nmkv\n")
    assert [(r.id, r.sequence) for r in recs] == [("sp|P1|A", "MKTAYI"), ("P2", "MKV")]


def test_comments_and_blank_lines_ignored():
    assert parse("; comment\n\n>P1\nMK\n")[0].sequence == "MK"


def test_invalid_residue_rejected_without_echoing_sequence():
    with pytest.raises(fasta.FastaError) as exc:
        parse(">P1\nMKT1AY\n")
    assert "MKT" not in str(exc.value)  # never leak sequence content


def test_empty_sequence_rejected():
    with pytest.raises(fasta.FastaError):
        parse(">P1\n>P2\nMK\n")


def test_data_before_header_rejected():
    with pytest.raises(fasta.FastaError):
        parse("MKT\n>P1\nMK\n")


def test_duplicate_ids_rejected(tmp_path):
    f = tmp_path / "dup.fasta"
    f.write_text(">P1\nMK\n>P1\nMV\n")
    with pytest.raises(fasta.FastaError):
        fasta.read(f)
