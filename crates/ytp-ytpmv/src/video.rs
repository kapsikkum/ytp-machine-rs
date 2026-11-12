//! The picture of a song: every part a tile, redrawn on its hits, piped as raw
//! frames into one ffmpeg together with the mix (`render._render_ytpmv`'s
//! second half).
//!
//! That is deliberately not a filter graph. A song is thousands of notes, and
//! the sentence renderer already found where "one ffmpeg input per clip"
//! ends: out of file handles well before a single verse.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::decode::Frames;
use crate::mix::{threads, Hit, Mixed, Prepared, Tile};
use crate::samples::Env;
use crate::{msg, say, scope, screen, Error, Progress, Result, FPS};

/// What a tile shows at `t`: `(take, frame, flipped, brightness)`.
///
/// Equal states draw equal pictures, so a tile whose state has not changed
/// since the last frame need not be drawn again — which, in a song of three
/// hundred parts, is most of them most of the time.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TileState {
    pub take: usize,
    pub frame: usize,
    pub flip: bool,
    pub brightness: f64,
}

/// The index of the last hit that has started by `t`.
fn last_started(starts: &[f64], t: f64) -> Option<usize> {
    starts.partition_point(|s| *s <= t).checked_sub(1)
}

fn brightness(since: f64, dur: f64, flash: bool, dim: bool) -> f64 {
    if flash && since < 2.0 / FPS as f64 {
        1.25
    } else if dim && since > dur {
        (1.0 - (since - dur) / 0.3 * 0.55).max(0.45)
    } else {
        1.0
    }
}

pub fn tile_state(takes: &[Frames], hits: &[Hit], starts: &[f64], t: f64, flash: bool, dim: bool) -> TileState {
    let Some(i) = last_started(starts, t) else {
        return TileState { take: 0, frame: 0, flip: false, brightness: if dim { 0.35 } else { 1.0 } };
    };
    let h = &hits[i];
    let take = h.take % takes.len();
    let since = t - h.start;
    let at = since.min(h.dur);
    let src = h.warp.map_or(at, |w| w.src(at));
    let frame = ((src * FPS as f64) as i64).clamp(0, takes[take].n as i64 - 1) as usize;
    TileState { take, frame, flip: h.flip, brightness: brightness(since, h.dur, flash, dim) }
}

/// Brighten or dim an image by `k`, staying in uint8.
fn scaled(v: u8, k: f64) -> u8 {
    if (k - 1.0).abs() < 0.001 {
        v
    } else if k <= 1.0 {
        (v as f64 * k) as u8
    } else {
        (v as f32 * k as f32).min(255.0) as u8
    }
}

/// Draw `state` into the canvas rectangle `cell`.
fn draw_tile(canvas: &mut [u8], cw: usize, cell: (usize, usize, usize, usize), takes: &[Frames], state: &TileState) {
    let (x0, y0, w, h) = cell;
    let img = takes[state.take].frame(state.frame);
    for row in 0..h {
        let src = &img[row * w * 3..(row + 1) * w * 3];
        let dst = &mut canvas[((y0 + row) * cw + x0) * 3..][..w * 3];
        for x in 0..w {
            let sx = if state.flip { w - 1 - x } else { x };
            for c in 0..3 {
                dst[x * 3 + c] = scaled(src[sx * 3 + c], state.brightness);
            }
        }
    }
}

/// A voiceless part's tile: its waveform, lit on the hit and dimmed after.
fn scope_tile(trace: &[f32], hits: &[Hit], starts: &[f64], t: f64, w: usize, h: usize, flash: bool, dim: bool, screen_name: &str) -> Vec<u8> {
    let Some(i) = last_started(starts, t) else {
        return scope::draw(trace, w, h, if dim { 0.35 } else { 1.0 }, false, screen_name);
    };
    let hit = &hits[i];
    let since = t - hit.start;
    let level = if dim && since > hit.dur { (1.0 - (since - hit.dur) / 0.3 * 0.55).max(0.45) } else { 1.0 };
    scope::draw(trace, w, h, level as f32, flash && since < 2.0 / FPS as f64, screen_name)
}

fn escape_label(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\'', "’").replace(':', "\\:")
}

