//! Behaviour the Python golden fixtures could not capture (they need a real
//! corpus database or media): phrase runs, phoneme splices, held letters,
//! votes, seeds. Hand-checked against the Python source.

use std::collections::HashMap;
use std::sync::Arc;

use ytp_corpus::data::{ClipRow, CorpusData};
use ytp_splice::phoneme_splice::{find_phoneme_splice, SpliceCtx};
use ytp_splice::{
    resolve_text, seeded_rng, timeline, AudioCache, Clip, GenerationOptions, NoAudio, Pool,
    PyRandom, Runtime,
};

fn row(id: i64, source: i64, word: &str, a: f64, b: f64) -> ClipRow {
    ClipRow {
        id,
        source_id: source,
        word: word.into(),
        start_time: a,
        end_time: b,
        source_file: "downloads/a.mp4".into(),
        edited: false,
        phones: None,
    }
}

fn data(clips: Vec<ClipRow>) -> CorpusData {
    CorpusData {
        name: "test".into(),
        dir: std::env::temp_dir().join("ytp-splice-test-corpus"),
        clips,
        ..Default::default()
    }
}

fn run(pool: Pool, text: &str, given: serde_json::Value, seed: u64) -> (Vec<ytp_splice::Segment>, ytp_splice::Report, GenerationOptions) {
    let pool = Arc::new(pool);
    let opts = GenerationOptions::from_json(&given);
    let no_voices = |_: &str| None;
    let audio = AudioCache::new(&NoAudio);
    let rt = Runtime {
        pool,
        voices: &no_voices,
        audio: &audio,
        glitches: vec!["shake".into(), "flip".into(), "cave".into()],
    };
    let mut rng = PyRandom::from_int(seed);
    let (s, r) = resolve_text(text, &opts, &rt, &mut rng, &|_, _, _| true).unwrap();
    (s, r, opts)
}

#[test]
fn consecutive_words_become_one_run() {
    let d = data(vec![
        row(1, 1, "so", 1.0, 1.3),
        row(2, 1, "long", 1.35, 1.8),
        row(3, 1, "good", 1.85, 2.3),
        row(4, 2, "so", 9.0, 9.3),
        row(5, 2, "good", 9.5, 9.9),
    ]);
    let pool = Pool::build(&d);
    // neighbours are linked in spoken order
    assert_eq!(pool.clips[1].prev_end, Some(1.3));
    assert_eq!(pool.clips[1].next_start, Some(1.85));
    let (segs, report, opts) = run(pool, "so long good", serde_json::json!({}), 1);
    assert_eq!(report.runs, vec!["so long good"]);
    assert_eq!(report.found, vec!["so", "long", "good"]);
    assert!(report.tokens.iter().all(|t| t.status == "run"));
    assert_eq!(segs.len(), 1); // the sentence pause is padding: there is no idle footage
    assert_eq!(segs[0].pause_after, Some(0.5));
    assert_eq!(segs[0].word, "so long good");
    assert_eq!(segs[0].tokens.len(), 3);
    assert_eq!(segs[0].subtitle_words[1].0, "long");
    // run clamps at the last word's content end (no neighbour after it)
    assert_eq!(segs[0].end_time, 2.3);
    let audio = AudioCache::new(&NoAudio);
    let tl = timeline(&segs, &opts, &audio);
    assert_eq!(tl.len(), 3);
    assert!(tl[0].start < tl[1].start && tl[1].start < tl[2].start);
}

#[test]
fn a_run_ends_at_a_full_stop_and_at_a_gap_over_the_limit() {
    let d = data(vec![
        row(1, 1, "so", 1.0, 1.3),
        row(2, 1, "long", 1.35, 1.8),
        row(3, 1, "good", 3.0, 3.4), // 1.2 s later: too far for a run
    ]);
    let (_, report, _) = run(Pool::build(&d), "so long good", serde_json::json!({}), 1);
    assert_eq!(report.runs, vec!["so long"]);
    assert_eq!(report.tokens[2].status, "found");
    let (_, report, _) = run(Pool::build(&d), "so. long good", serde_json::json!({}), 1);
    assert!(report.runs.is_empty());
}

