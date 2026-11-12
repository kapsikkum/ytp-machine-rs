//! Cutting clips and encoding the finished video by driving the ffmpeg CLI.
//!
//! [`Renderer`] is the boundary: [`FfmpegRenderer`] is the only
//! implementation today (a port of `_encode_chunk` / `_build_video` in
//! `app/generate.py`), and an in-process renderer can replace it later
//! without touching the callers.

pub mod cache;
pub mod glitch;
pub mod graph;
pub mod proc;
pub mod tools;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ytp_splice::{AudioCache, AudioProbe, GenerationOptions, Segment};

pub use crate::cache::ClipCache;
use crate::graph::{plan_chunk, FPS};
use crate::proc::{run, trim_stderr, RunError};
use crate::tools::Tools;

/// Windows' CreateProcess line caps at ~32K characters; each input adds
/// `-ss X -t Y -i <path>`, so long sentences are encoded in chunks.
const CMD_BUDGET: usize = 24_000;
/// Most inputs to hand one ffmpeg call: every input gets its own decoder, so
/// memory scales with this and not with the length of the sentence.
const MAX_INPUTS_PER_CALL: usize = 20;
const ENCODE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
    #[error("ffmpeg is not available: {0}")]
    Unavailable(String),
}

/// Everything a renderer needs to make one video.
pub struct RenderJob<'a> {
    pub segments: &'a [Segment],
    pub options: &'a GenerationOptions,
    pub captions: bool,
    pub out: &'a Path,
    /// Memoised audio measurements shared with the planning that made `segments`.
    pub audio: &'a AudioCache<'a>,
    /// Output height ceiling from the caller's role quota (0 = no limit).
    pub max_height: u32,
    /// Presets to finish the master with; `None` uses the built-in ones.
    pub encoder: Option<&'a ytp_encode::Encoder>,
    /// Finish the master into `out` through the shared encoder. Intermediate
    /// renders (cached sample clips) set this and take the master as they are.
    pub finish: bool,
    /// Asked while the final encode runs; `None` falls back to `progress`.
    pub cancel: Option<&'a dyn Fn() -> bool>,
}

/// `out` is `<id>.mp4`; the master kept beside it is `<id>.master.mp4`, so a
/// finished job can be re-encoded later without rendering again.
pub fn master_path(out: &Path) -> PathBuf {
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    out.with_file_name(format!("{stem}.master.mp4"))
}

/// `progress(stage, done, total)`; return `false` to cancel.
pub type ProgressFn<'a> = &'a dyn Fn(&str, usize, usize) -> bool;

pub trait Renderer: Send + Sync {
    /// Encode `job.segments` to `job.out` (H.264 + AAC in MP4, 480x270 at 25 fps).
    fn render(&self, job: &RenderJob, progress: ProgressFn) -> Result<(), RenderError>;
    /// Glitch effects this renderer can do (the others are never picked by chaos).
    fn glitches(&self) -> Vec<String>;
    /// Reads source audio for the planner's measurements.
    fn audio(&self) -> &dyn AudioProbe;
    /// Is the renderer usable? The reason when it is not, else a description.
    fn check(&self) -> Result<String, String>;
    /// Wait for background work such as filling the clip cache (tests and
    /// benchmarks).
    fn wait_idle(&self, _timeout: Duration) {}
}

/// A quick, low-quality render of one stretch of a sequence, for previews:
/// all the segments in one ffmpeg call, a fast encode, and only
/// `skip..skip + length` of the result kept.
#[derive(Debug, Clone)]
pub struct Quick {
    pub skip: f64,
    pub length: f64,
    pub crf: u32,
}

/// Most segments a quick render takes (one decoder each, one command line).
pub const QUICK_MAX_SEGMENTS: usize = 96;

impl Quick {
    pub fn encoder_args(&self) -> Vec<String> {
        ["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-g", "25", "-crf"]
            .iter()
            .map(|s| s.to_string())
            .chain(std::iter::once(self.crf.to_string()))
            .collect()
    }
}

pub struct FfmpegRenderer {
    tools: Tools,
    /// `-/filter_complex` (ffmpeg 7+) works; otherwise `-filter_complex_script`.
    new_script_flag: AtomicBool,
    cache: Option<ClipCache>,
}

impl FfmpegRenderer {
    pub fn new(ffmpeg: impl Into<PathBuf>) -> Self {
        Self {
            tools: Tools::new(ffmpeg),
            new_script_flag: AtomicBool::new(true),
            cache: None,
        }
    }

