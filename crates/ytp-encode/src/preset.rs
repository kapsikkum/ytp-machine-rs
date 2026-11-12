//! Encode presets as data: the built-in TOML, admin presets from
//! `<data>/presets/*.toml`, and validation with readable errors.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The built-in presets, compiled in.
const BUILTIN_TOML: &str = include_str!("../presets.toml");

const X264_SPEEDS: &[&str] = &[
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
    "placebo",
];

const TUNES: &[&str] = &[
    "film",
    "animation",
    "grain",
    "stillimage",
    "fastdecode",
    "zerolatency",
    "psnr",
    "ssim",
];

#[derive(Debug, Error)]
pub enum PresetError {
    #[error("{0}")]
    Invalid(String),
    #[error("cannot read {path}: {error}")]
    Io { path: String, error: String },
    #[error("{origin}: {message}")]
    Toml { origin: String, message: String },
}

impl PresetError {
    fn invalid(message: impl Into<String>) -> Self {
        PresetError::Invalid(message.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    Mp4,
    Mkv,
    Gif,
    M4a,
    Ogg,
}

impl Container {
    pub fn extension(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mkv => "mkv",
            Container::Gif => "gif",
            Container::M4a => "m4a",
            Container::Ogg => "ogg",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Container::Mp4 => "video/mp4",
            Container::Mkv => "video/x-matroska",
            Container::Gif => "image/gif",
            Container::M4a => "audio/mp4",
            Container::Ogg => "audio/ogg",
        }
    }
}

impl fmt::Display for Container {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    H265,
    Gif,
    None,
}

impl VideoCodec {
    pub fn ffmpeg_name(self) -> Option<&'static str> {
        match self {
            VideoCodec::H264 => Some("libx264"),
            VideoCodec::H265 => Some("libx265"),
            VideoCodec::Gif => Some("gif"),
            VideoCodec::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    Aac,
    Opus,
    None,
}

impl AudioCodec {
    pub fn ffmpeg_name(self) -> Option<&'static str> {
        match self {
            AudioCodec::Aac => Some("aac"),
            AudioCodec::Opus => Some("libopus"),
            AudioCodec::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Constant quality (`-crf`).
    Crf,
    /// Fit under a size in decimal megabytes, with a two-pass encode.
    TargetSizeMb,
    /// A fixed bitrate.
    Bitrate,
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Quality::Crf => "crf",
            Quality::TargetSizeMb => "target_size_mb",
            Quality::Bitrate => "bitrate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HwMode {
    /// Use a working hardware encoder when the codec has one.
    Auto,
    /// Always libx264/libx265.
    Never,
}

/// One validated encode preset.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preset {
    pub name: String,
    pub title: String,
    pub container: Container,
    pub video_codec: VideoCodec,
    pub audio_codec: AudioCodec,
    pub quality: Quality,
    pub crf: Option<f64>,
    pub target_mb: Option<f64>,
    pub bitrate_kbps: Option<u32>,
    pub audio_kbps: u32,
    pub speed: String,
    pub max_width: u32,
    pub max_height: u32,
    pub fps_cap: f64,
    /// Keyframe interval; 1 means all-intra, 0 the codec's default.
    pub gop: u32,
    pub tune: Option<String>,
    pub hw: HwMode,
    pub faststart: bool,
    /// Needs `export.hq`.
    pub hq: bool,
    pub extra_args: Vec<String>,
}

impl Preset {
    pub fn extension(&self) -> &'static str {
        self.container.extension()
    }

    pub fn mime(&self) -> &'static str {
        self.container.mime()
    }

    pub fn is_audio_only(&self) -> bool {
        self.video_codec == VideoCodec::None
    }

    pub fn is_gif(&self) -> bool {
        self.video_codec == VideoCodec::Gif
    }

    /// Target size in bytes, when this preset is a target-size preset.
    pub fn target_bytes(&self) -> Option<u64> {
        self.target_mb.map(|mb| (mb * 1_000_000.0).round() as u64)
    }