#[test]
fn downvoted_takes_are_vetoed_into_a_splice_or_missing() {
    let mut d = data(vec![row(1, 1, "cat", 1.0, 1.4), row(2, 1, "dog", 5.0, 5.4)]);
    d.ratings = vec![("cat".into(), 1, -2)];
    let (segs, report, _) = run(Pool::build(&d), "cat dog", serde_json::json!({ "phrases": false }), 3);
    // Not played as a recording any more; the splicer may still build it
    // from the only material there is (as the Python app does).
    assert_eq!(report.tokens[0].status, "spliced");
    assert_eq!(report.spliced, vec!["cat"]);
    assert_eq!(report.tokens[1].status, "found");
    assert!(segs.iter().any(|s| s.spliced_from.as_deref() == Some("cat")));
}

fn clip(id: i64, word: &str, start: f64, phones: &[(&str, f64, f64)]) -> Clip {
    let end = start + phones.last().unwrap().2;
    Clip {
        id: Some(id),
        source_id: 1,
        word: word.into(),
        start_time: start,
        end_time: end,
        source_file: "a.mp4".into(),
        prev_end: Some(start - 0.1),
        next_start: Some(end + 0.1),
        bleed_risk: false,
        edited: false,
        tone: None,
        phones: Some(phones.iter().map(|(p, a, b)| (p.to_string(), *a, *b)).collect()),
    }
}

#[test]
fn a_missing_word_is_built_from_pieces_cut_at_phoneme_boundaries() {
    // "cap" = K AE P; the corpus has "cat" (K AE T) and "top" (T AA P).
    let pool = Pool::from_words(
        "t",
        &[],
        vec![
            (
                "cat".into(),
                vec![clip(1, "cat", 10.0, &[("K", 0.0, 0.1), ("AE", 0.1, 0.3), ("T", 0.3, 0.4)])],
            ),
            (
                "top".into(),
                vec![clip(2, "top", 20.0, &[("T", 0.0, 0.1), ("AA", 0.1, 0.25), ("P", 0.25, 0.4)])],
            ),
        ],
    );
    let audio = AudioCache::new(&NoAudio);
    let cx = SpliceCtx { pool: &pool, tone: None, audio: &audio };
    let mut rng = PyRandom::from_int(5);
    let segs = find_phoneme_splice(&cx, &mut rng, "cap", &HashMap::new(), "strict", 8).unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].spliced_from.as_deref(), Some("cat"));
    assert_eq!(segs[0].unit.as_ref().unwrap().phones, vec!["K", "AE"]);
    assert!((segs[0].start_time - 10.0).abs() < 0.021);
    assert!((segs[0].end_time - 10.3).abs() < 1e-9);
    assert!(segs[0].subword);
    assert_eq!(segs[1].spliced_from.as_deref(), Some("top"));
    assert_eq!(segs[1].unit.as_ref().unwrap().phones, vec!["P"]);
    assert!((segs[1].start_time - 20.25).abs() < 1e-9);
    // joins are butted: no fades inside the word, natural edge fades at its ends
    assert_eq!(segs[0].fade_in, None);
    assert_eq!(segs[0].fade_out, Some(0.0));
    assert_eq!(segs[1].fade_in, Some(0.0));
    assert_eq!(segs[1].fade_out, None);
    assert_eq!(segs[0].next_start, Some(segs[0].end_time));
    // the cap on pieces turns it into a miss
    assert!(find_phoneme_splice(&cx, &mut rng, "cap", &HashMap::new(), "strict", 1).is_none());
    // a sound the corpus has nowhere is a miss in strict mode
    assert!(find_phoneme_splice(&cx, &mut rng, "zoo", &HashMap::new(), "strict", 8).is_none());
}

