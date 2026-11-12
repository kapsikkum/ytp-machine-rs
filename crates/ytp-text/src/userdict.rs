//! Pronunciation overrides from `pronunciations.csv` (`_load_user_dict` and
//! friends in `app/phonemes.py`).
//!
//! Two files can exist: `<data>/pronunciations.csv` for every corpus and
//! `corpora/<name>/pronunciations.csv` for one (it travels in the bundle and
//! wins). Two columns: a word, then ARPAbet (`N AH G`), words that already
//! sound right (`wee`, `mick nug`), `=letters` or `=skip`.
//!
//! A dictionary belongs to a corpus, so lookups are scoped: while a
//! [`Scope`] guard is alive on a thread, [`crate::word_to_phonemes`] consults
//! that dictionary (and keeps its memo in it) instead of the global one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::phonemes::{self, is_vowel, Phones};

const CONSONANTS: [&str; 24] = [
    "B", "CH", "D", "DH", "F", "G", "HH", "JH", "K", "L", "M", "N", "NG", "P", "R", "S", "SH", "T",
    "TH", "V", "W", "Y", "Z", "ZH",
];

fn is_arpabet(t: &str) -> bool {
    is_vowel(t) || CONSONANTS.contains(&t)
}

/// Which file a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `<data>/pronunciations.csv`
    All,
    /// The corpus's own file.
    Voice,
}

#[derive(Debug, Default)]
pub struct UserDict {
    /// word -> phonemes, or `None` for `=skip` (deliberately unpronounceable).
    words: HashMap<String, Option<Phones>>,
    /// word -> (file it came from, the raw value written).
    entries: HashMap<String, (Origin, String)>,
    /// `(origin, word, value)` for rows nothing could be made of.
    pub rejected: Vec<(Origin, String, String)>,
    pub(crate) memo: Mutex<HashMap<String, Option<Phones>>>,
}

enum Parsed {
    Phones(Phones),
    Skip,
    Letters,
    Unresolved,
}

fn parse_value(value: &str, known: &HashMap<String, Option<Phones>>) -> Parsed {
    let value = value.trim();
    if value.is_empty() {
        return Parsed::Unresolved;
    }
    match value.to_lowercase().as_str() {
        "=skip" | "=none" | "-" => return Parsed::Skip,
        "=letters" => return Parsed::Letters,
        _ => {}
    }
    let spaced = value.replace(',', " ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    if !tokens.is_empty() && tokens.iter().all(|t| is_arpabet(&t.to_uppercase())) {
        return Parsed::Phones(tokens.iter().map(|t| t.to_uppercase()).collect());
    }
    let mut out: Phones = Vec::new();
    for token in tokens {
        let clean: String = token
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || *c == '\'')
            .collect();
        if clean.is_empty() {
            continue;
        }
        // A lone consonant means its sound, not its name.
        if clean.chars().count() == 1 && !"aeiou".contains(&clean) {
            out.push(clean.to_uppercase());
            continue;
        }
        match phonemes::direct_with(&clean, known) {
            Some(got) if !got.is_empty() => out.extend(got),
            _ => return Parsed::Unresolved,
        }
    }
    if out.is_empty() {
        Parsed::Unresolved
    } else {
        Parsed::Phones(out)
    }
}

/// Rows of a CSV file (`csv.reader` defaults: commas, double quotes with
/// `""` for a quote, quoted fields may hold line breaks).
fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut in_row = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                quoted = true;
                in_row = true;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                in_row = true;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if in_row || !field.is_empty() {
                    row.push(std::mem::take(&mut field));
                }
                rows.push(std::mem::take(&mut row));
                in_row = false;
            }
            _ => {
                field.push(c);
                in_row = true;
            }
        }
    }
    if in_row || !field.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

fn dict_word(cell: &str) -> String {
    cell.trim()
        .to_lowercase()
        .chars()
        .filter(|&c| crate::is_word_char(c) || c == '\'')
        .collect()
}

impl UserDict {
    /// Dictionaries from the shared file's text and the corpus's, the corpus
    /// winning (later rows win, and a word can use words defined above it).
    pub fn from_csv(global: Option<&str>, voice: Option<&str>) -> Self {
        let mut d = UserDict::default();
        for (text, origin) in [(global, Origin::All), (voice, Origin::Voice)] {
            if let Some(text) = text {
                d.read(text, origin);
            }
        }
        d
    }