/// The ffmpeg filter chain for the finished picture.
fn filter_chain(prep: &Prepared, font: Option<&str>) -> String {
    let (cw, ch) = prep.canvas;
    let (out_w, out_h) = prep.out_size;
    let retro = prep.opts.screen() != "none";
    let mut vf: Vec<String> = Vec::new();
    // Blown back up first, so the labels are still placed in the full-size
    // picture and stay readable rather than becoming eight chunky squares.
    if retro {
        vf.push(format!("scale={out_w}:{out_h}:flags=neighbor"));
    } else if (cw, ch) != (out_w, out_h) {
        vf.push(format!("scale={out_w}:{out_h}:flags=bilinear"));
    }
    if let (true, Some(font)) = (prep.opts.labels, font) {
        let f = font.replace('\\', "/").replace(':', "\\:");
        for t in &prep.tiles {
            let (x, y, _w, lh) = t.out_cell;
            let text = escape_label(prep.settings[&t.id].text.as_deref().unwrap_or(""));
            vf.push(format!(
                "drawtext=fontfile='{f}':text='{text}':expansion=none:fontcolor=white:fontsize={}:borderw=2:bordercolor=black:x={}:y={}",
                (lh / 12).max(12),
                x + 8,
                y + lh - lh / 12 - 10
            ));
        }
    }
    vf.push("format=yuv420p".into());
    vf.join(",")
}

/// Decode every tile's picture(s), a few at a time.
fn load_frames(
    prep: &Prepared,
    env: &Env,
    progress: Progress,
    needed: Option<&HashSet<(String, usize)>>,
) -> Result<HashMap<String, Vec<Frames>>> {
    let jobs: Vec<(&Tile, usize)> = prep
        .tiles
        .iter()
        .filter(|t| !prep.chip_parts.contains(&t.id))
        .flat_map(|t| (0..prep.sample_of[&t.id].len()).map(move |k| (t, k)))
        .collect();
    let total = jobs.len();
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<Frames>>>> = Mutex::new((0..total).map(|_| None).collect());
    let next = AtomicUsize::new(0);
    let cover = prep.opts.cover;
    std::thread::scope(|s| {
        for _ in 0..threads().min(4).min(total.max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= total {
                    return;
                }
                let (tile, k) = jobs[i];
                let (_, _, w, h) = tile.cell;
                if needed.is_some_and(|n| !n.contains(&(tile.id.clone(), k))) {
                    // A take nothing in the window plays: a black stand-in keeps the indices.
                    done.fetch_add(1, Ordering::Relaxed);
                    results.lock().unwrap()[i] = Some(Ok(Frames { n: 1, w, h, data: vec![0; w * h * 3] }));
                    continue;
                }
                let (_, _, ow, oh) = tile.out_cell;
                let pixel_aspect = (ow as f64 / w as f64) / (oh as f64 / h as f64);
                let keep = || progress("tiles", done.load(Ordering::Relaxed), total);
                let r = prep.sample_of[&tile.id][k].frames(env, w, h, cover, pixel_aspect, &keep);
                done.fetch_add(1, Ordering::Relaxed);
                results.lock().unwrap()[i] = Some(r);
            });
        }
    });
    let mut out: HashMap<String, Vec<Frames>> = HashMap::new();
    for ((tile, _), r) in jobs.iter().zip(results.into_inner().unwrap()) {
        out.entry(tile.id.clone()).or_default().push(r.expect("every job ran")?);
    }
    Ok(out)
}

/// Encode the picture and the mix: the master first (`<out>.master.mp4`, H.264
/// + AAC), then the final file through `web-default`.
///
/// Keeping the master means a finished song can be re-encoded to another
/// preset later without playing every note again.
pub fn render_video(
    prep: &Prepared,
    mixed: &Mixed,
    env: &Env,
    font: Option<&str>,
    encoder: Option<&ytp_encode::Encoder>,
    out: &Path,
    progress: Progress,
) -> Result<()> {
    render_frames(prep, mixed, env, font, encoder, out, progress, false)
}

/// The picture of `prep`'s window (it starts at `prep.t_from`) and its mix as
/// one small fast MP4 at `out`: for looking at, not keeping. Only the takes
/// the window plays are decoded, and nothing is finished or mastered.
pub fn render_preview(prep: &Prepared, mixed: &Mixed, env: &Env, font: Option<&str>, out: &Path, progress: Progress) -> Result<()> {
    render_frames(prep, mixed, env, font, None, out, progress, true)
}