#[test]
fn loose_mode_substitutes_a_near_phoneme_and_says_so() {
    // "cab" = K AE B; no B anywhere, but P is one voicing away.
    let pool = Pool::from_words(
        "t",
        &[],
        vec![
            (
                "cat".into(),
                vec![clip(1, "cat", 10.0, &[("K", 0.0, 0.1), ("AE", 0.1, 0.3), ("T", 0.3, 0.4)])],
            ),
            (
                "top".into(),
                vec![clip(2, "top", 20.0, &[("T", 0.0, 0.1), ("AA", 0.1, 0.25), ("P", 0.25, 0.4)])],
            ),
        ],
    );
    let audio = AudioCache::new(&NoAudio);
    let cx = SpliceCtx { pool: &pool, tone: None, audio: &audio };
    let mut rng = PyRandom::from_int(5);
    assert!(find_phoneme_splice(&cx, &mut rng, "cab", &HashMap::new(), "strict", 8).is_none());
    let segs = find_phoneme_splice(&cx, &mut rng, "cab", &HashMap::new(), "loose", 8).unwrap();
    assert!(segs.iter().all(|s| s.approx));
}

#[test]
fn held_letters_stretch_the_word() {
    let d = data(vec![row(1, 1, "long", 1.0, 1.5), row(2, 1, "ago", 4.0, 4.4)]);
    let (segs, report, opts) = run(
        Pool::build(&d),
        "looooong ago",
        serde_json::json!({ "phrases": false }),
        2,
    );
    assert_eq!(report.tokens[0].word, "long");
    assert_eq!(report.tokens[0].stretched.as_deref(), Some("looooong"));
    assert!(segs.iter().any(|s| s.stretch.is_some_and(|f| f > 1.0)));
    // the held word is on screen for longer than the plain one
    let audio = AudioCache::new(&NoAudio);
    let held = timeline(&segs, &opts, &audio);
    let (plain, _, popts) = run(Pool::build(&d), "long ago", serde_json::json!({ "phrases": false }), 2);
    let plain_tl = timeline(&plain, &popts, &audio);
    assert!(held[0].end - held[0].start > plain_tl[0].end - plain_tl[0].start + 0.1);
}

#[test]
fn same_seed_same_video_and_chaos_changes_it() {
    let rows: Vec<ClipRow> = (0..40)
        .map(|k| row(k + 1, 1 + k % 5, ["a", "b"][(k % 2) as usize], k as f64 * 3.0, k as f64 * 3.0 + 0.3 + 0.01 * k as f64))
        .collect();
    let d = data(rows);
    let given = serde_json::json!({ "phrases": false, "seed": "same" });
    let mut opts = GenerationOptions::from_json(&given);
    let a: Vec<Option<i64>> = {
        let mut r = seeded_rng(&mut opts);
        let (s, _) = {
            let pool = Arc::new(Pool::build(&d));
            let nv = |_: &str| None;
            let audio = AudioCache::new(&NoAudio);
            let rt = Runtime { pool, voices: &nv, audio: &audio, glitches: vec![] };
            resolve_text("a b a b a b a b", &opts, &rt, &mut r, &|_, _, _| true).unwrap()
        };
        s.iter().map(|s| s.id).collect()
    };
    let b: Vec<Option<i64>> = {
        let mut r = seeded_rng(&mut opts);
        let pool = Arc::new(Pool::build(&d));
        let nv = |_: &str| None;
        let audio = AudioCache::new(&NoAudio);
        let rt = Runtime { pool, voices: &nv, audio: &audio, glitches: vec![] };
        let (s, _) = resolve_text("a b a b a b a b", &opts, &rt, &mut r, &|_, _, _| true).unwrap();
        s.iter().map(|s| s.id).collect()
    };
    assert_eq!(a, b);

    let (calm, _, _) = run(Pool::build(&d), "a b a b a b a b", serde_json::json!({ "phrases": false }), 4);
    let (wild, _, _) = run(
        Pool::build(&d),
        "a b a b a b a b",
        serde_json::json!({ "phrases": false, "chaos": 1.0 }),
        4,
    );
    assert!(calm.iter().all(|s| s.glitch.is_empty() && s.pitch.is_none()));
    assert!(wild.iter().any(|s| !s.glitch.is_empty()));
}

