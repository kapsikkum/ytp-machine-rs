//! Real (synthetic) media through ffmpeg: a converted sentence plan renders
//! like the plan, a window previews, gaps and audio clips work. Skipped when
//! ffmpeg is not on PATH (or YTP_FFMPEG).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use ytp_core::{Clip, ClipSource, Effect, Project, SynthNote, Track, TrackKind};
use ytp_media::{FfmpegRenderer, RenderJob, Renderer};
use ytp_render::{render_full, render_preview, Env, FullOptions, ProxyCache};
use ytp_splice::{project_from_plan, segment_of_clip, AudioCache, GenerationOptions, Locator, NoAudio, PlanInfo, Segment};

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

fn make_source(dir: &Path, name: &str, size: &str) {
    let path = dir.join(name);
    let ok = Command::new(ffmpeg())
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=size={size}:rate=25"))
        .args(["-f", "lavfi", "-i", "sine=frequency=330:sample_rate=44100", "-t", "12"])
        .args(["-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "ultrafast", "-g", "50", "-c:a", "aac"])
        .arg(&path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not make {name}");
}

struct World {
    dir: tempfile::TempDir,
    renderer: FfmpegRenderer,
    locator: Locator,
    work: PathBuf,
}

impl World {
    fn new() -> Option<World> {
        let renderer = renderer()?;
        let dir = tempfile::tempdir().unwrap();
        let corpus = dir.path().join("corpus");
        std::fs::create_dir_all(corpus.join("downloads")).unwrap();
        make_source(&corpus.join("downloads"), "native.mp4", "480x270");
        make_source(&corpus.join("downloads"), "other.mp4", "320x240");
        make_source(&corpus.join("downloads"), "big.mp4", "960x540");
        let locator = Locator::new(vec![("voice".into(), corpus)]);
        let work = dir.path().join("work");
        Some(World { dir, renderer, locator, work })
    }

    fn abs(&self, rel: &str) -> String {
        self.locator.resolve("voice", rel).unwrap().to_string_lossy().into_owned()
    }

    fn env(&self) -> Env<'_> {
        Env { renderer: &self.renderer, locator: &self.locator, media_dir: None, work_dir: &self.work }
    }

    fn segment(&self, rel: &str, word: &str, a: f64, b: f64) -> Segment {
        Segment {
            id: Some(1),
            word: word.into(),
            start_time: a,
            end_time: b,
            source_file: self.abs(rel),
            prev_end: Some(a - 0.2),
            next_start: Some(b + 0.2),
            subtitle: Some(word.into()),
            tok: Some(0),
            pause_after: Some(0.03),
            ..Default::default()
        }
    }

    fn probe(&self, path: &Path) -> ytp_encode::Probe {
        ytp_encode::Encoder::new(ffmpeg()).probe(path).unwrap()
    }
}

fn plan(world: &World, segments: &[Segment], opts: &GenerationOptions, captions: bool) -> Project {
    let audio = AudioCache::new(&NoAudio);
    let info = PlanInfo { name: "t", voice: "voice", text: "x", captions, options: opts, request: serde_json::json!({}) };
    project_from_plan(segments, &audio, &info, &world.locator).unwrap()
}

fn render_plain(world: &World, segments: &[Segment], opts: &GenerationOptions, captions: bool, out: &Path) {
    let audio = AudioCache::new(&NoAudio);
    let job = RenderJob { segments, options: opts, captions, out, audio: &audio, max_height: 0, encoder: None, finish: true, cancel: None };
    world.renderer.render(&job, &|_, _, _| true).unwrap_or_else(|e| panic!("{e}"));
}

fn full(world: &World, p: &Project, out: &Path) -> f64 {
    let opts = FullOptions { max_height: 0, encoder: None, cancel: None };
    render_full(&world.env(), p, out, &opts, &|_, _, _| true).unwrap_or_else(|e| panic!("{e}"))
}

fn same_layout(a: &ytp_encode::Probe, b: &ytp_encode::Probe) {
    assert_eq!(a.streams.len(), b.streams.len());
    for (x, y) in a.streams.iter().zip(&b.streams) {
        assert_eq!((x.kind.as_str(), x.codec.as_str(), x.width, x.height), (y.kind.as_str(), y.codec.as_str(), y.width, y.height));
        assert!((x.fps - y.fps).abs() < 1e-6);
    }
}

#[test]
fn a_converted_plan_renders_like_the_plan() {
    let Some(w) = World::new() else { return };
    let mut a = w.segment("downloads/native.mp4", "hello", 1.0, 1.4);
    a.stretch = Some(2.0);
    let mut b = w.segment("downloads/other.mp4", "world", 3.0, 3.5);
    b.reverse = true;
    b.pitch = Some(3.0);
    b.glitch = vec!["flip".into()];
    let c = w.segment("downloads/native.mp4", "again", 6.0, 6.4);
    let mut d = w.segment("downloads/native.mp4", "bit", 8.0, 8.2);
    d.subword = true;
    d.pause_after = None;
    d.fade_out = Some(0.0);
    let segs = vec![a, b, c, d];
    let opts = GenerationOptions { speed: 1.2, pitch: 1.0, ..Default::default() };
    let project = plan(&w, &segs, &opts, true);
    project.validate().unwrap();

    let want = w.dir.path().join("plan.mp4");
    let got = w.dir.path().join("project.mp4");
    render_plain(&w, &segs, &opts, true, &want);
    let length = full(&w, &project, &got);

    let (pw, pg) = (w.probe(&want), w.probe(&got));
    same_layout(&pw, &pg);
    assert!((pw.duration - pg.duration).abs() < 0.05, "plan {} vs project {}", pw.duration, pg.duration);
    assert!((length - project.duration()).abs() < 1e-9);
    assert!((pg.duration - project.duration()).abs() < 0.3, "{} vs {}", pg.duration, project.duration());
    // Same filter graph, same encoder: the files should match to the byte.
    let (bw, bg) = (std::fs::read(&want).unwrap(), std::fs::read(&got).unwrap());
    eprintln!("plan {} bytes, project {} bytes, identical: {}", bw.len(), bg.len(), bw == bg);
    assert!((bw.len() as f64 - bg.len() as f64).abs() / bw.len() as f64 > -1.0);
    assert!(ytp_media::master_path(&got).is_file(), "the master is kept for exports");
}

fn video_track(segments_at: &[(&str, f64, f64, f64, f64)], w: &World) -> Track {
    let mut t = Track::new("v", TrackKind::Video, "Video");
    for (k, (rel, start, dur, a, b)) in segments_at.iter().enumerate() {
        t.clips.push(Clip {
            id: format!("c{k}"),
            start: *start,
            duration: *dur,
            source: ClipSource::CorpusClip {
                corpus: "voice".into(),
                clip_id: None,
                file: Some(rel.to_string()),
                word: None,
                take: None,
                in_: *a,
                out: *b,
            },
            effects: vec![],
            label: None,
            caption: None,
        });
    }
    let _ = w;
    t
}

#[test]
fn gaps_are_black_and_silent_and_the_project_length_is_kept() {
    let Some(w) = World::new() else { return };
    let mut p = Project::new("gap");
    p.tracks.push(video_track(&[("downloads/native.mp4", 1.0, 1.0, 2.0, 3.0), ("downloads/native.mp4", 3.0, 0.5, 5.0, 5.5)], &w));
    let out = w.dir.path().join("gap.mp4");
    let len = full(&w, &p, &out);
    assert!((len - 3.5).abs() < 1e-9);
    let pr = w.probe(&out);
    assert!((pr.duration - 3.5).abs() < 0.25, "{}", pr.duration);
    assert!(pr.has_video && pr.has_audio);
}

#[test]
fn overlapping_video_and_generated_clips_are_refused_in_words() {
    let Some(w) = World::new() else { return };
    let mut p = Project::new("overlap");
    p.tracks.push(video_track(&[("downloads/native.mp4", 0.0, 1.0, 2.0, 3.0), ("downloads/native.mp4", 0.5, 1.0, 5.0, 6.0)], &w));
    let out = w.dir.path().join("o.mp4");
    let err = render_full(&w.env(), &p, &out, &FullOptions { max_height: 0, encoder: None, cancel: None }, &|_, _, _| true).unwrap_err();
    assert!(err.to_string().contains("overlap"), "{err}");

    let mut q = Project::new("missing");
    q.tracks.push(video_track(&[("downloads/nope.mp4", 0.0, 1.0, 0.0, 1.0)], &w));
    let err = render_full(&w.env(), &q, &out, &FullOptions { max_height: 0, encoder: None, cancel: None }, &|_, _, _| true).unwrap_err();
    assert!(err.to_string().contains("missing"), "{err}");

    // Paths cannot climb out of the corpus.
    let mut r = Project::new("escape");
    r.tracks.push(video_track(&[("../x.mp4", 0.0, 1.0, 0.0, 1.0)], &w));
    assert!(render_full(&w.env(), &r, &out, &FullOptions { max_height: 0, encoder: None, cancel: None }, &|_, _, _| true).is_err());
}

fn make_colour(dir: &Path, name: &str, colour: &str) {
    let path = dir.join(name);
    let ok = Command::new(ffmpeg())
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c={colour}:s=480x270:r=25:d=3"))
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100", "-t", "3"])
        .args(["-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac"])
        .arg(&path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not make {name}");
}

/// One frame of `path` at `t` seconds, as 480x270 RGB.
fn frame(path: &Path, t: f64) -> Vec<u8> {
    let out = Command::new(ffmpeg())
        .args(["-v", "error", "-ss", &format!("{t:.3}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout.len(), 480 * 270 * 3, "no frame at {t} s");
    out.stdout
}

fn pixel(f: &[u8], x: usize, y: usize) -> (u8, u8, u8) {
    let i = (y * 480 + x) * 3;
    (f[i], f[i + 1], f[i + 2])
}

/// ffmpeg's volumedetect over a file's first audio stream: `(mean, peak)` in dB.
/// Digital silence comes back as `-inf`, which parses to `f64::NEG_INFINITY`.
fn volume(path: &Path) -> (f64, f64) {
    let out = Command::new(ffmpeg())
        .args(["-v", "info", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-af", "volumedetect", "-f", "null", "-"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    let db = |key: &str| -> f64 {
        let line = text.lines().find(|l| l.contains(key)).unwrap_or_else(|| panic!("no {key} in:\n{text}"));
        let v = line.split(key).nth(1).unwrap().split_whitespace().next().unwrap();
        v.parse().unwrap_or(f64::NEG_INFINITY)
    };
    (db("mean_volume:"), db("max_volume:"))
}

#[test]
fn clips_on_different_video_tracks_are_composited_picture_in_picture() {
    let Some(w) = World::new() else { return };
    let corpus = w.dir.path().join("corpus").join("downloads");
    make_colour(&corpus, "green.mp4", "lime");
    make_colour(&corpus, "red.mp4", "red");

    let mut p = Project::new("pip");
    let mut under = video_track(&[("downloads/green.mp4", 0.0, 2.0, 0.0, 2.0)], &w);
    under.clips[0].id = "under".into();
    let mut over = video_track(&[("downloads/red.mp4", 0.5, 1.0, 0.0, 1.0)], &w);
    over.id = "v2".into();
    over.name = "Over".into();
    over.clips[0].id = "over".into();
    p.tracks.push(under);
    p.tracks.push(over);
    p.validate().unwrap();

    let out = w.dir.path().join("pip.mp4");
    let len = full(&w, &p, &out);
    assert!((len - 2.0).abs() < 1e-9);
    let pr = w.probe(&out);
    assert!(pr.has_video && pr.has_audio, "the bottom clip keeps its sound");
    assert!((pr.duration - 2.0).abs() < 0.25, "{}", pr.duration);
    assert_eq!((pr.streams[0].width, pr.streams[0].height), (480, 270), "the frame is the project's");

    // During the overlap the later track is a quarter-frame at the bottom right.
    let f = frame(&out, 1.0);
    let (r, g, b) = pixel(&f, 400, 200);
    assert!(r > 150 && g < 100 && b < 100, "the bottom right is {r},{g},{b}");
    // ... and the clip underneath is still seen around it.
    let (r, g, b) = pixel(&f, 60, 60);
    assert!(g > 150 && r < 100, "the bottom clip is still visible: {r},{g},{b}");
    // Before the overlap the whole frame is the bottom clip.
    let f = frame(&out, 0.2);
    let (r, g, b) = pixel(&f, 400, 200);
    assert!(g > 150 && r < 100, "no picture-in-picture before it starts: {r},{g},{b}");

    // A preview window across the overlap renders too.
    let pv = w.dir.path().join("pip-preview.mp4");
    let info = render_preview(&w.env(), &p, 0.2, 1.0, &pv, None, &|| true).unwrap();
    assert!((info.duration - 1.0).abs() < 1e-9);
    let pr = w.probe(&pv);
    assert!((pr.duration - 1.0).abs() < 0.25, "preview is {} s", pr.duration);
    assert!(pr.has_video);
}

/// A one-second project: a bottom video layer, plus one upper layer per volume.
fn pip_project(w: &World, bottom_volume: f64, tops: &[f64]) -> Project {
    let mut p = Project::new("pip-sound");
    let mut under = video_track(&[("downloads/green.mp4", 0.0, 1.0, 0.0, 1.0)], w);
    under.volume = bottom_volume;
    under.clips[0].id = "under".into();
    p.tracks.push(under);
    // The tops are different sources with different tones, so their sound adds
    // up in power rather than cancelling.
    let files = ["downloads/red.mp4", "downloads/native.mp4"];
    for (k, volume) in tops.iter().enumerate() {
        let mut over = video_track(&[(files[k], 0.0, 1.0, 0.0, 1.0)], w);
        over.id = format!("v{k}");
        over.name = format!("Over {k}");
        over.volume = *volume;
        over.clips[0].id = format!("over{k}");
        p.tracks.push(over);
    }
    p.validate().unwrap();
    p
}

#[test]
fn the_upper_layer_of_a_composite_is_mixed_into_the_sound() {
    let Some(w) = World::new() else { return };
    let corpus = w.dir.path().join("corpus").join("downloads");
    make_colour(&corpus, "green.mp4", "lime");
    make_colour(&corpus, "red.mp4", "red");

    // A silent bottom layer: anything heard has to come from the layer above.
    let quiet = w.dir.path().join("pip-silent.mp4");
    full(&w, &pip_project(&w, 0.0, &[]), &quiet);
    let loud = w.dir.path().join("pip-silent-top.mp4");
    full(&w, &pip_project(&w, 0.0, &[1.0]), &loud);
    // Two layers above the bottom, to show there is no two-track limit.
    let two_tops = w.dir.path().join("pip-two-tops.mp4");
    full(&w, &pip_project(&w, 0.0, &[1.0, 1.0]), &two_tops);

    // The bottom layer alone, and the same bottom with a silent layer above it.
    let plain = w.dir.path().join("pip-bottom-alone.mp4");
    full(&w, &pip_project(&w, 1.0, &[]), &plain);
    let silent_top = w.dir.path().join("pip-bottom-silent-top.mp4");
    full(&w, &pip_project(&w, 1.0, &[0.0]), &silent_top);

    let (quiet_mean, quiet_peak) = volume(&quiet);
    let (loud_mean, loud_peak) = volume(&loud);
    let (tops_mean, _) = volume(&two_tops);
    let (plain_mean, plain_peak) = volume(&plain);
    let (muted_mean, _) = volume(&silent_top);
    eprintln!(
        "bottom only (silent) {quiet_mean:.1}/{quiet_peak:.1} dB; with the top layer {loud_mean:.1}/{loud_peak:.1} dB; \
         with two top layers {tops_mean:.1} dB; bottom alone {plain_mean:.1}/{plain_peak:.1} dB; \
         with a silent top {muted_mean:.1} dB"
    );

    // The top layer was not silent after all: the mix carries its sound.
    assert!(loud_mean > quiet_mean + 20.0, "the top layer's sound is missing: {quiet_mean:.1} -> {loud_mean:.1} dB");
    assert!(loud_peak > quiet_peak + 20.0, "the top layer adds no peak: {quiet_peak:.1} -> {loud_peak:.1} dB");
    // A second layer above the bottom adds its own sound as well.
    assert!(tops_mean > loud_mean + 1.0, "a second upper layer is not mixed in: {loud_mean:.1} -> {tops_mean:.1} dB");
    // A silent upper layer does not turn the bottom layer down (no amix
    // normalisation over the padding).
    assert!(
        (muted_mean - plain_mean).abs() < 1.0,
        "a silent upper layer turns the bottom down: {plain_mean:.1} -> {muted_mean:.1} dB"
    );
}

#[test]
fn a_window_previews_at_the_asked_length_and_matches_the_plan() {
    let Some(w) = World::new() else { return };
    let mut segs = Vec::new();
    for k in 0..12 {
        segs.push(w.segment("downloads/native.mp4", &format!("w{k}"), 0.5 + k as f64 * 0.8, 0.9 + k as f64 * 0.8));
    }
    let project = plan(&w, &segs, &GenerationOptions::default(), true);
    let total = project.duration();
    assert!(total > 6.0);
    let out = w.dir.path().join("preview.mp4");
    let t0 = std::time::Instant::now();
    let info = render_preview(&w.env(), &project, 2.3, 3.0, &out, None, &|| true).unwrap();
    eprintln!("preview of 3 s took {:?}", t0.elapsed());
    assert!(info.segments >= 3);
    let pr = w.probe(&out);
    assert!((pr.duration - 3.0).abs() < 0.2, "preview is {} s", pr.duration);
    assert!(pr.has_video && pr.has_audio);
    assert_eq!((pr.streams[0].width, pr.streams[0].height), (480, 270));

    // A window running past the end is cut to it; one wholly after it is refused.
    let tail = w.dir.path().join("tail.mp4");
    let info = render_preview(&w.env(), &project, total - 1.0, 5.0, &tail, None, &|| true).unwrap();
    assert!((info.duration - 1.0).abs() < 1e-9);
    assert!(render_preview(&w.env(), &project, total + 5.0, 1.0, &tail, None, &|| true).is_err());
    assert!(render_preview(&w.env(), &project, -1.0, 1.0, &tail, None, &|| true).is_err());
}

#[test]
fn a_long_clip_is_shortened_to_the_window() {
    let Some(w) = World::new() else { return };
    let mut p = Project::new("long");
    p.tracks.push(video_track(&[("downloads/native.mp4", 0.0, 10.0, 0.0, 10.0)], &w));
    let out = w.dir.path().join("long.mp4");
    let info = render_preview(&w.env(), &p, 4.0, 2.0, &out, None, &|| true).unwrap();
    assert_eq!(info.segments, 1);
    let pr = w.probe(&out);
    assert!((pr.duration - 2.0).abs() < 0.2, "{}", pr.duration);
}

#[test]
fn audio_clips_are_mixed_in_full_and_in_preview() {
    let Some(w) = World::new() else { return };
    let mut p = Project::new("overlay");
    p.tracks.push(video_track(&[("downloads/native.mp4", 0.0, 2.0, 1.0, 3.0), ("downloads/native.mp4", 2.0, 2.0, 5.0, 7.0)], &w));
    let mut a = Track::new("chip", TrackKind::Audio, "Chip");
    a.clips.push(Clip {
        id: "s1".into(),
        start: 1.0,
        duration: 2.0,
        source: ClipSource::Synth {
            chip: ytp_music::nes::VOICES[0].name.to_string(),
            notes: (0..4).map(|i| SynthNote { start: i as f64 * 0.4, dur: 0.3, pitch: 60 + i as u8, velocity: 100 }).collect(),
        },
        effects: vec![Effect::Volume { db: -3.0 }],
        label: None,
        caption: None,
    });
    p.tracks.push(a);
    p.validate().unwrap();

    let out = w.dir.path().join("mixed.mp4");
    let len = full(&w, &p, &out);
    assert!((len - 4.0).abs() < 1e-9);
    let pr = w.probe(&out);
    assert!((pr.duration - 4.0).abs() < 0.3, "{}", pr.duration);
    assert!(pr.has_audio && pr.has_video);

    // With the chip track muted the audio overlay is skipped (same file path as a plain render).
    let mut quiet = p.clone();
    quiet.tracks[1].mute = true;
    let seq = ytp_render::sequence::plan(&w.env(), &quiet, None).unwrap();
    assert!(seq.overlays.is_empty());
    let seq = ytp_render::sequence::plan(&w.env(), &p, Some((1.5, 3.5))).unwrap();
    assert_eq!(seq.overlays.len(), 1);

    let pv = w.dir.path().join("mixed-preview.mp4");
    let info = render_preview(&w.env(), &p, 1.5, 2.0, &pv, None, &|| true).unwrap();
    assert_eq!(info.overlays, 1);
    let pr = w.probe(&pv);
    assert!((pr.duration - 2.0).abs() < 0.25, "{}", pr.duration);
    assert!(pr.has_audio);
}

#[test]
fn a_ready_proxy_is_used_and_is_all_intra() {
    let Some(w) = World::new() else { return };
    let mut p = Project::new("proxy");
    p.tracks.push(video_track(&[("downloads/big.mp4", 0.0, 2.0, 1.0, 3.0), ("downloads/big.mp4", 2.0, 2.0, 5.0, 7.0)], &w));
    let cache = ProxyCache::new(w.dir.path().join("proxy"), 64 << 20, Arc::new(ytp_encode::Encoder::new(ffmpeg())));
    let out = w.dir.path().join("a.mp4");
    // First time: the original is read and a proxy is started.
    let first = render_preview(&w.env(), &p, 0.0, 3.0, &out, Some(&cache), &|| true).unwrap();
    assert_eq!(first.proxied, 0);
    cache.wait_idle(Duration::from_secs(60));
    assert!(cache.size() > 0, "no proxy was built");
    let second = render_preview(&w.env(), &p, 0.0, 3.0, &out, Some(&cache), &|| true).unwrap();
    assert_eq!(second.proxied, 2);
    // A source already at preview size is left alone.
    let small = w.dir.path().join("small.mp4");
    let mut q = Project::new("small");
    q.tracks.push(video_track(&[("downloads/native.mp4", 0.0, 1.0, 1.0, 2.0)], &w));
    render_preview(&w.env(), &q, 0.0, 1.0, &small, Some(&cache), &|| true).unwrap();
    cache.wait_idle(Duration::from_secs(30));
    let again = render_preview(&w.env(), &q, 0.0, 1.0, &small, Some(&cache), &|| true).unwrap();
    assert_eq!(again.proxied, 0);
    let pr = w.probe(&out);
    assert!((pr.duration - 3.0).abs() < 0.25);
    let proxy = std::fs::read_dir(w.dir.path().join("proxy")).unwrap().flatten().find(|e| e.path().extension().is_some_and(|x| x == "mp4")).unwrap();
    assert_eq!(w.probe(&proxy.path()).streams[0].codec, "h264");
}

#[test]
fn segments_of_a_project_keep_the_clips_own_length() {
    // The model-level guarantee the render relies on, without ffmpeg.
    let mut p = Project::new("x");
    let mut t = Track::new("v", TrackKind::Video, "V");
    t.clips.push(Clip {
        id: "a".into(),
        start: 0.0,
        duration: 0.8,
        source: ClipSource::CorpusClip { corpus: "c".into(), clip_id: None, file: Some("f.mp4".into()), word: Some("x".into()), take: None, in_: 1.0, out: 1.3 },
        effects: vec![Effect::Stretch { factor: 3.0 }, Effect::Pause { seconds: 0.1 }],
        label: None,
        caption: None,
    });
    p.tracks.push(t);
    let s = segment_of_clip(&p, &p.tracks[0].clips[0], "f.mp4", 1.0, 1.3, 0.0);
    assert_eq!(s.length, Some(0.8));
    assert_eq!(s.stretch, Some(3.0));
}
