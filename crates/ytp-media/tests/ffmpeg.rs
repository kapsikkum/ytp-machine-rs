//! Renders real (synthetic) media through ffmpeg. Skipped when ffmpeg is not
//! on PATH (or YTP_FFMPEG).

use std::path::{Path, PathBuf};
use std::process::Command;

use ytp_media::{FfmpegRenderer, RenderJob, Renderer};
use ytp_splice::{total_duration, AudioCache, GenerationOptions, Segment};

fn ffmpeg() -> PathBuf {
    std::env::var("YTP_FFMPEG").map(PathBuf::from).unwrap_or_else(|_| "ffmpeg".into())
}

fn renderer() -> Option<FfmpegRenderer> {
    let r = FfmpegRenderer::new(ffmpeg());
    match r.check() {
        Ok(_) => Some(r),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

fn make_source(dir: &Path, name: &str, size: &str) -> String {
    let path = dir.join(name);
    let ok = Command::new(ffmpeg())
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=size={size}:rate=25"))
        .args(["-f", "lavfi", "-i", "sine=frequency=330:sample_rate=44100", "-t", "12"])
        .args(["-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac"])
        .arg(&path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not make {name}");
    path.to_string_lossy().into_owned()
}

fn seg(src: &str, word: &str, a: f64, b: f64, tok: usize) -> Segment {
    Segment {
        id: Some(tok as i64),
        word: word.into(),
        start_time: a,
        end_time: b,
        source_file: src.into(),
        prev_end: Some(a - 0.2),
        next_start: Some(b + 0.2),
        subtitle: Some(word.into()),
        tok: Some(tok),
        pause_after: Some(0.03),
        ..Default::default()
    }
}

fn duration(path: &Path) -> f64 {
    let out = Command::new(ffmpeg().to_string_lossy().replace("ffmpeg", "ffprobe"))
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(-1.0),
        Err(_) => -1.0,
    }
}

fn render(r: &FfmpegRenderer, segs: &[Segment], opts: &GenerationOptions, captions: bool, out: &Path) {
    let audio = AudioCache::new(r.audio());
    let job = RenderJob { segments: segs, options: opts, captions, out, audio: &audio };
    r.render(&job, &|_, _, _| true).unwrap_or_else(|e| panic!("{e}"));
    let want = total_duration(segs, opts, &audio);
    let got = duration(out);
    if got >= 0.0 {
        assert!((got - want).abs() < 0.25, "duration {got} vs planned {want}");
    }
    assert!(std::fs::metadata(out).unwrap().len() > 1000);
}

#[test]
fn renders_plain_and_foreign_sources() {
    let Some(r) = renderer() else { return };
    let dir = tempfile::tempdir().unwrap();
    let native = make_source(dir.path(), "native.mp4", "480x270");
    let other = make_source(dir.path(), "other.mp4", "320x240");
    let segs = vec![
        seg(&native, "hello", 1.0, 1.4, 0),
        seg(&other, "world", 3.0, 3.5, 1),
        seg(&native, "again", 6.0, 6.4, 2),
    ];
    render(&r, &segs, &GenerationOptions::default(), false, &dir.path().join("a.mp4"));
}

#[test]
fn renders_effects_captions_and_pacing() {
    let Some(r) = renderer() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "native.mp4", "480x270");
    let mut held = seg(&src, "loong", 2.0, 2.5, 0);
    held.stretch = Some(3.0);
    held.subword = true;
    let mut rev = seg(&src, "back", 4.0, 4.4, 1);
    rev.reverse = true;
    rev.pitch = Some(3.0);
    rev.gain_db = Some(6.0);
    rev.glitch = vec!["shake".into(), "earrape".into()];
    let mut run = seg(&src, "so long", 7.0, 7.8, 2);
    run.tok = None;
    run.tokens = vec![(2, 7.0, 7.3), (3, 7.35, 7.8)];
    run.subtitle = None;
    run.subtitle_words = vec![("so".into(), 7.0, 7.3), ("long".into(), 7.35, 7.8)];
    run.pause_after = Some(0.5);
    let segs = vec![held, rev, run];
    let opts = GenerationOptions::from_json(&serde_json::json!({ "speed": 1.25, "pitch": 2.0 }));
    render(&r, &segs, &opts, true, &dir.path().join("b.mp4"));
}

#[test]
fn long_sentences_are_encoded_in_chunks_and_joined() {
    let Some(r) = renderer() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "native.mp4", "480x270");
    let segs: Vec<Segment> = (0..45)
        .map(|k| seg(&src, "w", 0.5 + 0.2 * (k % 40) as f64, 0.6 + 0.2 * (k % 40) as f64, k))
        .collect();
    let seen = std::sync::Mutex::new(Vec::new());
    let audio = AudioCache::new(r.audio());
    let out = dir.path().join("c.mp4");
    let opts = GenerationOptions::default();
    let job = RenderJob { segments: &segs, options: &opts, captions: false, out: &out, audio: &audio };
    r.render(&job, &|stage, done, total| {
        seen.lock().unwrap().push((stage.to_string(), done, total));
        true
    })
    .unwrap();
    let seen = seen.into_inner().unwrap();
    assert!(seen.iter().any(|(s, _, t)| s == "encoding" && *t == 3), "{seen:?}");
    assert!(seen.iter().any(|(s, _, _)| s == "joining"));
    assert!(out.is_file());
    // no leftovers beside the result
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".part") || n.ends_with(".txt"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn cancelling_stops_the_encode_and_leaves_nothing() {
    let Some(r) = renderer() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "native.mp4", "480x270");
    let segs = vec![seg(&src, "a", 1.0, 1.4, 0), seg(&src, "b", 2.0, 2.4, 1)];
    let audio = AudioCache::new(r.audio());
    let out = dir.path().join("d.mp4");
    let opts = GenerationOptions::default();
    let job = RenderJob { segments: &segs, options: &opts, captions: false, out: &out, audio: &audio };
    let res = r.render(&job, &|_, _, _| false);
    assert!(matches!(res, Err(ytp_media::RenderError::Cancelled)));
    assert!(!out.exists());
}