    /// Read segments from cut clips kept in cache where there are any.
    pub fn with_cache(mut self, cache: ClipCache) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn cache(&self) -> Option<&ClipCache> {
        self.cache.as_ref()
    }

    pub fn tools(&self) -> &Tools {
        &self.tools
    }

    fn run_ffmpeg(
        &self,
        args: &[String],
        progress: ProgressFn,
        stage: (&str, usize, usize),
    ) -> Result<(), RenderError> {
        let mut cmd = std::process::Command::new(&self.tools.ffmpeg);
        cmd.args(args);
        let out = run(&mut cmd, ENCODE_TIMEOUT, &|| progress(stage.0, stage.1, stage.2))
            .map_err(|e| match e {
                RunError::Cancelled => RenderError::Cancelled,
                RunError::Timeout => RenderError::Failed("ffmpeg took too long".into()),
                RunError::Spawn(e) => RenderError::Unavailable(format!(
                    "cannot run {}: {e}",
                    self.tools.ffmpeg.display()
                )),
            })?;
        if out.success {
            Ok(())
        } else {
            Err(RenderError::Failed(format!(
                "ffmpeg failed:\n{}",
                trim_stderr(&out.stderr, 2000)
            )))
        }
    }

    /// One ffmpeg call for one chunk of segments.
    #[allow(clippy::too_many_arguments)]
    fn encode_chunk(
        &self,
        job: &RenderJob,
        segments: &[Segment],
        out: &Path,
        final_tail: bool,
        quick: Option<&Quick>,
        progress: ProgressFn,
        stage: (&str, usize, usize),
    ) -> Result<(), RenderError> {
        let plan = plan_chunk(segments, final_tail, job.captions, job.options, &self.tools, job.audio);
        let script = PathBuf::from(format!("{}.filter.txt", out.display()));
        std::fs::write(&script, &plan.graph)
            .map_err(|e| RenderError::Failed(format!("cannot write the filter graph: {e}")))?;

        // A window already cut is read whole from its small file.
        let cuts: Vec<Option<PathBuf>> = plan
            .inputs
            .iter()
            .map(|i| self.cache.as_ref().and_then(|c| c.lookup(&i.path, i.start, i.read)))
            .collect();
        let build = |flag: &str| -> Vec<String> {
            let mut a: Vec<String> = ["-y", "-hide_banner", "-loglevel", "error"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            for (i, cut) in plan.inputs.iter().zip(&cuts) {
                if let Some(cut) = cut {
                    a.extend(["-i".into(), cut.display().to_string()]);
                    continue;
                }
                a.extend([
                    "-ss".into(),
                    format!("{:.4}", i.start),
                    "-t".into(),
                    format!("{:.4}", i.read),
                    "-i".into(),
                    i.path.clone(),
                ]);
            }
            a.extend([flag.to_string(), script.display().to_string()]);
            a.extend(["-map", "[vout]", "-map", "[aout]"].map(String::from));
            match quick {
                Some(q) => {
                    a.extend(q.encoder_args());
                    a.extend(["-ss".to_string(), format!("{:.4}", q.skip), "-t".into(), format!("{:.4}", q.length)]);
                }
                None => a.extend(self.tools.encoder_args()),
            }
            // Always 4:2:0: colour effects hand the encoder RGB, x264 then
            // writes 4:4:4, and Firefox calls that file corrupt.
            a.extend(
                ["-pix_fmt", "yuv420p", "-c:a", "aac", "-ar", "44100", "-ac", "2", "-shortest", "-movflags", "+faststart"]
                    .map(String::from),
            );
            a.push(out.display().to_string());
            a
        };
        let new = self.new_script_flag.load(Ordering::Relaxed);
        let mut res = self.run_ffmpeg(&build(if new { "-/filter_complex" } else { "-filter_complex_script" }), progress, stage);
        if let Err(RenderError::Failed(msg)) = &res {
            if msg.contains("Unrecognized option") || msg.contains("Option not found") {
                self.new_script_flag.store(!new, Ordering::Relaxed);
                res = self.run_ffmpeg(&build(if new { "-filter_complex_script" } else { "-/filter_complex" }), progress, stage);
            }
        }
        let _ = std::fs::remove_file(&script);
        if res.is_err() {
            let _ = std::fs::remove_file(out);
        }
        res
    }

    /// Render `segments` (at most [`QUICK_MAX_SEGMENTS`]) to `out` as one fast
    /// MP4, keeping only the stretch `quick` names. Nothing is finished or
    /// kept: it is for looking at.
    #[allow(clippy::too_many_arguments)]
    pub fn render_quick(
        &self,
        segments: &[Segment],
        options: &GenerationOptions,
        captions: bool,
        audio: &AudioCache,
        out: &Path,
        quick: &Quick,
        progress: ProgressFn,
    ) -> Result<(), RenderError> {
        if segments.is_empty() {
            return Err(RenderError::Failed("nothing to render".into()));
        }
        if segments.len() > QUICK_MAX_SEGMENTS {
            return Err(RenderError::Failed(format!(
                "{} clips in one preview window is too many (at most {QUICK_MAX_SEGMENTS})",
                segments.len()
            )));
        }
        self.tools.version().map_err(RenderError::Unavailable)?;
        let job = RenderJob {
            segments,
            options,
            captions,
            out,
            audio,
            max_height: 0,
            encoder: None,
            finish: false,
            cancel: None,
        };
        self.encode_chunk(&job, segments, out, true, Some(quick), progress, ("encoding", 0, 1))
    }

    fn build_video(&self, job: &RenderJob, progress: ProgressFn) -> Result<(), RenderError> {
        let say = |stage: &str, done: usize, total: usize| -> Result<(), RenderError> {
            if progress(stage, done, total) {
                Ok(())
            } else {
                Err(RenderError::Cancelled)
            }
        };
        let master = master_path(job.out);
        let mut chunks: Vec<&[Segment]> = Vec::new();
        let (mut begin, mut used) = (0usize, 0usize);
        for (k, seg) in job.segments.iter().enumerate() {
            let cost = seg.source_file.len() + 40;
            if k > begin && (used + cost > CMD_BUDGET || k - begin >= MAX_INPUTS_PER_CALL) {
                chunks.push(&job.segments[begin..k]);
                begin = k;
                used = 0;
            }
            used += cost;
        }
        chunks.push(&job.segments[begin..]);

        if chunks.len() == 1 {
            say("encoding", 0, 1)?;
            self.encode_chunk(job, chunks[0], &master, true, None, progress, ("encoding", 0, 1))?;
            say("encoding", 1, 1)?;
            return self.finish_output(job, &master, progress);
        }

        let parts: Vec<PathBuf> = (0..chunks.len())
            .map(|ci| PathBuf::from(format!("{}.part{ci}.mp4", master.display())))
            .collect();
        let list = PathBuf::from(format!("{}.concat.txt", master.display()));
        let result = (|| -> Result<(), RenderError> {
            for (ci, chunk) in chunks.iter().enumerate() {
                say("encoding", ci, chunks.len())?;
                self.encode_chunk(
                    job,
                    chunk,
                    &parts[ci],
                    ci == chunks.len() - 1,
                    None,
                    progress,
                    ("encoding", ci, chunks.len()),
                )?;
            }
            say("joining", chunks.len(), chunks.len())?;
            let text: String = parts
                .iter()
                .map(|p| {
                    let abs = std::path::absolute(p).unwrap_or_else(|_| p.clone());
                    let s = abs.display().to_string().replace('\\', "/").replace('\'', "'\\''");
                    format!("file '{s}'\n")
                })
                .collect();
            std::fs::write(&list, text)
                .map_err(|e| RenderError::Failed(format!("cannot write the part list: {e}")))?;
            // The picture is encoded again rather than copied: each part starts
            // a frame's fraction late and a stream copy carried that into the
            // join. A constant frame rate rebuilds the timing.
            let mut a: Vec<String> = ["-y", "-hide_banner", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            a.push(list.display().to_string());
            a.extend(self.tools.encoder_args());
            a.extend(
                ["-pix_fmt", "yuv420p", "-fps_mode", "cfr", "-r"].map(String::from),
            );
            a.push(FPS.to_string());
            a.extend(["-c:a", "copy", "-movflags", "+faststart"].map(String::from));
            a.push(master.display().to_string());
            self.run_ffmpeg(&a, progress, ("joining", chunks.len(), chunks.len()))
        })();
        for p in parts.iter().chain(std::iter::once(&list)) {
            let _ = std::fs::remove_file(p);
        }
        match result {
            Ok(()) => self.finish_output(job, &master, progress),
            Err(e) => {
                let _ = std::fs::remove_file(&master);
                Err(e)
            }
        }
    }

    /// Turn the intermediate master into the result with `web-default`,
    /// applying the caller's height ceiling.
    fn finish_output(
        &self,
        job: &RenderJob,
        master: &Path,
        progress: ProgressFn,
    ) -> Result<(), RenderError> {
        if !job.finish {
            // An intermediate (a cached sample clip): the master is the file.
            if master != job.out {
                let _ = std::fs::remove_file(job.out);
                std::fs::rename(master, job.out)
                    .map_err(|e| RenderError::Failed(format!("cannot move the render into place: {e}")))?;
            }
            return Ok(());
        }
        let own;
        let encoder = match job.encoder {
            Some(e) => e,
            None => {
                own = ytp_encode::Encoder::new(&self.tools.ffmpeg);
                &own
            }
        };
        let Some(preset) = encoder.presets().get("web-default") else {
            return Err(RenderError::Failed(
                "the web-default preset is missing".into(),
            ));
        };
        let last = std::sync::atomic::AtomicUsize::new(0);
        let report = |f: f32| {
            let done = (f.clamp(0.0, 1.0) * 100.0) as usize;
            last.store(done, Ordering::Relaxed);
            progress("finishing", done, 100);
        };
        // The job manager's cancel flag when there is one; otherwise a
        // progress callback that says false stops us.
        let cancel = || {
            job.cancel.map(|c| c()).unwrap_or(false)
                || !progress("finishing", last.load(Ordering::Relaxed), 100)
        };
        let overrides = ytp_encode::Overrides {
            max_width: 0,
            max_height: job.max_height,
        };
        match encoder.encode(master, preset, overrides, job.out, &report, &cancel) {
            Ok(_) => {
                let _ = progress("finishing", 100, 100);
                Ok(())
            }
            Err(ytp_encode::EncodeError::Cancelled) => {
                let _ = std::fs::remove_file(master);
                Err(RenderError::Cancelled)
            }
            Err(e) => {
                let _ = std::fs::remove_file(master);
                let _ = std::fs::remove_file(job.out);
                Err(RenderError::Failed(e.to_string()))
            }
        }
    }
}

impl Renderer for FfmpegRenderer {
    fn render(&self, job: &RenderJob, progress: ProgressFn) -> Result<(), RenderError> {
        if job.segments.is_empty() {
            return Err(RenderError::Failed("nothing to render".into()));
        }
        self.tools.version().map_err(RenderError::Unavailable)?;
        self.build_video(job, progress)
    }

    fn glitches(&self) -> Vec<String> {
        glitch::GLITCHES
            .iter()
            .filter(|g| {
                g.video
                    .iter()
                    .chain(g.audio)
                    .all(|f| self.tools.filters().contains(f.split('=').next().unwrap_or(f)))
            })
            .map(|g| g.name.to_string())
            .collect()
    }

    fn audio(&self) -> &dyn AudioProbe {
        self
    }

    fn check(&self) -> Result<String, String> {
        self.tools.version()
    }

    fn wait_idle(&self, timeout: Duration) {
        if let Some(c) = &self.cache {
            c.wait_idle(timeout);
        }
    }
}

impl AudioProbe for FfmpegRenderer {
    /// Mono 16 kHz samples decoded through 16-bit PCM, as the Python app
    /// measures them (levels were tuned on that).
    fn decode(&self, source: &str, start: f64, duration: f64) -> Option<Vec<f32>> {
        // The same window is measured again whenever a sentence is repeated.
        let (start, duration) = (start.max(0.0), duration);
        if let Some(hit) = self.cache.as_ref().and_then(|c| c.audio(source, start, duration)) {
            return Some(hit);
        }
        let mut cmd = std::process::Command::new(&self.tools.ffmpeg);
        cmd.args(["-v", "error", "-nostdin", "-threads", "2", "-filter_threads", "2"]);
        cmd.args(["-ss", &format!("{:.4}", start.max(0.0)), "-t", &format!("{duration:.4}")]);
        cmd.args(["-i", source, "-vn", "-ac", "1", "-ar", "16000", "-threads", "2", "-f", "s16le", "-"]);
        let out = run(&mut cmd, Duration::from_secs(30), &|| true).ok()?;
        if !out.success {
            return None;
        }
        let raw = &out.stdout[..out.stdout.len() / 2 * 2];
        if let Some(c) = &self.cache {
            c.put_audio(source, start, duration, raw);
        }
        Some(
            raw.as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
                .collect(),
        )
    }
}