/// The takes of each tile that the window `prep.t_from..prep.t_end` shows.
fn takes_in_window(prep: &Prepared, mixed: &Mixed) -> HashSet<(String, usize)> {
    let mut needed = HashSet::new();
    for tile in &prep.tiles {
        let Some(takes) = prep.sample_of.get(&tile.id).map(Vec::len).filter(|n| *n > 0) else { continue };
        needed.insert((tile.id.clone(), 0));
        let hits = mixed.hits.get(&tile.id).map(Vec::as_slice).unwrap_or(&[]);
        let before = hits.partition_point(|h| h.start <= prep.t_from).saturating_sub(1);
        for h in hits[before..].iter().take_while(|h| h.start < prep.t_end) {
            needed.insert((tile.id.clone(), h.take % takes));
        }
    }
    needed
}

fn render_frames(
    prep: &Prepared,
    mixed: &Mixed,
    env: &Env,
    font: Option<&str>,
    encoder: Option<&ytp_encode::Encoder>,
    out: &Path,
    progress: Progress,
    preview: bool,
) -> Result<()> {
    let (cw, ch) = prep.canvas;
    let screen_name = prep.opts.screen();
    let retro = screen_name != "none";
    // A preview is written straight to `out`; a render makes a master first.
    let master = if preview { out.to_path_buf() } else { ytp_media::master_path(out) };
    say(progress, "tiles", 0, 1)?;
    let needed = preview.then(|| takes_in_window(prep, mixed));
    let frames_of = load_frames(prep, env, progress, needed.as_ref())?;
    let starts_of: HashMap<&str, Vec<f64>> =
        prep.tiles.iter().map(|t| (t.id.as_str(), mixed.hits.get(&t.id).map(|h| h.iter().map(|x| x.start).collect()).unwrap_or_default())).collect();
    let empty: Vec<Hit> = Vec::new();

    let wav_path = out.with_extension("mix.wav");
    std::fs::write(&wav_path, mixed.wav()).map_err(|e| Error::Internal(format!("cannot write the mix: {e}")))?;
    let cleanup = |e: Error| {
        let _ = std::fs::remove_file(&wav_path);
        let _ = std::fs::remove_file(&master);
        let _ = std::fs::remove_file(out);
        e
    };

    let mut cmd = Command::new(env.ffmpeg);
    cmd.args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{cw}x{ch}"), "-r", &FPS.to_string(), "-i", "-"]);
    cmd.arg("-i").arg(&wav_path);
    cmd.args(["-map", "0:v", "-map", "1:a", "-vf", &filter_chain(prep, font)]);
    if preview {
        cmd.args(["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-g", "25", "-crf", "30"]);
        cmd.args(["-c:a", "aac", "-b:a", "96k", "-ar", &crate::SR.to_string(), "-ac", "2", "-shortest", "-movflags", "+faststart"]);
    } else {
        cmd.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", if retro { "16" } else { "23" }]);
        cmd.args(["-c:a", "aac", "-b:a", "192k", "-ar", &crate::SR.to_string(), "-ac", "2", "-shortest", "-movflags", "+faststart"]);
    }
    cmd.arg(&master);
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| cleanup(Error::Internal(format!("cannot run {}: {e}", env.ffmpeg.display()))))?;
    let mut stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let mut stdin = child.stdin.take();

    let mut canvas = vec![0u8; cw * ch * 3];
    let mut drawn: HashMap<usize, TileState> = HashMap::new();
    let mut last_frame: Option<Vec<u8>> = None;
    let mut result: Result<()> = Ok(());
    let (flash, dim) = (prep.opts.flash, prep.opts.dim);
    'frames: for f in 0..prep.n_frames {
        if f % 25 == 0 && !progress("frames", f, prep.n_frames) {
            result = Err(Error::Cancelled);
            break;
        }
        let t = prep.t_from + f as f64 / FPS as f64;
        let mut moved = !prep.chip_parts.is_empty();
        for (i, tile) in prep.tiles.iter().enumerate() {
            let (x0, y0, tw, th) = tile.cell;
            let hits = mixed.hits.get(&tile.id).unwrap_or(&empty);
            let starts = &starts_of[tile.id.as_str()];
            if prep.chip_parts.contains(&tile.id) {
                // A pixel of black between tiles, so the traces do not run together.
                let (w, h) = (tw.saturating_sub(1).max(1), th.saturating_sub(1).max(1));
                let Some(trace) = mixed.traces.get(&tile.id) else { continue };
                let tr = &trace[f * w * 2..(f + 1) * w * 2];
                let img = scope_tile(tr, hits, starts, t, w, h, flash, dim, screen_name);
                for row in 0..h {
                    canvas[((y0 + row) * cw + x0) * 3..][..w * 3].copy_from_slice(&img[row * w * 3..(row + 1) * w * 3]);
                }
                continue;
            }
            let Some(takes) = frames_of.get(&tile.id) else { continue };
            let state = tile_state(takes, hits, starts, t, flash, dim);
            if drawn.get(&i) == Some(&state) {
                continue; // the canvas already shows it
            }
            drawn.insert(i, state);
            moved = true;
            draw_tile(&mut canvas, cw, tile.cell, takes, &state);
        }
        let write = if retro {
            if moved || last_frame.is_none() {
                last_frame = Some(screen::apply(&canvas, cw, ch, screen_name));
            }
            stdin.as_mut().unwrap().write_all(last_frame.as_ref().unwrap())
        } else {
            stdin.as_mut().unwrap().write_all(&canvas)
        };
        if write.is_err() {
            break 'frames;
        }
    }
    drop(stdin);

    if result.is_ok() {
        // Wait for the encoder, watching for a cancel.
        let _ = say(progress, "encoding", 0, 1);
        let deadline = std::time::Instant::now() + Duration::from_secs(3600);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {}
                Err(_) => break None,
            }
            if !progress("encoding", 0, 1) {
                result = Err(Error::Cancelled);
                break None;
            }
            if std::time::Instant::now() > deadline {
                result = msg("ffmpeg took too long");
                break None;
            }
            std::thread::sleep(Duration::from_millis(40));
        };
        match status {
            Some(s) if s.success() => {}
            Some(_) => {
                let log = err_reader.join().unwrap_or_default();
                let log = ytp_media::proc::trim_stderr(&log, 600);
                let _ = std::fs::remove_file(&wav_path);
                let _ = std::fs::remove_file(&master);
                let _ = std::fs::remove_file(out);
                return msg(format!("FFmpeg failed: {log}"));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    } else {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_file(&wav_path);
    match result {
        Ok(()) => {
            if !master.is_file() {
                return msg("the encoder produced no video");
            }
            if preview {
                return Ok(());
            }
            finish_encode(prep, env, encoder, &master, out, progress)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&master);
            let _ = std::fs::remove_file(out);
            Err(e)
        }
    }
}

