//! Text side of the generator, ported from the Python app: number words,
//! the pronunciation dictionary (CMUdict, embedded) and the tokeniser.
//!
//! No espeak-ng, no nltk: the dictionary is vendored in `data/`.

pub mod cmudict;
pub mod num2words;
pub mod phonemes;
pub mod tokenize;

pub use phonemes::{guess_phonemes, is_vowel, phones_of, word_to_phonemes};
pub use tokenize::{
    clean_word, effective_fx, tokenize, tokenize_full, tokenize_marked, EffectiveFx, Fx,
    Lexicon, NoLexicon, Note, Token,
};

/// Characters Python's `\w` matches (str patterns): letters, digits, `_`.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Number of characters, as Python's `len` counts them.
pub fn clen(s: &str) -> usize {
    s.chars().count()
}

/// `s[a:b]` by character index (Python slicing, clamped).
pub fn cslice(s: &str, a: usize, b: usize) -> &str {
    let mut it = s.char_indices().map(|(i, _)| i).chain(std::iter::once(s.len()));
    let start = it.clone().nth(a).unwrap_or(s.len());
    let end = it.nth(b).unwrap_or(s.len());
    if start >= end {
        ""
    } else {
        &s[start..end]
    }
}
