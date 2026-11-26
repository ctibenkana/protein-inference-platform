"""Minimal FASTA reader.

Sequences can be unpublished lab data, so nothing in this module logs or
prints sequence content; errors mention record numbers only.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Iterator

# The 20 standard amino acids plus the IUPAC ambiguity/rare codes
# ESM-2's vocabulary accepts (B, Z, X, U, O).
VALID_RESIDUES = frozenset("ACDEFGHIKLMNPQRSTVWYBZXUO")


@dataclass(frozen=True)
class Record:
    id: str
    sequence: str

    def __len__(self) -> int:
        return len(self.sequence)


class FastaError(ValueError):
    pass


def parse(lines: Iterator[str]) -> Iterator[Record]:
    """Yield records from FASTA text. The id is the first word of the header."""
    header: str | None = None
    chunks: list[str] = []
    count = 0

    def emit() -> Record:
        seq = "".join(chunks).upper()
        if not seq:
            raise FastaError(f"record {count} has an empty sequence")
        bad = set(seq) - VALID_RESIDUES
        if bad:
            raise FastaError(
                f"record {count} contains {len(bad)} invalid residue code(s)"
            )
        return Record(id=header.split()[0] if header.split() else f"record_{count}", sequence=seq)

    for raw in lines:
        line = raw.strip()
        if not line or line.startswith(";"):
            continue
        if line.startswith(">"):
            if header is not None:
                yield emit()
            count += 1
            header = line[1:].strip()
            chunks = []
        else:
            if header is None:
                raise FastaError("sequence data found before the first '>' header")
            chunks.append(line.replace(" ", ""))

    if header is not None:
        yield emit()


def read(path: str | Path) -> list[Record]:
    with open(path, encoding="utf-8") as fh:
        records = list(parse(fh))
    ids = [r.id for r in records]
    if len(set(ids)) != len(ids):
        raise FastaError("duplicate record ids in input")
    return records