#[test]
fn cancelling_from_progress_stops_the_resolve() {
    let d = data(vec![row(1, 1, "hi", 1.0, 1.3)]);
    let pool = Arc::new(Pool::build(&d));
    let nv = |_: &str| None;
    let audio = AudioCache::new(&NoAudio);
    let rt = Runtime { pool, voices: &nv, audio: &audio, glitches: vec![] };
    let mut rng = PyRandom::from_int(1);
    let opts = GenerationOptions::default();
    let r = resolve_text("hi hi hi", &opts, &rt, &mut rng, &|stage, _, _| stage != "resolving");
    assert!(r.is_err());
}

fn corpus_for_recipes() -> Pool {
    Pool::from_words(
        "t",
        &[],
        vec![
            ("cat".into(), vec![clip(1, "cat", 10.0, &[("K", 0.0, 0.1), ("AE", 0.1, 0.3), ("T", 0.3, 0.4)])]),
            ("top".into(), vec![clip(2, "top", 20.0, &[("T", 0.0, 0.1), ("AA", 0.1, 0.25), ("P", 0.25, 0.4)])]),
            ("pop".into(), vec![clip(3, "pop", 30.0, &[("P", 0.0, 0.1), ("AA", 0.1, 0.25), ("P", 0.25, 0.4)])]),
        ],
    )
}

fn splice(pool: &Pool, word: &str) -> Option<Vec<ytp_splice::Segment>> {
    let audio = AudioCache::new(&NoAudio);
    let cx = SpliceCtx { pool, tone: None, audio: &audio };
    let mut rng = PyRandom::from_int(5);
    find_phoneme_splice(&cx, &mut rng, word, &HashMap::new(), "strict", 8)
}

fn recipes_of(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(w, r)| (w.to_string(), r.to_string())).collect()
}

#[test]
fn a_saved_recipe_picks_the_source_the_search_would_not() {
    // Both "top" and "pop" end in P; the plain search takes one, the recipe the other.
    let plain = corpus_for_recipes();
    let by_search = splice(&plain, "cap").unwrap();
    assert_eq!(by_search[1].spliced_from.as_deref(), Some("pop"));

    let mut d = data(Vec::new());
    d.recipes = recipes_of(&[(
        "cap",
        r#"[{"phones":["K","AE"],"from":["cat"]},{"phones":["P"],"from":["top"]}]"#,
    )]);
    let mut pool = corpus_for_recipes();
    pool.recipes = Pool::build(&d).recipes; // read the way a corpus load reads it
    assert_eq!(pool.recipes["cap"].len(), 2);
    let segs = splice(&pool, "cap").unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].spliced_from.as_deref(), Some("cat"));
    assert_eq!(segs[1].spliced_from.as_deref(), Some("top"));
    assert!((segs[1].start_time - 20.25).abs() < 0.03, "{}", segs[1].start_time);
}