    /// What the preset is about, for the API.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "title": self.title,
            "container": self.container.extension(),
            "video_codec": self.video_codec,
            "audio_codec": self.audio_codec,
            "quality": self.quality.label(),
            "crf": self.crf,
            "target_mb": self.target_mb,
            "bitrate_kbps": self.bitrate_kbps,
            "audio_kbps": self.audio_kbps,
            "max_width": self.max_width,
            "max_height": self.max_height,
            "fps_cap": self.fps_cap,
            "gop": self.gop,
            "hw": self.hw,
            "hq": self.hq,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPreset {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    container: Option<Container>,
    #[serde(default)]
    video_codec: Option<VideoCodec>,
    #[serde(default)]
    audio_codec: Option<AudioCodec>,
    quality: Quality,
    #[serde(default)]
    crf: Option<f64>,
    #[serde(default)]
    target_mb: Option<f64>,
    #[serde(default)]
    bitrate_kbps: Option<u32>,
    #[serde(default = "default_audio_kbps")]
    audio_kbps: u32,
    #[serde(default = "default_speed")]
    speed: String,
    #[serde(default)]
    max_width: u32,
    #[serde(default)]
    max_height: u32,
    #[serde(default)]
    fps_cap: f64,
    #[serde(default)]
    gop: u32,
    #[serde(default)]
    tune: Option<String>,
    #[serde(default = "default_hw")]
    hw: HwMode,
    #[serde(default = "default_true")]
    faststart: bool,
    #[serde(default)]
    hq: bool,
    #[serde(default)]
    extra_args: Vec<String>,
}

fn default_audio_kbps() -> u32 {
    128
}

fn default_speed() -> String {
    "medium".to_string()
}

fn default_hw() -> HwMode {
    HwMode::Never
}

fn default_true() -> bool {
    true
}

fn validate_name(name: &str) -> Result<(), PresetError> {
    if name.is_empty() {
        return Err(PresetError::invalid("a preset needs a name"));
    }
    if name.len() > 64 {
        return Err(PresetError::invalid(format!(
            "preset name {name:?} is longer than 64 characters"
        )));
    }
    let ok = name.bytes().enumerate().all(|(i, b)| {
        b.is_ascii_lowercase()
            || b.is_ascii_digit()
            || (i > 0 && matches!(b, b'-' | b'_' | b'.'))
    });
    if !ok {
        return Err(PresetError::invalid(format!(
            "preset name {name:?} must be lower-case letters, digits, '-', '_' or '.', starting with a letter or digit"
        )));
    }
    Ok(())
}

/// Check the values and fill the defaults that depend on the codecs.
fn build(name: String, raw: RawPreset) -> Result<Preset, PresetError> {
    validate_name(&name)?;
    if let Some(given) = &raw.name {
        if given != &name {
            return Err(PresetError::invalid(format!(
                "preset {name:?} says name = {given:?}; the table key is the name"
            )));
        }
    }
    let video_codec = raw.video_codec.unwrap_or(VideoCodec::H264);
    let audio_codec = raw.audio_codec.unwrap_or(if video_codec == VideoCodec::Gif {
        AudioCodec::None
    } else {
        AudioCodec::Aac
    });
    let container = raw.container.unwrap_or(match video_codec {
        VideoCodec::Gif => Container::Gif,
        VideoCodec::None => Container::M4a,
        _ => Container::Mp4,
    });

    // codec and container have to make sense together
    if (container == Container::Gif) != (video_codec == VideoCodec::Gif) {
        return Err(PresetError::invalid(format!(
            "{name}: an animated GIF needs container = \"gif\" and video_codec = \"gif\", and neither one alone"
        )));
    }
    if video_codec == VideoCodec::Gif && audio_codec != AudioCodec::None {
        return Err(PresetError::invalid(format!(
            "{name}: a GIF cannot carry audio (set audio_codec = \"none\")"
        )));
    }
    if video_codec == VideoCodec::None && audio_codec == AudioCodec::None {
        return Err(PresetError::invalid(format!(
            "{name}: video_codec = \"none\" and audio_codec = \"none\" leave nothing to encode"
        )));
    }
    match container {
        Container::M4a if audio_codec != AudioCodec::Aac => {
            return Err(PresetError::invalid(format!(
                "{name}: an .m4a file holds AAC (audio_codec = \"aac\")"
            )));
        }
        Container::Ogg if audio_codec != AudioCodec::Opus => {
            return Err(PresetError::invalid(format!(
                "{name}: an .ogg file holds Opus (audio_codec = \"opus\")"
            )));
        }
        _ => {}
    }

    // the quality mode decides which value is needed
    match raw.quality {
        Quality::Crf => {
            if raw.crf.is_none() {
                return Err(PresetError::invalid(format!(
                    "{name}: quality = \"crf\" needs a crf value"
                )));
            }
            if raw.target_mb.is_some() || raw.bitrate_kbps.is_some() {
                return Err(PresetError::invalid(format!(
                    "{name}: quality = \"crf\" cannot also set target_mb or bitrate_kbps"
                )));
            }
        }
        Quality::TargetSizeMb => {
            if video_codec == VideoCodec::None || video_codec == VideoCodec::Gif {
                return Err(PresetError::invalid(format!(
                    "{name}: only a video preset with a real video codec can target a size"
                )));
            }
            if raw.target_mb.is_none() {
                return Err(PresetError::invalid(format!(
                    "{name}: quality = \"target_size_mb\" needs a target_mb value"
                )));
            }
            if raw.crf.is_some() || raw.bitrate_kbps.is_some() {
                return Err(PresetError::invalid(format!(
                    "{name}: quality = \"target_size_mb\" cannot also set crf or bitrate_kbps"
                )));
            }
        }
        Quality::Bitrate => {
            if raw.bitrate_kbps.is_none() {
                return Err(PresetError::invalid(format!(
                    "{name}: quality = \"bitrate\" needs a bitrate_kbps value"
                )));
            }
            if raw.crf.is_some() || raw.target_mb.is_some() {
                return Err(PresetError::invalid(format!(
                    "{name}: quality = \"bitrate\" cannot also set crf or target_mb"
                )));
            }
        }
    }
    if let Some(crf) = raw.crf {
        if !(0.0..=51.0).contains(&crf) {
            return Err(PresetError::invalid(format!(
                "{name}: crf {crf} is outside 0-51"
            )));
        }
    }
    if let Some(mb) = raw.target_mb {
        if !(0.5..=100_000.0).contains(&mb) {
            return Err(PresetError::invalid(format!(
                "{name}: target_mb {mb} must be between 0.5 and 100000"
            )));
        }
    }
    if let Some(kbps) = raw.bitrate_kbps {
        if !(32..=200_000).contains(&kbps) {
            return Err(PresetError::invalid(format!(
                "{name}: bitrate_kbps {kbps} must be between 32 and 200000"
            )));
        }
    }
    if audio_codec != AudioCodec::None && raw.audio_kbps == 0 {
        return Err(PresetError::invalid(format!(
            "{name}: audio_kbps cannot be 0 when the preset has audio"
        )));
    }
    if audio_codec != AudioCodec::None && !(8..=512).contains(&raw.audio_kbps) {
        return Err(PresetError::invalid(format!(
            "{name}: audio_kbps {} must be between 8 and 512",
            raw.audio_kbps
        )));
    }
    if !X264_SPEEDS.contains(&raw.speed.as_str()) {
        return Err(PresetError::invalid(format!(
            "{name}: speed {:?} is not one of {}",
            raw.speed,
            X264_SPEEDS.join(", ")
        )));
    }
    if let Some(tune) = &raw.tune {
        if !TUNES.contains(&tune.as_str()) {
            return Err(PresetError::invalid(format!(
                "{name}: tune {tune:?} is not one of {}",
                TUNES.join(", ")
            )));
        }
    }
    for (what, v) in [
        ("max_width", raw.max_width),
        ("max_height", raw.max_height),
    ] {
        if v != 0 && !(16..=16384).contains(&v) {
            return Err(PresetError::invalid(format!(
                "{name}: {what} {v} must be 0 (no limit) or 16-16384"
            )));
        }
    }
    if !(0.0..=240.0).contains(&raw.fps_cap) {
        return Err(PresetError::invalid(format!(
            "{name}: fps_cap {} must be between 0 (no cap) and 240",
            raw.fps_cap
        )));
    }
    if raw.gop > 3600 {
        return Err(PresetError::invalid(format!(
            "{name}: gop {} must be 0 (codec default) or at most 3600",
            raw.gop
        )));
    }
    for arg in &raw.extra_args {
        if arg.trim().is_empty() {
            return Err(PresetError::invalid(format!(
                "{name}: extra_args cannot contain an empty argument"
            )));
        }
        if arg.contains('\0') {
            return Err(PresetError::invalid(format!(
                "{name}: extra_args cannot contain a NUL byte"
            )));
        }
    }

    Ok(Preset {
        name: name.clone(),
        title: raw.title.unwrap_or(name),
        container,
        video_codec,
        audio_codec,
        quality: raw.quality,
        crf: raw.crf,
        target_mb: raw.target_mb,
        bitrate_kbps: raw.bitrate_kbps,
        audio_kbps: raw.audio_kbps,
        speed: raw.speed,
        max_width: raw.max_width,
        max_height: raw.max_height,
        fps_cap: raw.fps_cap,
        gop: raw.gop,
        tune: raw.tune,
        hw: raw.hw,
        faststart: raw.faststart,
        hq: raw.hq,
        extra_args: raw.extra_args,
    })
}

/// Parse a TOML document holding one or more presets (each top-level table
/// is one preset, keyed by its name).
pub fn parse_document(text: &str, origin: &str) -> Result<Vec<Preset>, PresetError> {
    let table: toml::Table = toml::from_str(text).map_err(|e| PresetError::Toml {
        origin: origin.to_string(),
        message: e.to_string(),
    })?;
    let mut out = Vec::new();
    for (name, value) in table {
        let raw: RawPreset = value.try_into().map_err(|e| PresetError::Toml {
            origin: format!("{origin}: preset {name:?}"),
            message: e.to_string(),
        })?;
        out.push(build(name, raw)?);
    }
    if out.is_empty() {
        return Err(PresetError::invalid(format!(
            "{origin}: no presets (each top-level table is one preset)"
        )));
    }
    Ok(out)
}

/// A set of presets by name.
#[derive(Debug, Clone, Default)]
pub struct PresetSet {
    presets: BTreeMap<String, Preset>,
}

impl PresetSet {
    /// The built-in presets.
    pub fn builtin() -> &'static PresetSet {
        static BUILTIN: OnceLock<PresetSet> = OnceLock::new();
        BUILTIN.get_or_init(|| {
            let presets = parse_document(BUILTIN_TOML, "presets.toml")
                .expect("the built-in presets are valid");
            let mut set = PresetSet::default();
            for p in presets {
                set.presets.insert(p.name.clone(), p);
            }
            set
        })
    }

