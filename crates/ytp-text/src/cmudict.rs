//! The Carnegie Mellon Pronouncing Dictionary, embedded in the binary.
//! First pronunciation of each word, stress removed (see `data/LICENSE-cmudict.txt`).

use std::collections::HashMap;
use std::sync::OnceLock;

static RAW: &str = include_str!("../data/cmudict.txt");

fn table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::with_capacity(130_000);
        for line in RAW.lines() {
            if let Some((word, phones)) = line.split_once('\t') {
                m.insert(word, phones);
            }
        }
        m
    })
}

/// Is `word` (lower case) in the dictionary?
pub fn contains(word: &str) -> bool {
    table().contains_key(word)
}

/// The (primary) pronunciation of `word` (lower case).
pub fn get(word: &str) -> Option<Vec<String>> {
    table()
        .get(word)
        .map(|p| p.split(' ').map(str::to_string).collect())
}

pub fn len() -> usize {
    table().len()
}

pub fn is_empty() -> bool {
    table().is_empty()
}