#[test]
fn a_recipe_that_does_not_spell_the_word_is_ignored() {
    let mut pool = corpus_for_recipes();
    d_recipes(&mut pool, r#"[{"phones":["K","AE"],"from":["cat"]},{"phones":["B"],"from":["top"]}]"#);
    // K AE B is not K AE P: the search runs as if there were no recipe
    let segs = splice(&pool, "cap").unwrap();
    assert_eq!(segs[1].spliced_from.as_deref(), Some("pop"));
}

fn d_recipes(pool: &mut Pool, json: &str) {
    let mut d = data(Vec::new());
    d.recipes = recipes_of(&[("cap", json)]);
    pool.recipes = Pool::build(&d).recipes;
}

#[test]
fn a_hand_cut_piece_is_played_exactly() {
    let mut pool = corpus_for_recipes();
    d_recipes(
        &mut pool,
        r#"[{"phones":["K","AE"],"from":["cat"]},
            {"phones":["P"],"from":["top"],"clip_id":2,"start":20.30,"end":20.38}]"#,
    );
    let segs = splice(&pool, "cap").unwrap();
    assert_eq!(segs[1].spliced_from.as_deref(), Some("top"));
    assert_eq!((segs[1].start_time, segs[1].end_time), (20.30, 20.38));
    assert!(segs[1].edited && segs[1].cut && segs[1].subword);
    assert_eq!(segs[1].src_start, Some(20.0));
    assert!(!segs[0].edited);

    // times outside the clip, or a clip that is not that word's: cut by the usual rules
    d_recipes(
        &mut pool,
        r#"[{"phones":["K","AE"],"from":["cat"]},
            {"phones":["P"],"from":["top"],"clip_id":2,"start":25.0,"end":25.2}]"#,
    );
    let segs = splice(&pool, "cap").unwrap();
    assert!(!segs[1].edited);
    assert!((segs[1].start_time - 20.25).abs() < 1e-9);
    d_recipes(
        &mut pool,
        r#"[{"phones":["K","AE"],"from":["cat"]},
            {"phones":["P"],"from":["top"],"clip_id":99,"start":20.30,"end":20.38}]"#,
    );
    assert!(!splice(&pool, "cap").unwrap()[1].edited);
}

#[test]
fn a_corpus_pronunciation_changes_what_its_clips_are_believed_to_contain() {
    // CMU says "tomato" is T AH M EY T OW, so "mate" (M EY T) can be cut out
    // of it. This speaker says tom-AH-to: the same cut would give "mart".
    let tomato = clip(
        1,
        "tomato",
        10.0,
        &[("T", 0.0, 0.1), ("AH", 0.1, 0.15), ("M", 0.15, 0.25), ("EY", 0.25, 0.4), ("T", 0.4, 0.5), ("OW", 0.5, 0.7)],
    );
    let pool = Pool::from_words("t", &[], vec![("tomato".into(), vec![tomato.clone()])]);
    let by_cmu = splice(&pool, "mate").expect("CMU's tomato holds M EY T");
    assert_eq!(by_cmu[0].spliced_from.as_deref(), Some("tomato"));

    let mut d = data(Vec::new());
    d.pronunciations = Some("tomato,T AH M AA T OW\n".into());
    let mut rosen = Pool::from_words("t", &[], vec![("tomato".into(), vec![tomato])]);
    rosen.pron = Pool::build(&d).pron;
    assert!(rosen.pron.is_some());
    assert!(splice(&rosen, "mate").is_none(), "no EY left to cut");
    // and the global dictionary is untouched afterwards
    assert!(splice(&pool, "mate").is_some());
}

#[test]
fn the_target_word_uses_the_corpus_pronunciation_too() {
    let pool0 = Pool::from_words(
        "t",
        &[],
        vec![
            ("cat".into(), vec![clip(1, "cat", 10.0, &[("K", 0.0, 0.1), ("AE", 0.1, 0.3), ("T", 0.3, 0.4)])]),
            ("top".into(), vec![clip(2, "top", 20.0, &[("T", 0.0, 0.1), ("AA", 0.1, 0.25), ("P", 0.25, 0.4)])]),
        ],
    );
    // "cap" is K AE P; this corpus insists on "cat" -> K AE T, a word it already has
    let mut d = data(Vec::new());
    d.pronunciations = Some("cap,K AE T\n".into());
    let mut pool = pool0;
    pool.pron = Pool::build(&d).pron;
    let segs = splice(&pool, "cap").unwrap();
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].unit.as_ref().unwrap().phones, vec!["K", "AE", "T"]);
}