    pub fn get(&self, name: &str) -> Option<&Preset> {
        self.presets.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.presets.keys().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Preset> {
        self.presets.values()
    }

    pub fn len(&self) -> usize {
        self.presets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    /// Add or replace presets from a TOML document. Returns how many were set.
    pub fn merge_document(&mut self, text: &str, origin: &str) -> Result<usize, PresetError> {
        let presets = parse_document(text, origin)?;
        let n = presets.len();
        for p in presets {
            self.presets.insert(p.name.clone(), p);
        }
        Ok(n)
    }

    /// The built-in presets plus every `*.toml` under `dir` (override/add by
    /// name). Unreadable or invalid files are reported, not fatal.
    pub fn with_dir(dir: &Path) -> (PresetSet, Vec<String>) {
        let mut set = PresetSet::builtin().clone();
        let mut problems = Vec::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return (set, problems);
        };
        let mut files: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("toml")))
            .collect();
        files.sort();
        for path in files {
            let origin = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    if let Err(e) = set.merge_document(&text, &origin) {
                        problems.push(e.to_string());
                    }
                }
                Err(e) => problems.push(PresetError::Io {
                    path: path.display().to_string(),
                    error: e.to_string(),
                }
                .to_string()),
            }
        }
        (set, problems)
    }

    /// Like [`PresetSet::with_dir`] but any bad file is an error.
    pub fn load_dir(dir: &Path) -> Result<PresetSet, PresetError> {
        let (set, problems) = Self::with_dir(dir);
        match problems.into_iter().next() {
            Some(p) => Err(PresetError::invalid(p)),
            None => Ok(set),
        }
    }

    /// Every preset, sorted by name, as the API shows them.
    pub fn to_json(&self) -> Vec<serde_json::Value> {
        self.presets.values().map(Preset::to_json).collect()
    }
}
