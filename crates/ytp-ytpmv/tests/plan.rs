//! The song plan as a project: the notes it lists are the hits the mixer
//! plays. Synth-only, so no corpus or ffmpeg is needed.

use serde_json::json;
use ytp_core::{ClipSource, TrackKind};
use ytp_media::FfmpegRenderer;
use ytp_music::music;
use ytp_ytpmv::mix::{self, plan_song, Request};
use ytp_ytpmv::plan::plan_project;
use ytp_ytpmv::recommend::AnalysisStore;
use ytp_ytpmv::samples::{Banks, Env};
use ytp_ytpmv::settings::Options;
use ytp_ytpmv::{MAX_SECONDS, OUT_H, OUT_W};

fn vlq(mut v: u32) -> Vec<u8> {
    let mut out = vec![(v & 0x7F) as u8];
    v >>= 7;
    while v > 0 {
        out.push(((v & 0x7F) as u8) | 0x80);
        v >>= 7;
    }
    out.reverse();
    out
}

fn track(name: &str, channel: u8, notes: &[(u32, u32, u8)]) -> Vec<u8> {
    let mut ev: Vec<(u32, u8, u8, u8)> = Vec::new();
    for &(t, len, key) in notes {
        ev.push((t, 0x90 | channel, key, 96));
        ev.push((t + len, 0x80 | channel, key, 0));
    }
    ev.sort_by_key(|e| (e.0, e.1 & 0xF0 == 0x90));
    let mut body = vec![0x00, 0xFF, 0x03, name.len() as u8];
    body.extend_from_slice(name.as_bytes());
    let mut last = 0;
    for (t, s, a, b) in ev {
        body.extend(vlq(t - last));
        body.extend_from_slice(&[s, a, b]);
        last = t;
    }
    body.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
    let mut out = b"MTrk".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend(body);
    out
}

fn song_midi() -> Vec<u8> {
    let beat = 480;
    let mut tempo = vec![0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, 0x00, 0xFF, 0x2F, 0x00];
    let mut t0 = b"MTrk".to_vec();
    t0.extend_from_slice(&(tempo.len() as u32).to_be_bytes());
    t0.append(&mut tempo);
    // A chord on every other beat, so one hit has several notes.
    let mut lead: Vec<(u32, u32, u8)> = Vec::new();
    for i in 0..12u32 {
        lead.push((i * beat / 2, beat / 2 - 20, 72 + (i % 4) as u8));
        if i % 2 == 0 {
            lead.push((i * beat / 2, beat / 2 - 20, 79));
        }
    }
    let bass: Vec<(u32, u32, u8)> = (0..6).map(|i| (i * beat, beat - 30, [36, 43][i as usize % 2])).collect();
    let mut drums: Vec<(u32, u32, u8)> = Vec::new();
    for b in 0..8u32 {
        drums.push((b * beat, 60, if b % 2 == 0 { 36 } else { 38 }));
        drums.push((b * beat + beat / 2, 40, 42));
    }
    let tracks = [t0, track("Lead", 0, &lead), track("Bass", 1, &bass), track("Kit", 9, &drums)];
    let mut out = b"MThd".to_vec();
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, tracks.len() as u8, (beat >> 8) as u8, (beat & 0xFF) as u8]);
    for t in tracks {
        out.extend(t);
    }
    out
}

fn check(options: serde_json::Value) {
    let song = music::parse(&song_midi(), "Test").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let renderer = FfmpegRenderer::new("ffmpeg");
    let ffmpeg = std::path::PathBuf::from("ffmpeg");
    let env = Env { ffmpeg: &ffmpeg, renderer: &renderer, samples_dir: dir.path() };
    let store = AnalysisStore::new(dir.path());
    let banks = Banks::new();
    let opts = Options::from_json(&options, MAX_SECONDS);
    let req = Request {
        song: &song,
        parts: &[],
        options: opts,
        default_corpus: "",
        banks: &banks,
        env: &env,
        store: &store,
        audition: None,
        audio_only: false,
        start_seconds: 0.0,
        only_part: None,
        out_size: (OUT_W, OUT_H),
    };
    let progress = |_: &str, _: usize, _: usize| true;
    let prep = mix::prepare(&req, &progress).unwrap();
    let mixed = mix::mix(&prep, &progress).unwrap();
    let planned = plan_song(&prep);
    assert!(!planned.is_empty());
    let mut total_hits = 0;
    for (id, notes) in &planned {
        let hits = &mixed.hits[id];
        let firsts: Vec<_> = notes.iter().filter(|n| n.new_hit).collect();
        assert_eq!(firsts.len(), hits.len(), "part {id}: hits");
        for (n, h) in firsts.iter().zip(hits) {
            assert!((n.start - h.start).abs() < 1e-12, "part {id}: start {} vs {}", n.start, h.start);
            assert!((n.dur - h.dur).abs() < 1e-12, "part {id}: dur");
            assert_eq!(n.take, h.take, "part {id}: take");
            assert_eq!(n.flip, h.flip, "part {id}: flip");
        }
        total_hits += hits.len();
    }
    assert!(total_hits > 10);

    let project = plan_project(&prep, "Test", json!({ "midi_id": "x" })).unwrap();
    project.validate().unwrap();
    assert_eq!(project.generator.as_ref().unwrap().kind, ytp_core::GeneratorKind::Ytpmv);
    assert_eq!(project.tracks.len(), planned.len());
    for (t, (id, notes)) in project.tracks.iter().zip(&planned) {
        assert_eq!(t.part.as_deref(), Some(id.as_str()));
        assert!(t.generated);
        assert_eq!(t.clips.len(), notes.len());
        assert_eq!(t.kind, TrackKind::Audio, "chip parts are audio tracks");
        assert!(t.clips.iter().all(|c| matches!(c.source, ClipSource::Synth { .. })));
    }
    // The chord has two notes at one start, planned as two clips.
    let lead = &project.tracks[0];
    assert!(lead.clips.windows(2).any(|w| w[0].start == w[1].start));
    assert!((project.duration() - (prep.t_end - prep.t_from)).abs() < 0.8 + 1e-6 || project.duration() <= prep.t_end);
}

#[test]
fn planned_notes_are_the_hits_the_mixer_plays() {
    check(json!({ "synth": true, "chip": "nes", "max_seconds": 6 }));
}

#[test]
fn the_random_draws_line_up_with_jitter_and_flip_on() {
    check(json!({ "synth": true, "chip": "gb", "jitter": true, "flip": true, "vary": "random", "max_seconds": 6 }));
}

#[test]
fn speed_and_transpose_are_in_the_plan() {
    check(json!({ "synth": true, "chip": "sms", "speed": 1.5, "transpose": 3, "g_transpose": 2, "max_seconds": 6, "accurate": true }));
}
