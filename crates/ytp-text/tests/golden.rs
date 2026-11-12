//! Parity with the Python app: tests/golden/tokens.json and phonemes.json.

use std::path::PathBuf;

use serde_json::{json, Value};
use ytp_text::tokenize::expand_token;
use ytp_text::{
    guess_phonemes, phones_of, tokenize_full, tokenize_marked, word_to_phonemes, NoLexicon,
};

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("{}: {e}", path.display());
    }))
    .unwrap()
}

#[test]
fn tokens_match_python() {
    let g = golden("tokens.json");
    let sentences = g["sentences"].as_array().unwrap();
    assert!(sentences.len() >= 20);
    for s in sentences {
        let text = s["text"].as_str().unwrap();
        let marked: Vec<Value> = tokenize_marked(text, &NoLexicon)
            .into_iter()
            .map(|(w, e, n, r)| json!([w, e, n, r]))
            .collect();
        assert_eq!(Value::Array(marked), s["marked"], "marked: {text}");
        let full: Vec<Value> = tokenize_full(text, &NoLexicon)
            .iter()
            .map(|t| t.to_json())
            .collect();
        assert_eq!(Value::Array(full), s["full"], "full: {text}");
    }
}

/// Markup the golden sentences do not reach (effects, notes, spelling, noises,
/// corpus-decided held letters): fixtures written by tools/text/export_data.py.
#[test]
fn markup_matches_python() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/markup.json");
    let g: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let lex: std::collections::HashMap<String, usize> = g["clips"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_u64().unwrap() as usize))
        .collect();
    for c in g["cases"].as_array().unwrap() {
        let text = c["text"].as_str().unwrap();
        let full: Vec<Value> = tokenize_full(text, &lex).iter().map(|t| t.to_json()).collect();
        assert_eq!(Value::Array(full), c["full"], "{text}");
    }
}

#[test]
fn token_cases_match_python() {
    let g = golden("tokens.json");
    let cases = g["cases"].as_array().unwrap();
    assert!(cases.len() >= 37);
    for c in cases {
        let token = c["token"].as_str().unwrap();
        let got = expand_token(token, &NoLexicon);
        assert_eq!(json!(got), c["expanded"], "token {token:?}");
    }
}

#[test]
fn phonemes_match_python() {
    let g = golden("phonemes.json");
    let words = g["words"].as_array().unwrap();
    assert!(words.len() >= 59);
    for w in words {
        let word = w["word"].as_str().unwrap();
        assert_eq!(json!(word_to_phonemes(word)), w["phonemes"], "phonemes of {word}");
        let expect_of = &w["phones_of"];
        let got_of = word_to_phonemes(word).and_then(|_| phones_of(word));
        assert_eq!(json!(got_of), *expect_of, "phones_of {word}");
    }
}

#[test]
fn guessed_phonemes_match_python() {
    let g = golden("phonemes.json");
    for w in g["nonsense"].as_array().unwrap() {
        let word = w["word"].as_str().unwrap();
        let got = guess_phonemes(word).unwrap_or_default();
        assert_eq!(json!(got), w["guess"], "guess {word}");
    }
}
