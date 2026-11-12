//! Real encodes through ffmpeg on synthetic sources. Skipped when ffmpeg is
//! not on PATH (or YTP_FFMPEG).

use std::path::{Path, PathBuf};
use std::process::Command;

use ytp_encode::{EncodeError, Encoder, Overrides, PresetSet};

fn ffmpeg() -> PathBuf {
    std::env::var("YTP_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| "ffmpeg".into())
}

fn ffprobe() -> PathBuf {
    std::env::var("YTP_FFPROBE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| "ffprobe".into())
}

fn encoder() -> Option<Encoder> {
    let ok = Command::new(ffmpeg())
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipping: ffmpeg is not on PATH");
        return None;
    }
    Some(Encoder::new(ffmpeg()))
}

fn make_source(dir: &Path, name: &str, w: u32, h: u32, fps: u32, seconds: f64, noisy: bool) -> PathBuf {
    let path = dir.join(name);
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={w}x{h}:rate={fps}:duration={seconds}"))
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args(["-t", &seconds.to_string()]);
    if noisy {
        cmd.args(["-vf", "noise=alls=24:allf=t"]);
    }
    cmd.args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "16", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "128k", "-shortest"])
        .arg(&path);
    let out = cmd.output().expect("ffmpeg runs");
    assert!(out.status.success(), "could not make {name}: {}", String::from_utf8_lossy(&out.stderr));
    path
}

fn preset(name: &str) -> ytp_encode::Preset {
    PresetSet::builtin().get(name).unwrap().clone()
}

fn encode_to(enc: &Encoder, input: &Path, preset_name: &str, out: &Path, overrides: Overrides) -> ytp_encode::EncodeOutput {
    let p = preset(preset_name);
    enc.encode(input, &p, overrides, out, &|_| {}, &|| false)
        .unwrap_or_else(|e| panic!("{preset_name}: {e}"))
}

#[test]
fn web_default_makes_a_shareable_mp4() {
    let Some(enc) = encoder() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "in.mp4", 640, 480, 30, 4.0, false);
    let out = dir.path().join("out.mp4");
    let got = encode_to(&enc, &src, "web-default", &out, Overrides::default());
    assert_eq!((got.width, got.height), (640, 480));
    assert!((got.duration - 4.0).abs() < 0.4, "{}", got.duration);
    assert_eq!(got.bytes, std::fs::metadata(&out).unwrap().len());
    let probe = enc.probe(&out).unwrap();
    assert!(probe.has_video && probe.has_audio, "{probe:?}");
    let v = probe.streams.iter().find(|s| s.kind == "video").unwrap();
    assert_eq!(v.codec, "h264");
    let a = probe.streams.iter().find(|s| s.kind == "audio").unwrap();
    assert_eq!(a.codec, "aac");
    eprintln!("web-default: {} bytes for {:.1} s", got.bytes, got.duration);
}

#[test]
fn preview_proxy_is_all_intra_and_480p() {
    let Some(enc) = encoder() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "in.mp4", 1280, 720, 30, 2.0, false);
    let out = dir.path().join("proxy.mp4");
    let got = encode_to(&enc, &src, "preview-proxy", &out, Overrides::default());
    assert_eq!(got.height, 480);
    assert_eq!(got.width, 852);
    let probe = enc.probe(&out).unwrap();
    assert_eq!(probe.height, 480);
    let flags = Command::new(ffprobe())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=flags", "-of", "csv=p=0"])
        .arg(&out)
        .output()
        .unwrap();
    let lines: Vec<&str> = std::str::from_utf8(&flags.stdout).unwrap().lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(lines.len() >= 40, "{} packets", lines.len());
    assert!(lines.iter().all(|l| l.contains('K')), "not every frame is a keyframe: {flags:?}");
}

#[test]
fn max_height_override_scales_a_render_down() {
    let Some(enc) = encoder() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "in.mp4", 1280, 720, 30, 2.0, false);
    let out = dir.path().join("small.mp4");
    let got = encode_to(&enc, &src, "web-default", &out, Overrides::max_height(480));
    assert_eq!((got.width, got.height), (852, 480), "the role quota max_height applies");
}

#[test]
fn a_target_size_lands_under_the_target() {
    let Some(enc) = encoder() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "big.mp4", 1280, 720, 30, 15.0, true);
    let input_bytes = std::fs::metadata(&src).unwrap().len();
    assert!(input_bytes > 10_000_000, "the input is only {input_bytes} bytes; make it bigger");
    let out = dir.path().join("discord.mp4");
    let started = std::time::Instant::now();
    let got = encode_to(&enc, &src, "discord-10mb", &out, Overrides::default());
    eprintln!(
        "discord-10mb: {input_bytes} bytes -> {} bytes in {:.1} s ({} kbps, {}x{})",
        got.bytes,
        started.elapsed().as_secs_f64(),
        got.video_kbps.unwrap_or(0),
        got.width,
        got.height
    );
    assert!(got.two_pass);
    assert!(got.bytes <= 10_000_000, "{} bytes is over the 10 MB target", got.bytes);
    assert!(got.bytes > 1_000_000, "{} bytes is suspiciously small", got.bytes);
    assert!((got.duration - 15.0).abs() < 1.0, "{}", got.duration);
    assert!(got.warnings.is_empty(), "{:?}", got.warnings);
}

#[test]
fn gif_and_audio_only_work() {
    let Some(enc) = encoder() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "in.mp4", 320, 240, 30, 2.0, false);

    let gif = dir.path().join("out.gif");
    let got = encode_to(&enc, &src, "gif", &gif, Overrides::default());
    let bytes = std::fs::read(&gif).unwrap();
    assert!(bytes.starts_with(b"GIF89a") || bytes.starts_with(b"GIF87a"), "not a GIF");
    assert!(got.bytes > 1000);
    let probe = enc.probe(&gif).unwrap();
    assert!(probe.has_video && !probe.has_audio);
    assert!(probe.height <= 480 && probe.fps <= 15.1, "{probe:?}");

    let m4a = dir.path().join("out.m4a");
    let got = encode_to(&enc, &src, "audio-only", &m4a, Overrides::default());
    assert!(got.bytes > 1000);
    let probe = enc.probe(&m4a).unwrap();
    assert!(probe.has_audio && !probe.has_video, "{probe:?}");
    assert!((probe.duration - 2.0).abs() < 0.4, "{}", probe.duration);
}

#[test]
fn cancelling_kills_the_encoder() {
    let Some(enc) = encoder() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(dir.path(), "in.mp4", 1280, 720, 30, 4.0, true);
    let out = dir.path().join("nope.mp4");
    let p = preset("web-default");
    let res = enc.encode(&src, &p, Overrides::default(), &out, &|_| {}, &|| true);
    assert!(matches!(res, Err(EncodeError::Cancelled)), "{res:?}");
    assert!(!out.exists());
}

#[test]
fn the_hardware_report_is_a_list_of_answers() {
    let Some(enc) = encoder() else { return };
    let report = enc.hardware();
    assert!(report.ok, "{report:?}");
    assert!(report.encoders.iter().any(|e| e.encoder == "h264_nvenc"));
    for e in &report.encoders {
        assert!(!e.detail.is_empty(), "{e:?}");
        if !e.available {
            eprintln!("{}: unavailable ({})", e.encoder, e.detail);
        } else {
            eprintln!("{}: available", e.encoder);
        }
    }
}