    fn read(&mut self, text: &str, origin: Origin) {
        for row in csv_rows(text) {
            let Some(first) = row.first() else { continue };
            if first.trim_start().starts_with('#') {
                continue;
            }
            let word = dict_word(first);
            if word.is_empty() || row.len() < 2 {
                continue;
            }
            let value = row[1].trim();
            let phones = match parse_value(value, &self.words) {
                Parsed::Letters => phonemes::letters_to_phones(&word),
                Parsed::Skip => None,
                Parsed::Phones(p) => Some(p),
                Parsed::Unresolved => {
                    self.rejected.push((origin, word, value.to_string()));
                    continue;
                }
            };
            self.words.insert(word.clone(), phones);
            self.entries.insert(word, (origin, value.to_string()));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// `Some(None)` is a deliberate `=skip`.
    pub fn get(&self, word: &str) -> Option<&Option<Phones>> {
        self.words.get(word)
    }

    /// The winning row for `word`: which file and what was written.
    pub fn entry(&self, word: &str) -> Option<(Origin, &str)> {
        self.entries.get(word).map(|(o, v)| (*o, v.as_str()))
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<UserDict>>> = const { RefCell::new(None) };
}

pub(crate) fn active() -> Option<Arc<UserDict>> {
    ACTIVE.with(|a| a.borrow().clone())
}

/// Restores the previous dictionary of this thread when dropped.
pub struct Scope {
    prev: Option<Arc<UserDict>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let prev = self.prev.take();
        ACTIVE.with(|a| *a.borrow_mut() = prev);
    }
}

/// Use `dict` for phoneme lookups on this thread until the guard drops. An
/// empty or absent dictionary means the plain global behaviour.
pub fn scope(dict: Option<&Arc<UserDict>>) -> Scope {
    let next = dict.filter(|d| !d.is_empty()).cloned();
    let prev = ACTIVE.with(|a| std::mem::replace(&mut *a.borrow_mut(), next));
    Scope { prev }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Option<Phones> {
        Some(v.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn csv_handles_quotes_comments_and_bom() {
        let rows = csv_rows("\u{feff}# note\r\nnug,\"N AH G\"\n\n\"a,b\",x\n");
        assert_eq!(rows[0], vec!["# note"]);
        assert_eq!(rows[1], vec!["nug", "N AH G"]);
        assert_eq!(rows[2], Vec::<String>::new());
        assert_eq!(rows[3], vec!["a,b", "x"]);
    }

    #[test]
    fn values_in_every_form() {
        let d = UserDict::from_csv(
            None,
            Some("nug,N AH G\nwii,wee\nmcnug,mick nug\nusb,=letters\nhevexum,=skip\nzoop,zoo p\nbad,qq zzz\n"),
        );
        assert_eq!(d.get("nug"), Some(&p(&["N", "AH", "G"])));
        assert_eq!(d.get("wii"), Some(&p(&["W", "IY"])));
        // defined earlier in the same file, so usable
        assert_eq!(d.get("mcnug"), Some(&p(&["M", "IH", "K", "N", "AH", "G"])));
        assert_eq!(d.get("usb"), Some(&p(&["Y", "UW", "EH", "S", "B", "IY"])));
        assert_eq!(d.get("hevexum"), Some(&None));
        assert_eq!(d.get("zoop"), Some(&p(&["Z", "UW", "P"])));
        assert!(d.get("bad").is_none());
        assert_eq!(d.rejected.len(), 1);
        assert_eq!(d.rejected[0].1, "bad");
        assert_eq!(d.entry("wii"), Some((Origin::Voice, "wee")));
    }

    #[test]
    fn the_corpus_file_wins_over_the_shared_one() {
        let d = UserDict::from_csv(Some("tomato,T AH M EY T OW\nonly,OW N L IY\n"), Some("tomato,T AH M AA T OW\n"));
        assert_eq!(d.get("tomato"), Some(&p(&["T", "AH", "M", "AA", "T", "OW"])));
        assert_eq!(d.entry("tomato").unwrap().0, Origin::Voice);
        assert_eq!(d.entry("only").unwrap().0, Origin::All);
    }
}
