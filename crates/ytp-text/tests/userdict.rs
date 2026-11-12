//! `pronunciations.csv` overrides, applied per corpus (`_direct` /
//! `_load_user_dict` in `app/phonemes.py`).

use std::sync::Arc;

use ytp_text::userdict::scope;
use ytp_text::{phones_of, word_to_phonemes, UserDict};

fn v(p: &[&str]) -> Option<Vec<String>> {
    Some(p.iter().map(|s| s.to_string()).collect())
}

fn dict(global: Option<&str>, voice: Option<&str>) -> Arc<UserDict> {
    Arc::new(UserDict::from_csv(global, voice))
}

#[test]
fn a_corpus_pronounces_a_word_its_own_way() {
    let plain = word_to_phonemes("tomato");
    assert_eq!(plain, v(&["T", "AH", "M", "EY", "T", "OW"]));
    let rosen = dict(None, Some("tomato,T AH M AA T OW\n"));
    {
        let _g = scope(Some(&rosen));
        assert_eq!(word_to_phonemes("tomato"), v(&["T", "AH", "M", "AA", "T", "OW"]));
        // splice indexes see it too
        assert_eq!(phones_of("tomato"), v(&["T", "AH", "M", "AA", "T", "OW"]));
    }
    // another corpus, and no corpus at all, are untouched
    let other = dict(None, Some("tomato,T AH M AH T OW\n"));
    {
        let _g = scope(Some(&other));
        assert_eq!(word_to_phonemes("tomato"), v(&["T", "AH", "M", "AH", "T", "OW"]));
    }
    assert_eq!(word_to_phonemes("tomato"), plain);
}

#[test]
fn the_memo_does_not_leak_between_dictionaries() {
    let a = dict(None, Some("hevexum,HH EH V AH K S AH M\n"));
    let b = dict(None, Some("hevexum,=skip\n"));
    let in_a = {
        let _g = scope(Some(&a));
        word_to_phonemes("hevexum")
    };
    let in_b = {
        let _g = scope(Some(&b));
        word_to_phonemes("hevexum")
    };
    assert_eq!(in_a, v(&["HH", "EH", "V", "AH", "K", "S", "AH", "M"]));
    assert_ne!(in_b, in_a);
    assert_eq!(word_to_phonemes("hevexum"), None);
}

#[test]
fn skip_suppresses_the_dictionary_word_and_scopes_nest() {
    let skip = dict(None, Some("dog,=skip\n"));
    let say = dict(None, Some("dog,D AO G\n"));
    assert_eq!(word_to_phonemes("dog"), v(&["D", "AO", "G"]));
    let _outer = scope(Some(&skip));
    assert_eq!(word_to_phonemes("dog"), None);
    {
        let _inner = scope(Some(&say));
        assert_eq!(word_to_phonemes("dog"), v(&["D", "AO", "G"]));
    }
    assert_eq!(word_to_phonemes("dog"), None, "the outer scope is back");
}

#[test]
fn derived_forms_use_the_dictionary_for_the_whole_word_only() {
    // Python's -s rule looks the base up in CMU, not in pronunciations.csv:
    // an override for "nug" does not reach "nugs".
    let d = dict(None, Some("nug,N AH G\nnugs,N AH G Z\n"));
    let _g = scope(Some(&d));
    assert_eq!(word_to_phonemes("nug"), v(&["N", "AH", "G"]));
    assert_eq!(word_to_phonemes("nugs"), v(&["N", "AH", "G", "Z"]));
}

#[test]
fn the_shared_file_applies_unless_the_corpus_file_says_otherwise() {
    let d = dict(Some("wii,wee\nkilo,K IY L OW\n"), Some("kilo,K IH L OW\n"));
    let _g = scope(Some(&d));
    assert_eq!(word_to_phonemes("wii"), v(&["W", "IY"]));
    assert_eq!(word_to_phonemes("kilo"), v(&["K", "IH", "L", "OW"]));
}

#[test]
fn an_empty_dictionary_changes_nothing() {
    let d = dict(Some("# only a comment\n"), None);
    assert!(d.is_empty());
    let _g = scope(Some(&d));
    assert_eq!(word_to_phonemes("tomato"), v(&["T", "AH", "M", "EY", "T", "OW"]));
}
