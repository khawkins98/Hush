//! Parakeet vocabulary: `vocab.txt` parsing and SentencePiece join.
//!
//! The upstream `vocab.txt` is one `"<piece> <id>"` pair per line, in
//! id order, 8193 entries with `<blk>` last. Pieces are SentencePiece
//! subwords where `▁` (U+2581 LOWER ONE EIGHTH BLOCK — *not* an
//! underscore) marks a word boundary.
//!
//! Detokenizing is therefore just "replace ▁ with a space and
//! concatenate", which is why this engine needs no SentencePiece
//! dependency. The subtlety is the leading piece: `▁Hello` at the
//! start of an utterance must not produce a leading space.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// SentencePiece word-boundary marker used by the Parakeet vocabulary.
pub const WORD_BOUNDARY: char = '\u{2581}';

/// Token id of `<blk>`, the transducer blank symbol. Also used as the
/// predictor's initial input at the start of an utterance (there is no
/// separate SOS token in this export).
pub const BLANK_ID: i32 = 8192;

/// Expected vocabulary size. The joint head is `VOCAB_SIZE + 5` wide;
/// the trailing 5 are TDT duration logits.
pub const VOCAB_SIZE: usize = 8193;

/// Parakeet's token table, indexed by token id.
pub struct Vocabulary {
    pieces: Vec<String>,
}

impl Vocabulary {
    /// Parse a `vocab.txt`.
    ///
    /// Tolerates trailing blank lines and out-of-order ids (entries are
    /// placed by their declared id, not by line position), because a
    /// hand-regenerated export is a plausible thing for a user to drop
    /// in and silently mis-ordering the table would corrupt every
    /// transcript in a way that's very hard to trace back here.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("parakeet: read vocabulary at {}", path.display()))?;
        Self::parse(&raw)
            .with_context(|| format!("parakeet: parse vocabulary at {}", path.display()))
    }

    /// Parse vocabulary text. Split out from [`load`](Self::load) so
    /// tests don't need a file on disk.
    pub fn parse(raw: &str) -> Result<Self> {
        // Pre-size to the expected table and fill by declared id.
        let mut pieces: Vec<String> = vec![String::new(); VOCAB_SIZE];
        let mut seen = vec![false; VOCAB_SIZE];
        let mut count = 0usize;

        for (lineno, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            // Split on the LAST space: pieces themselves can contain a
            // space (rare, but the table is not ours to constrain), so
            // anchoring on the trailing id is the safe parse.
            let (piece, id) = line.rsplit_once(' ').ok_or_else(|| {
                anyhow!(
                    "line {}: expected \"<piece> <id>\", got {line:?}",
                    lineno + 1
                )
            })?;
            let id: usize = id.trim().parse().map_err(|e| {
                anyhow!(
                    "line {}: token id {id:?} is not an integer: {e}",
                    lineno + 1
                )
            })?;
            if id >= VOCAB_SIZE {
                return Err(anyhow!(
                    "line {}: token id {id} is outside the expected 0..{VOCAB_SIZE} table",
                    lineno + 1
                ));
            }
            if seen[id] {
                return Err(anyhow!("line {}: duplicate token id {id}", lineno + 1));
            }
            seen[id] = true;
            pieces[id] = piece.to_owned();
            count += 1;
        }

        if count != VOCAB_SIZE {
            return Err(anyhow!(
                "expected {VOCAB_SIZE} vocabulary entries, found {count}"
            ));
        }
        Ok(Self { pieces })
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Look up one piece by token id.
    pub fn piece(&self, id: i32) -> Option<&str> {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.pieces.get(i))
            .map(|s| s.as_str())
    }

    /// Join emitted token ids into text.
    ///
    /// `▁` becomes a space, except at the very start where it would
    /// produce a leading one. Unknown ids are skipped rather than
    /// rendered as a placeholder: a single out-of-range id is far more
    /// likely to be a decoder bug than something the user wants to see
    /// in their transcript.
    pub fn decode_tokens(&self, tokens: &[i32]) -> String {
        let mut out = String::new();
        for &id in tokens {
            let Some(piece) = self.piece(id) else {
                continue;
            };
            // Special tokens (`<unk>`, `<pad>`, `<|nospeech|>`, `<blk>`)
            // never belong in user-visible text. The decoder already
            // filters blanks; this catches the rest.
            if piece.starts_with('<') && piece.ends_with('>') {
                continue;
            }
            if let Some(rest) = piece.strip_prefix(WORD_BOUNDARY) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(rest);
            } else {
                out.push_str(piece);
            }
        }
        out
    }
}
