//! Audio clips laid over a rendered sequence: each is cut, put through its
//! effects, delayed to its place and summed with the sequence's own sound.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};
use ytp_core::{Clip, ClipSource, Effect, SynthNote};
use ytp_media::glitch;
use ytp_media::graph::{atempo_chain, stretch_filter, GraphEnv};
use ytp_media::proc::{run, trim_stderr, RunError};
use ytp_music::pitch::midi_to_hz;
use ytp_music::sid::Model;
use ytp_music::tone::{self, ToneParams};
use ytp_music::{wav, SR};

use crate::sequence::Env;
use crate::Error;

/// Most clips mixed in one ffmpeg call (one input each).
const BATCH: usize = 48;
const MIX_TIMEOUT: Duration = Duration::from_secs(600);
const FADE: f64 = 0.004;

#[derive(Debug, Clone)]
pub struct Overlay {
    /// Project time the clip starts at.
    pub start: f64,
    pub duration: f64,
    pub input: PathBuf,
    /// Where in the input to read from, and how much.
    pub in_: f64,
    pub read: f64,
    pub effects: Vec<Effect>,
    /// Track volume as dB.
    pub gain_db: f64,
}

/// A synth clip's notes as a mono WAV in `dir`, made once per distinct clip.
fn synth_file(dir: &Path, chip: &str, notes: &[SynthNote], duration: f64) -> Result<PathBuf, Error> {
    let resolved = tone::resolve(chip).filter(|t| tone::synth_tones().contains(t));
    let Some(chip) = resolved else {
        return Err(Error::Invalid(format!("{chip:?} is not a sound chip this build can play")));
    };
    let mut h = Sha256::new();
    h.update(chip.as_bytes());
    h.update(duration.to_le_bytes());
    for n in notes {
        h.update(n.start.to_le_bytes());
        h.update(n.dur.to_le_bytes());
        h.update([n.pitch, n.velocity]);
    }
    let key: String = h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("synth-{key}.wav"));
    if path.metadata().is_ok_and(|m| m.len() > 0) {
        return Ok(path);
    }
    let sr = SR as f64;
    let mut buf = vec![0.0f32; ((duration * sr).ceil() as usize).max(1)];
    for n in notes {
        let at = (n.start * sr).round() as usize;
        if at >= buf.len() {
            continue;
        }
        let len = ((n.dur * sr).round() as usize).clamp(1, buf.len() - at);
        let params = ToneParams {
            sample_rate: SR,
            hz: Some(midi_to_hz(n.pitch as f64)),
            level: (n.velocity as f64 / 127.0).max(0.2),
            program: None,
            key: None,
            model: Model::Mos6581,
        };
        let y = tone::apply(&vec![0.5f32; len], &chip, &params);
        let gain = 0.5 * (0.35 + 0.65 * n.velocity as f64 / 127.0) as f32;
        for (dst, src) in buf[at..].iter_mut().zip(&y) {
            *dst += src * gain;
        }
    }
    let samples = wav::quantize_f32(&buf);
    let bytes = wav::write(&wav::Wav { sample_rate: SR, channels: 1, samples });
    std::fs::create_dir_all(dir).map_err(|e| Error::Failed(e.to_string()))?;
    let tmp = dir.join(format!("synth-{key}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| Error::Failed(e.to_string()))?;
    std::fs::rename(&tmp, &path).map_err(|e| Error::Failed(e.to_string()))?;
    Ok(path)
}

pub fn overlay_of(env: &Env, clip: &Clip, gain_db: f64) -> Result<Overlay, Error> {
    let (input, in_, read) = match &clip.source {
        ClipSource::Synth { chip, notes } => {
            (synth_file(env.work_dir, chip, notes, clip.duration)?, 0.0, clip.duration)
        }
        ClipSource::CorpusClip { corpus, file, in_, out, .. } => {
            let Some(file) = file else {
                return Err(Error::Invalid(format!("clip {} has no media file to play", clip.id)));
            };
            let path = env
                .locator
                .resolve(corpus, file)
                .filter(|p| p.is_file())
                .ok_or_else(|| Error::Invalid(format!("clip {}: {file:?} is not available in corpus {corpus:?}", clip.id)))?;
            (path, *in_, out - in_)
        }
        ClipSource::Media { asset, in_, out } => {
            let path = env
                .media_path(asset)
                .ok_or_else(|| Error::Invalid(format!("clip {}: media asset {asset:?} was not found", clip.id)))?;
            (path, *in_, out - in_)
        }
        ClipSource::Generated { .. } => {
            return Err(Error::Invalid(format!(
                "clip {}: a generated clip has to be expanded into clips before it can be rendered",
                clip.id
            )))
        }
    };
    Ok(Overlay { start: clip.start, duration: clip.duration, input, in_, read, effects: clip.effects.clone(), gain_db })
}

/// The audio filters of one overlay, ending trimmed to its length.
fn chain(ov: &Overlay, graph_env: &dyn GraphEnv) -> Vec<String> {
    let (mut speed, mut stretch, mut pitch, mut db) = (1.0, 1.0, 0.0, ov.gain_db);
    let (mut reverse, mut pause) = (false, 0.0);
    let (mut fade_in, mut fade_out) = (FADE, FADE);
    let mut glitches: Vec<&str> = Vec::new();
    for e in &ov.effects {
        match e {
            Effect::Speed { factor } => speed = *factor,
            Effect::Stretch { factor } => stretch = *factor,
            Effect::Reverse => reverse = true,
            Effect::Pitch { semitones } => pitch += semitones,
            Effect::Volume { db: d } => db += d,
            Effect::FadeIn { seconds } => fade_in = *seconds,
            Effect::FadeOut { seconds } => fade_out = *seconds,
            Effect::Pause { seconds } => pause = *seconds,
            Effect::Subword => {}
            other => {
                if let Some(g) = other.glitch_name() {
                    glitches.push(g);
                }
            }
        }
    }
    let mut a: Vec<String> = vec!["aformat=sample_rates=44100:channel_layouts=stereo".into()];
    if reverse {
        a.push("areverse".into());
    }
    let span = ov.read.max(0.01);
    let mut len = span;
    if stretch != 1.0 {
        len = span * stretch;
        a.extend(stretch_filter(graph_env, stretch, span, Some(len)));
    }
    if fade_in > 0.0 {
        a.push(format!("afade=t=in:st=0:d={fade_in:.4}"));
    }
    if fade_out > 0.0 {
        a.push(format!("afade=t=out:st={:.4}:d={fade_out:.4}", (len - fade_out).max(0.0)));
    }
    if pause > 0.0 {
        a.push(format!("apad=pad_dur={pause:.4}"));
    }
    if db != 0.0 {
        a.push(format!("volume={db:.2}dB"));
    }
    if pitch != 0.0 {
        let ratio = 2f64.powf(pitch / 12.0);
        if graph_env.has_filter("rubberband") {
            a.push(format!("rubberband=pitch={ratio:.5}:formant=preserved"));
        } else {
            a.push(format!("asetrate={}", (44100.0 * ratio) as i64));
            a.push("aresample=44100".into());
            a.extend(atempo_chain(1.0 / ratio));
        }
    }
    if speed != 1.0 {
        a.extend(atempo_chain(speed));
    }
    for g in glitches {
        if let Some(g) = glitch::find(g) {
            a.extend(g.audio.iter().map(|s| s.to_string()));
        }
    }
    a.push("apad".into());
    a.push(format!("atrim=end={:.4}", ov.duration));
    a.push("asetpts=PTS-STARTPTS".into());
    a
}

fn run_ffmpeg(env: &Env, args: &[String], keep_going: &dyn Fn() -> bool) -> Result<(), Error> {
    let mut cmd = Command::new(&env.renderer.tools().ffmpeg);
    cmd.args(args);
    let out = run(&mut cmd, MIX_TIMEOUT, keep_going).map_err(|e| match e {
        RunError::Cancelled => Error::Cancelled,
        RunError::Timeout => Error::Failed("ffmpeg took too long".into()),
        RunError::Spawn(e) => Error::Unavailable(format!("cannot run ffmpeg: {e}")),
    })?;
    if out.success {
        Ok(())
    } else {
        Err(Error::Failed(format!("ffmpeg failed:\n{}", trim_stderr(&out.stderr, 2000))))
    }
}

/// `base` with `overlays` mixed into its sound; the picture is copied.
/// `base_t0` is the project time `base` starts at.
pub fn mix_overlays(
    env: &Env,
    base: &Path,
    base_t0: f64,
    overlays: &[Overlay],
    out: &Path,
    keep_going: &dyn Fn() -> bool,
) -> Result<(), Error> {
    let mut current = base.to_path_buf();
    let batches: Vec<&[Overlay]> = overlays.chunks(BATCH).collect();
    for (bi, batch) in batches.iter().enumerate() {
        let last = bi + 1 == batches.len();
        let target = if last { out.to_path_buf() } else { PathBuf::from(format!("{}.mix{bi}.mp4", out.display())) };
        mix_batch(env, &current, base_t0, batch, &target, keep_going)?;
        if current != base {
            let _ = std::fs::remove_file(&current);
        }
        current = target;
    }
    Ok(())
}

fn mix_batch(
    env: &Env,
    base: &Path,
    base_t0: f64,
    batch: &[Overlay],
    out: &Path,
    keep_going: &dyn Fn() -> bool,
) -> Result<(), Error> {
    let mut graph: Vec<String> = Vec::new();
    let mut labels = String::from("[0:a]");
    let tools = env.renderer.tools();
    for (k, ov) in batch.iter().enumerate() {
        let mut f = chain(ov, tools);
        let delay = ov.start - base_t0;
        if delay < 0.0 {
            f.push(format!("atrim=start={:.4}", -delay));
            f.push("asetpts=PTS-STARTPTS".into());
        } else if delay > 0.0 {
            f.push(format!("adelay=delays={}:all=1", (delay * 1000.0).round() as i64));
        }
        graph.push(format!("[{}:a]{}[o{k}]", k + 1, f.join(",")));
        labels.push_str(&format!("[o{k}]"));
    }
    graph.push(format!(
        "{labels}amix=inputs={}:normalize=0:duration=first:dropout_transition=0,alimiter=limit=0.95[aout]",
        batch.len() + 1
    ));
    let script = PathBuf::from(format!("{}.filter.txt", out.display()));
    std::fs::write(&script, graph.join(";")).map_err(|e| Error::Failed(format!("cannot write the filter graph: {e}")))?;
    let build = |flag: &str| -> Vec<String> {
        let mut a: Vec<String> = ["-y", "-hide_banner", "-loglevel", "error", "-i"].iter().map(|s| s.to_string()).collect();
        a.push(base.display().to_string());
        for ov in batch {
            a.extend(["-ss".to_string(), format!("{:.4}", ov.in_), "-t".into(), format!("{:.4}", ov.read.max(0.01))]);
            a.extend(["-i".to_string(), ov.input.display().to_string()]);
        }
        a.extend([flag.to_string(), script.display().to_string()]);
        a.extend(["-map", "0:v", "-map", "[aout]", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-ar", "44100", "-ac", "2"].map(String::from));
        a.extend(["-shortest", "-movflags", "+faststart"].map(String::from));
        a.push(out.display().to_string());
        a
    };
    let mut res = run_ffmpeg(env, &build("-/filter_complex"), keep_going);
    if let Err(Error::Failed(msg)) = &res {
        if msg.contains("Unrecognized option") || msg.contains("Option not found") {
            res = run_ffmpeg(env, &build("-filter_complex_script"), keep_going);
        }
    }
    let _ = std::fs::remove_file(&script);
    if res.is_err() {
        let _ = std::fs::remove_file(out);
    }
    res
}