/// Turn the master into the result with `web-default`, at the size the role's
/// quota decided.
fn finish_encode(
    prep: &Prepared,
    env: &Env,
    encoder: Option<&ytp_encode::Encoder>,
    master: &Path,
    out: &Path,
    progress: Progress,
) -> Result<()> {
    let own;
    let encoder = match encoder {
        Some(e) => e,
        None => {
            own = ytp_encode::Encoder::new(env.ffmpeg);
            &own
        }
    };
    let Some(preset) = encoder.presets().get("web-default") else {
        let _ = std::fs::remove_file(master);
        return msg("the web-default preset is missing");
    };
    let last = AtomicUsize::new(0);
    let report = |f: f32| {
        let done = (f.clamp(0.0, 1.0) * 100.0) as usize;
        last.store(done, Ordering::Relaxed);
        progress("encoding", done, 100);
    };
    let cancel = || !progress("encoding", last.load(Ordering::Relaxed), 100);
    let overrides = ytp_encode::Overrides { max_width: 0, max_height: prep.out_size.1 as u32 };
    match encoder.encode(master, preset, overrides, out, &report, &cancel) {
        Ok(_) => {
            let _ = say(progress, "encoding", 100, 100);
            Ok(())
        }
        Err(ytp_encode::EncodeError::Cancelled) => {
            let _ = std::fs::remove_file(master);
            Err(Error::Cancelled)
        }
        Err(e) => {
            let _ = std::fs::remove_file(master);
            let _ = std::fs::remove_file(out);
            msg(format!("the final encode failed: {e}"))
        }
    }
}
