//! Preset parsing and validation, target-size maths, and `-progress` parsing
//! from a captured sample. No ffmpeg needed.

use ytp_encode::preset::{parse_document, PresetSet};
use ytp_encode::progress::ProgressParser;
use ytp_encode::target::{fit_even, plan_target_size, target_video_kbps, CONTAINER_MARGIN};

#[test]
fn the_builtin_presets_are_what_the_docs_promise() {
    let set = PresetSet::builtin();
    let names: Vec<&str> = set.names().collect();
    for want in [
        "web-default",
        "discord-10mb",
        "discord-25mb",
        "discord-50mb",
        "matrix",
        "hq-archive",
        "preview-proxy",
        "gif",
        "audio-only",
    ] {
        assert!(names.contains(&want), "missing {want}: {names:?}");
    }
    let web = set.get("web-default").unwrap();
    assert_eq!(web.extension(), "mp4");
    assert_eq!(web.crf, Some(23.0));
    assert!(web.faststart);
    assert_eq!(web.max_height, 1080);
    assert!(!web.hq);

    let hq = set.get("hq-archive").unwrap();
    assert!(hq.hq, "the archive preset needs export.hq");
    assert!(hq.crf.unwrap() <= 18.0);

    let proxy = set.get("preview-proxy").unwrap();
    assert_eq!(proxy.gop, 1, "preview-proxy is all-intra");
    assert_eq!(proxy.max_height, 480);
    assert_eq!(proxy.tune.as_deref(), Some("fastdecode"));

    let gif = set.get("gif").unwrap();
    assert!(gif.is_gif());
    assert_eq!(gif.audio_codec, ytp_encode::AudioCodec::None);
    assert!(gif.fps_cap <= 15.0);

    for mb in [10.0, 25.0, 50.0] {
        let name = format!("discord-{}mb", mb as u32);
        let p = set.get(&name).unwrap();
        assert_eq!(p.target_mb, Some(mb), "{name}");
        assert_eq!(p.quality, ytp_encode::Quality::TargetSizeMb);
    }
    assert_eq!(set.get("matrix").unwrap().target_mb, Some(50.0));
    assert!(set.get("audio-only").unwrap().is_audio_only());
}

#[test]
fn validation_errors_say_what_is_wrong() {
    let cases = [
        (
            "[x]\nquality = \"crf\"\n",
            "needs a crf value",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 60\n",
            "outside 0-51",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\ntarget_mb = 10\n",
            "cannot also set",
        ),
        (
            "[x]\nquality = \"target_size_mb\"\ncrf = 20\n",
            "needs a target_mb",
        ),
        (
            "[x]\nquality = \"bitrate\"\n",
            "needs a bitrate_kbps",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\ncontainer = \"gif\"\n",
            "animated GIF",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\nvideo_codec = \"gif\"\ncontainer = \"gif\"\naudio_codec = \"aac\"\n",
            "cannot carry audio",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\ncontainer = \"ogg\"\n",
            ".ogg",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\naudio_codec = \"none\"\nvideo_codec = \"none\"\n",
            "nothing to encode",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\nspeed = \"warp\"\n",
            "not one of",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\ntune = \"vibes\"\n",
            "not one of",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\nmade_up = true\n",
            "unknown field",
        ),
        (
            "[\"Bad Name\"]\nquality = \"crf\"\ncrf = 20\n",
            "preset name",
        ),
        (
            "[x]\nquality = \"crf\"\ncrf = 20\ngop = -1\n",
            "gop",
        ),
    ];
    for (toml, want) in cases {
        let err = parse_document(toml, "test.toml").unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains(&want.to_lowercase()),
            "{want:?} not in {err:?} for {toml:?}"
        );
    }
}

#[test]
fn admin_presets_add_and_override_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mine.toml");
    std::fs::write(
        &file,
        "[web-default]\ntitle = \"Mine\"\nquality = \"crf\"\ncrf = 20\n\n[phone]\nquality = \"crf\"\ncrf = 28\nmax_height = 720\n",
    )
    .unwrap();
    let set = PresetSet::load_dir(dir.path()).unwrap();
    assert_eq!(set.get("web-default").unwrap().title, "Mine");
    assert_eq!(set.get("web-default").unwrap().crf, Some(20.0));
    assert_eq!(set.get("phone").unwrap().max_height, 720);

    // A broken file is reported but the built-ins and good files survive.
    std::fs::write(dir.path().join("bad.toml"), "[oops\n").unwrap();
    let (set, problems) = PresetSet::with_dir(dir.path());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("bad.toml"), "{problems:?}");
    assert!(set.get("web-default").is_some());
    assert!(set.get("phone").is_some());

    // Nothing there: the built-ins alone.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        PresetSet::load_dir(&empty.path().join("nope")).unwrap().len(),
        PresetSet::builtin().len()
    );
}

#[test]
fn target_size_math_and_resolution_steps() {
    let kbps = target_video_kbps(10_000_000, 60.0, 96, CONTAINER_MARGIN);
    assert!((kbps - 1184.0).abs() < 1.0, "{kbps}");
    assert_eq!(fit_even(1920, 1080, 854, 480), (852, 480));
    let plan = plan_target_size(30.0, 50_000_000, 128, 1920, 1080, 30.0, 1920, 1080, 30.0).unwrap();
    assert_eq!((plan.width, plan.height), (1920, 1080));
    assert!(!plan.scaled);
    let plan = plan_target_size(120.0, 3_000_000, 96, 1920, 1080, 30.0, 1920, 1080, 30.0).unwrap();
    assert!(plan.scaled);
    assert!(plan.width < 1920);
    // too long for the target even without a picture
    assert!(plan_target_size(600.0, 1_000_000, 192, 1920, 1080, 30.0, 1920, 1080, 30.0).is_err());
}

#[test]
fn progress_from_a_captured_ffmpeg_sample() {
    let sample = include_str!("fixtures/progress.txt");
    let duration = 8.0;
    let mut parser = ProgressParser::new(duration);
    let mut seen = Vec::new();
    for line in sample.lines() {
        if let Some(f) = parser.push_line(line) {
            seen.push(f);
        }
    }
    assert!(seen.len() >= 3, "{seen:?}");
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "not monotonic: {seen:?}");
    assert!((seen[0] - 0.22).abs() < 0.01, "{seen:?}");
    assert_eq!(*seen.last().unwrap(), 1.0, "{seen:?}");
    // `progress=end` gives exactly 1 even if the timestamps stop short.
    let mut short = ProgressParser::new(10.0);
    assert_eq!(short.push_line("out_time_us=4000000"), Some(0.4));
    assert_eq!(short.push_line("progress=end"), Some(1.0));
    assert_eq!(short.push_line("progress=continue"), None);
}

#[test]
fn a_preset_can_be_written_to_json_for_the_api() {
    let p = PresetSet::builtin().get("discord-10mb").unwrap();
    let v = p.to_json();
    assert_eq!(v["name"], "discord-10mb");
    assert_eq!(v["quality"], "target_size_mb");
    assert_eq!(v["target_mb"], 10.0);
    assert_eq!(v["hq"], false);
    assert_eq!(v["container"], "mp4");
}
