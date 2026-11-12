//! Parity with the Python app: tests/golden/splice.json (selection plans and
//! timelines on a synthetic corpus, seed 20260913).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use ytp_splice::{
    resolve_text, timeline, AudioCache, Clip, GenerationOptions, NoAudio, Pool, PyRandom, Runtime,
};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/splice.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap()
}

fn opt(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn pool_from(g: &Value) -> Pool {
    let settings: Vec<(String, String)> = g["corpus"]["settings"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string().trim_matches('"').to_string()))
        .collect();
    let settings: Vec<(&str, &str)> = settings.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let words = g["corpus"]["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            let clips = w["clips"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| Clip {
                    id: c["id"].as_i64(),
                    source_id: c["source_id"].as_i64().unwrap(),
                    word: c["word"].as_str().unwrap().into(),
                    start_time: f(&c["start_time"]),
                    end_time: f(&c["end_time"]),
                    source_file: c["source_file"].as_str().unwrap().into(),
                    prev_end: opt(&c["prev_end"]),
                    next_start: opt(&c["next_start"]),
                    bleed_risk: false,
                    edited: false,
                    tone: None,
                    phones: None,
                })
                .collect();
            (w["word"].as_str().unwrap().to_string(), clips)
        })
        .collect();
    Pool::from_words("synthetic", &settings, words)
}

fn near(a: &Value, b: Option<f64>, what: &str) {
    match (a.as_f64(), b) {
        (Some(x), Some(y)) => assert!((x - y).abs() < 1e-9, "{what}: {x} vs {y}"),
        (None, None) => {}
        (x, y) => panic!("{what}: {x:?} vs {y:?}"),
    }
}

#[test]
fn splice_plans_match_python() {
    let g = golden();
    let pool = Arc::new(pool_from(&g));
    let given = serde_json::json!({ "phrases": false });
    let opts = GenerationOptions::from_json(&given);
    // The fixture is the Python app's, which still has sing/vocode options;
    // those were removed here pending a rework, so compare the rest.
    let mut expected = g["options"].clone();
    expected
        .as_object_mut()
        .unwrap()
        .retain(|k, _| !ytp_splice::options::is_removed_option(k));
    assert_eq!(opts.to_json(), expected, "options");

    let no_voices = |_: &str| None;
    let audio = AudioCache::new(&NoAudio);
    let rt = Runtime {
        pool: pool.clone(),
        voices: &no_voices,
        audio: &audio,
        glitches: Vec::new(),
    };
    let plans = g["plans"].as_array().unwrap();
    assert_eq!(plans.len(), 5);
    for plan in plans {
        let text = plan["text"].as_str().unwrap();
        let mut rng = PyRandom::from_int(g["seed"].as_u64().unwrap());
        let (segments, report) =
            resolve_text(text, &opts, &rt, &mut rng, &|_, _, _| true).unwrap();

        let units = plan["units"].as_array().unwrap();
        assert_eq!(segments.len(), units.len(), "{text}: segment count");
        for (s, u) in segments.iter().zip(units) {
            let ctx = format!("{text} / {}", u["word"]);
            assert_eq!(u["word"].as_str().unwrap(), s.word, "{ctx}");
            assert_eq!(u["source_file"].as_str().unwrap(), s.source_file, "{ctx}");
            assert_eq!(u["clip_id"].as_i64(), s.id, "{ctx}");
            assert_eq!(u["source_id"].as_i64(), Some(s.source_id), "{ctx}");
            near(&u["start_time"], Some(s.start_time), &format!("{ctx} start"));
            near(&u["end_time"], Some(s.end_time), &format!("{ctx} end"));
            near(&u["span"], Some(s.end_time - s.start_time), &format!("{ctx} span"));
            near(&u["prev_end"], s.prev_end, &format!("{ctx} prev_end"));
            near(&u["next_start"], s.next_start, &format!("{ctx} next_start"));
            near(&u["pause_after"], s.pause_after, &format!("{ctx} pause"));
            assert_eq!(u["reverse"].as_bool().unwrap(), s.reverse, "{ctx} reverse");
            assert_eq!(u["token"].as_u64().map(|t| t as usize), s.tok, "{ctx} token");
        }
        assert_eq!(report.to_json(), plan["report"], "{text}: report");

        let tl = timeline(&segments, &opts, &audio);
        let expect = plan["timeline"].as_array().unwrap();
        assert_eq!(tl.len(), expect.len(), "{text}: timeline length");
        for (t, e) in tl.iter().zip(expect) {
            assert_eq!(e["i"].as_u64().unwrap() as usize, t.i, "{text}");
            near(&e["start"], Some(t.start), &format!("{text} start {}", t.i));
            near(&e["end"], Some(t.end), &format!("{text} end {}", t.i));
        }
    }
}
