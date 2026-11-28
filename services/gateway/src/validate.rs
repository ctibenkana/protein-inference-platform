//! Request validation.
//!
//! Sequences can be unpublished research data, so every error names the
//! sequence's position in the request and never its content.

use axum::http::StatusCode;

/// The 20 standard amino acids plus the ambiguity and rare codes in
/// ESM-2's vocabulary (B, Z, X, U, O); the same set the model server accepts.
const ALPHABET: &[u8] = b"ACDEFGHIKLMNPQRSTVWYBZXUO";

/// Start and end tokens the model adds to every sequence.
pub const SPECIAL_TOKENS: usize = 2;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Sequences per request.
    pub max_sequences: usize,
    /// Residues summed over a request.
    pub max_request_residues: usize,
    /// Residues in one sequence (the model's context, minus special tokens).
    pub max_residues: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_sequences: 64,
            max_request_residues: 65_536,
            max_residues: 1_022,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("\"sequences\" must not be empty")]
    NoSequences,
    #[error("{count} sequences in one request; at most {max}")]
    TooManySequences { count: usize, max: usize },
    #[error("{total} residues in one request; at most {max}")]
    TooManyResidues { total: usize, max: usize },
    #[error("sequence {index} is empty")]
    EmptySequence { index: usize },
    #[error("sequence {index} has {len} residues; the model accepts at most {max}")]
    SequenceTooLong {
        index: usize,
        len: usize,
        max: usize,
    },
    #[error("sequence {index} contains characters outside the amino-acid alphabet")]
    InvalidResidue { index: usize },
}

impl Invalid {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NoSequences => StatusCode::BAD_REQUEST,
            Self::TooManySequences { .. } | Self::TooManyResidues { .. } => {
                StatusCode::PAYLOAD_TOO_LARGE
            }
            Self::EmptySequence { .. }
            | Self::SequenceTooLong { .. }
            | Self::InvalidResidue { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::NoSequences => "no_sequences",
            Self::TooManySequences { .. } => "too_many_sequences",
            Self::TooManyResidues { .. } => "too_many_residues",
            Self::EmptySequence { .. } => "empty_sequence",
            Self::SequenceTooLong { .. } => "sequence_too_long",
            Self::InvalidResidue { .. } => "invalid_residue",
        }
    }
}

/// Upper-cases sequences in place and checks them against `limits`.
/// Returns the total number of residues.
pub fn validate(sequences: &mut [String], limits: &Limits) -> Result<usize, Invalid> {
    if sequences.is_empty() {
        return Err(Invalid::NoSequences);
    }
    if sequences.len() > limits.max_sequences {
        return Err(Invalid::TooManySequences {
            count: sequences.len(),
            max: limits.max_sequences,
        });
    }
    let mut total = 0;
    for (index, seq) in sequences.iter_mut().enumerate() {
        if seq.is_empty() {
            return Err(Invalid::EmptySequence { index });
        }
        // Non-ASCII input can't be valid; checking bytes after
        // upper-casing ASCII rejects it without decoding.
        seq.make_ascii_uppercase();
        if !seq.bytes().all(|b| ALPHABET.contains(&b)) {
            return Err(Invalid::InvalidResidue { index });
        }
        let len = seq.len();
        if len > limits.max_residues {
            return Err(Invalid::SequenceTooLong {
                index,
                len,
                max: limits.max_residues,
            });
        }
        total += len;
    }
    if total > limits.max_request_residues {
        return Err(Invalid::TooManyResidues {
            total,
            max: limits.max_request_residues,
        });
    }
    Ok(total)
}

/// Token positions a batch computes: every sequence is padded to the longest.
pub fn padded_tokens(lengths: impl IntoIterator<Item = usize> + Clone) -> usize {
    let n = lengths.clone().into_iter().count();
    let longest = lengths.into_iter().max().unwrap_or(0);
    n * (longest + SPECIAL_TOKENS)
}

/// Token positions holding real input (residues plus special tokens).
pub fn real_tokens(lengths: impl IntoIterator<Item = usize>) -> usize {
    lengths.into_iter().map(|l| l + SPECIAL_TOKENS).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "MKTAYIAKQRQISFVKSHFSRQLEERLGLIEVQ";

    fn check(seqs: &[&str], limits: Limits) -> Result<usize, Invalid> {
        let mut owned: Vec<String> = seqs.iter().map(|s| s.to_string()).collect();
        validate(&mut owned, &limits)
    }

    #[test]
    fn accepts_and_uppercases() {
        let mut seqs = vec!["mkta".to_string(), "MA".to_string()];
        assert_eq!(validate(&mut seqs, &Limits::default()), Ok(6));
        assert_eq!(seqs, ["MKTA", "MA"]);
    }

    #[test]
    fn rejects_with_position_and_status() {
        let small = Limits {
            max_sequences: 2,
            max_request_residues: 10,
            max_residues: 6,
        };
        let cases: [(&[&str], Invalid, u16); 6] = [
            (&[], Invalid::NoSequences, 400),
            (
                &["M", "M", "M"],
                Invalid::TooManySequences { count: 3, max: 2 },
                413,
            ),
            (
                &["MKTAYI", "MKTAY"],
                Invalid::TooManyResidues { total: 11, max: 10 },
                413,
            ),
            (&["M", ""], Invalid::EmptySequence { index: 1 }, 422),
            (
                &["M", "MKTAYIA"],
                Invalid::SequenceTooLong {
                    index: 1,
                    len: 7,
                    max: 6,
                },
                422,
            ),
            (&["M1"], Invalid::InvalidResidue { index: 0 }, 422),
        ];
        for (seqs, want, status) in cases {
            let got = check(seqs, small).unwrap_err();
            assert_eq!(got, want);
            assert_eq!(got.status().as_u16(), status);
        }
    }

    #[test]
    fn rejects_non_ascii_and_whitespace() {
        for bad in ["MKΑ", "MK A", "MK\n", "MK*"] {
            assert_eq!(
                check(&[bad], Limits::default()),
                Err(Invalid::InvalidResidue { index: 0 })
            );
        }
    }

    #[test]
    fn errors_never_contain_input() {
        let long = SECRET.repeat(40);
        for seqs in [vec![format!("{SECRET}!")], vec![long]] {
            let mut seqs = seqs;
            let msg = validate(&mut seqs, &Limits::default())
                .unwrap_err()
                .to_string();
            assert!(!msg.contains(SECRET), "{msg}");
        }
    }

    #[test]
    fn token_accounting_matches_the_batch_job() {
        // Same numbers as esm_embed.batching.Batch: lengths 10 and 4.
        assert_eq!(padded_tokens([10, 4]), 24);
        assert_eq!(real_tokens([10, 4]), 18);
        assert_eq!(padded_tokens(Vec::<usize>::new()), 0);
    }
}