#[test]
fn glitches_listed_are_all_real_filters() {
    let Some(r) = renderer() else { return };
    let g = r.glitches();
    assert!(g.contains(&"flip".to_string()));
}

fn psnr(a: &Path, b: &Path) -> f64 {
    let out = Command::new(ffmpeg())
        .args(["-hide_banner", "-i"])
        .arg(a)
        .arg("-i")
        .arg(b)
        .args(["-lavfi", "psnr", "-f", "null", "-"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    let avg = text.rsplit("average:").next().unwrap_or("").split_whitespace().next().unwrap_or("");
    if avg == "inf" {
        return 99.0;
    }
    avg.parse().unwrap_or_else(|_| panic!("no PSNR in: {text}"))
}

#[test]
fn rendering_from_cut_clips_gives_the_same_video() {
    use ytp_media::ClipCache;
    let Some(plain) = renderer() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "native.mp4", "480x270");
    let other = make_source(dir.path(), "other.mp4", "320x240");
    let mut rev = seg(&other, "back", 4.0, 4.4, 1);
    rev.reverse = true;
    let segs = vec![seg(&src, "hello", 1.0, 1.4, 0), rev, seg(&src, "again", 6.0, 6.4, 2)];
    let opts = GenerationOptions::default();

    let cached = FfmpegRenderer::new(ffmpeg()).with_cache(ClipCache::new(
        dir.path().join("cache"),
        64 << 20,
        ffmpeg(),
    ));
    let cache = cached.cache().unwrap();
    // The first render reads the sources, as without a cache; the cuts and the
    // measurements it made are kept for the next one.
    render(&cached, &segs, &opts, false, &dir.path().join("first.mp4"));
    assert!(cache.wait_idle(std::time::Duration::from_secs(60)));
    let kept = cache.stats();
    assert!(kept.files >= 3, "{kept:?}");

    render(&cached, &segs, &opts, false, &dir.path().join("second.mp4"));
    render(&plain, &segs, &opts, false, &dir.path().join("plain.mp4"));
    let (second, plain_out) = (dir.path().join("second.mp4"), dir.path().join("plain.mp4"));
    let (a, b) = (duration(&second), duration(&plain_out));
    if a >= 0.0 && b >= 0.0 {
        assert!((a - b).abs() < 0.1, "duration {a} vs {b}");
    }
    let db = psnr(&second, &plain_out);
    eprintln!("cached vs uncached video: {db:.1} dB PSNR");
    assert!(db > 30.0, "video differs from the uncached render: {db} dB");
    // nothing new was needed the second time
    assert!(cache.wait_idle(std::time::Duration::from_secs(60)));
    assert_eq!(cache.stats().files, kept.files);
}

#[test]
fn a_cut_cache_never_outgrows_its_cap() {
    use ytp_media::ClipCache;
    let Some(r) = renderer() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "native.mp4", "480x270");
    let cap = 400_000;
    let cache = ClipCache::new(dir.path().join("cache"), cap, ffmpeg());
    for k in 0..6 {
        let (start, read) = (0.5 + k as f64, 0.8);
        assert!(cache.lookup(&src, start, read).is_none(), "a miss reads the source");
        assert!(cache.wait_idle(std::time::Duration::from_secs(60)));
    }
    let s = cache.stats();
    assert!(s.files >= 1 && s.bytes <= cap, "{s:?}");
    // the most recent cut is still there; the oldest went first
    assert!(cache.lookup(&src, 5.5, 0.8).is_some());
    assert!(cache.lookup(&src, 0.5, 0.8).is_none());
    drop(r);
}
