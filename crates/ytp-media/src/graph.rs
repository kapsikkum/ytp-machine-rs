//! The ffmpeg filter graph for one chunk of segments (`_encode_chunk` in
//! `app/generate.py`). Pure string building, so it is testable without ffmpeg.

use ytp_splice::window::{extract_window, segment_durations, PAD_END, PAD_END_LAST};
use ytp_splice::{AudioCache, GenerationOptions, Segment};

use crate::glitch;

pub const FPS: u32 = 25;
const FADE_IN: f64 = 0.012;
const FADE_OUT: f64 = 0.015;
const HOLD_LOOKAHEAD: f64 = 0.12;
const BUTT_LOOKAHEAD: f64 = 0.025;
const CAP_WIDTH: f64 = 440.0;
const CAP_SIZE: i64 = 26;
const CAP_MIN: i64 = 12;

/// What the graph builder needs to ask about the machine and the sources.
pub trait GraphEnv {
    fn has_filter(&self, name: &str) -> bool;
    /// Is the segment's source already 480x270 at 25 fps?
    fn is_native(&self, seg: &Segment) -> bool;
    /// A font for captions, if the machine has one.
    fn font(&self) -> Option<String>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub start: f64,
    pub read: f64,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct ChunkPlan {
    pub inputs: Vec<Input>,
    pub graph: String,
}

/// `atempo` only goes from 0.5 to 2 in one step; chain it past that.
pub fn atempo_chain(mut factor: f64) -> Vec<String> {
    let mut steps = Vec::new();
    while factor < 0.5 {
        steps.push("atempo=0.5".to_string());
        factor /= 0.5;
    }
    while factor > 2.0 {
        steps.push("atempo=2.0".to_string());
        factor /= 2.0;
    }
    steps.push(format!("atempo={factor:.5}"));
    steps
}

/// Audio made `factor` times longer, pitch kept.
pub fn stretch_filter(env: &dyn GraphEnv, factor: f64, span: f64, length: Option<f64>) -> Vec<String> {
    let mut out = vec![format!("apad=pad_dur={:.3}", span.max(0.5))];
    if env.has_filter("rubberband") {
        out.push(format!(
            "rubberband=tempo={:.5}:transients=smooth:window=standard",
            1.0 / factor
        ));
    } else {
        out.extend(atempo_chain(1.0 / factor));
    }
    let len = length.filter(|l| *l != 0.0).unwrap_or(span * factor);
    out.push(format!("atrim=end={len:.4}"));
    out.push("asetpts=PTS-STARTPTS".into());
    out
}

fn cap_size(text: &str) -> i64 {
    let n = text.chars().count();
    if n == 0 {
        return CAP_SIZE;
    }
    let fits = (CAP_WIDTH / (0.58 * n as f64)) as i64;
    CAP_MIN.max(CAP_SIZE.min(fits))
}

/// A drawtext filter for one word, white on a black outline.
pub fn drawtext(text: &str, font: &str, window: Option<(f64, f64)>) -> String {
    let size = cap_size(text);
    let mut text = text.replace('\'', "’");
    for ch in ['\\', ':'] {
        text = text.replace(ch, &format!("\\{ch}"));
    }
    let font = font.replace('\\', "/").replace(':', "\\:");
    let mut f = format!(
        "drawtext=fontfile='{font}':text='{text}':expansion=none:fontcolor=white:fontsize={size}:borderw=3:bordercolor=black:x=(w-tw)/2:y=h-th-12"
    );
    if let Some((a, b)) = window {
        f.push_str(&format!(":enable='between(t,{a:.3},{b:.3})'"));
    }
    f
}

/// The graph and inputs for `segments`. `final_tail` is true only for the
/// chunk holding the overall last word, which gets the generous tail pad.
pub fn plan_chunk(
    segments: &[Segment],
    final_tail: bool,
    captions: bool,
    opts: &GenerationOptions,
    env: &dyn GraphEnv,
    audio: &AudioCache,
) -> ChunkPlan {
    let (speed, pitch) = (opts.speed, opts.pitch);
    let n = segments.len();
    let mut inputs = Vec::with_capacity(n);
    let mut durations = Vec::with_capacity(n);
    let mut starts = Vec::with_capacity(n);
    for (idx, seg) in segments.iter().enumerate() {
        let pad_end = if final_tail && idx == n - 1 { PAD_END_LAST } else { PAD_END };
        let (start, tail_end) = extract_window(seg, pad_end, audio);
        starts.push(start);
        let duration = tail_end - start;
        durations.push(duration);
        let read = duration
            + if seg.stretch.is_some_and(|s| s != 0.0) {
                HOLD_LOOKAHEAD
            } else if seg.fade_out == Some(0.0) {
                BUTT_LOOKAHEAD
            } else {
                0.0
            };
        inputs.push(Input { start, read, path: seg.source_file.clone() });
    }

    let font = if captions { env.font() } else { None };
    let mut parts: Vec<String> = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        let dur0 = durations[i];
        let fi = seg.fade_in.unwrap_or(FADE_IN);
        let fo = seg.fade_out.unwrap_or(FADE_OUT);
        let pause = seg.pause();
        let rev = seg.reverse;
        let stretch = seg.stretch.filter(|s| *s != 0.0).unwrap_or(1.0);
        let span = dur0;
        let dur = dur0 * stretch;
        let (_s, _snd, quantised) = segment_durations(seg, (starts[i], starts[i] + span), opts);

        let native = env.is_native(seg);
        let mut v: Vec<String> = Vec::new();
        if !native {
            v.push("scale=480:270:force_original_aspect_ratio=decrease".into());
            v.push("pad=480:270:(ow-iw)/2:(oh-ih)/2".into());
        }
        if stretch != 1.0 {
            v.push(format!("setpts=PTS*{stretch:.5}"));
        }
        if !native || stretch != 1.0 {
            v.push(format!("fps={FPS}"));
        }
        v.push("setsar=1".into());
        if rev {
            v.push("reverse".into());
        }
        if pause > 0.0 {
            v.push(format!("tpad=stop_mode=clone:stop_duration={pause:.4}"));
        }
        for name in &seg.glitch {
            if let Some(g) = glitch::find(name) {
                v.extend(g.video.iter().map(|s| s.to_string()));
            }
        }
        if !seg.glitch.is_empty() {
            v.push("setsar=1".into());
        }
        if let Some(font) = &font {
            if !seg.subtitle_words.is_empty() {
                let base = starts[i];
                let len = seg.subtitle_words.len();
                for (k, (w, a, b)) in seg.subtitle_words.iter().enumerate() {
                    let mut lo = if k == 0 { 0.0 } else { (a - base).max(0.0) };
                    let mut hi = if k == len - 1 { dur } else { lo.max(b - base) };
                    if stretch != 1.0 {
                        lo *= stretch;
                        hi = dur.min(hi * stretch);
                    }
                    if rev {
                        let (l, h) = ((dur - hi).max(0.0), (dur - lo).max(0.0));
                        lo = l;
                        hi = h;
                    }
                    v.push(drawtext(w, font, Some((lo, hi))));
                }
            } else if let Some(sub) = &seg.subtitle {
                v.push(drawtext(sub, font, None));
            }
        }
        if speed != 1.0 {
            v.push(format!("setpts=PTS/{speed:.5}"));
            v.push(format!("fps={FPS}"));
        }
        v.push("tpad=stop_mode=clone:stop_duration=0.2".into());
        v.push(format!("trim=end={quantised:.4}"));
        v.push("setpts=PTS-STARTPTS".into());
        parts.push(format!("[{i}:v]{}[v{i}]", v.join(",")));

        let mut a: Vec<String> = vec!["aformat=sample_rates=44100:channel_layouts=stereo".into()];
        if rev {
            a.push("areverse".into());
        }
        if stretch != 1.0 {
            a.extend(stretch_filter(env, stretch, span, Some(quantised * speed - pause)));
        }
        if fi > 0.0 {
            a.push(format!("afade=t=in:st=0:d={fi:.4}"));
        }
        if fo > 0.0 {
            a.push(format!("afade=t=out:st={:.4}:d={fo:.4}", (dur - fo).max(0.0)));
        }
        if pause > 0.0 {
            a.push(format!("apad=pad_dur={pause:.4}"));
        }
        if let Some(g) = seg.gain_db.filter(|g| *g != 0.0) {
            a.push(format!("volume={g:.2}dB"));
        }
        let seg_pitch = pitch + seg.pitch.unwrap_or(0.0);
        if seg_pitch != 0.0 {
            let ratio = 2f64.powf(seg_pitch / 12.0);
            if env.has_filter("rubberband") {
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
        for name in &seg.glitch {
            if let Some(g) = glitch::find(name) {
                a.extend(g.audio.iter().map(|s| s.to_string()));
            }
        }
        if !seg.subword {
            a.push("apad".into());
        }
        a.push(format!("atrim=end={quantised:.4}"));
        a.push("asetpts=PTS-STARTPTS".into());
        parts.push(format!("[{i}:a]{}[a{i}]", a.join(",")));
    }
    let concat_in: String = (0..n).map(|i| format!("[v{i}][a{i}]")).collect();
    parts.push(format!("{concat_in}concat=n={n}:v=1:a=1[vout][aout]"));
    ChunkPlan { inputs, graph: parts.join(";") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytp_splice::NoAudio;

    struct Env {
        rubberband: bool,
        native: bool,
    }
    impl GraphEnv for Env {
        fn has_filter(&self, name: &str) -> bool {
            self.rubberband && name == "rubberband"
        }
        fn is_native(&self, _: &Segment) -> bool {
            self.native
        }
        fn font(&self) -> Option<String> {
            Some("C:\\Windows\\Fonts\\arial.ttf".into())
        }
    }

    fn seg(word: &str, a: f64, b: f64) -> Segment {
        Segment {
            word: word.into(),
            start_time: a,
            end_time: b,
            source_file: "v.mp4".into(),
            prev_end: Some(a - 0.2),
            next_start: Some(b + 0.2),
            subtitle: Some(word.into()),
            tok: Some(0),
            ..Default::default()
        }
    }

    #[test]
    fn atempo_chains_out_of_range_factors() {
        assert_eq!(atempo_chain(1.5), vec!["atempo=1.50000"]);
        assert_eq!(atempo_chain(0.2), vec!["atempo=0.5", "atempo=0.5", "atempo=0.80000"]);
        assert_eq!(atempo_chain(5.0), vec!["atempo=2.0", "atempo=2.0", "atempo=1.25000"]);
    }

    #[test]
    fn plain_clip_graph() {
        let audio = AudioCache::new(&NoAudio);
        let opts = GenerationOptions::default();
        let env = Env { rubberband: false, native: true };
        let mut s = seg("hello", 1.0, 1.4);
        s.pause_after = Some(0.03);
        let p = plan_chunk(&[s.clone(), seg("world", 3.0, 3.5)], true, false, &opts, &env, &audio);
        assert_eq!(p.inputs.len(), 2);
        assert!((p.inputs[0].start - 0.95).abs() < 1e-9); // 0.05 of lead-in
        assert!(p.graph.starts_with("[0:v]setsar=1,tpad=stop_mode=clone:stop_duration=0.0300,"));
        assert!(p.graph.contains("[0:a]aformat=sample_rates=44100:channel_layouts=stereo,afade=t=in:st=0:d=0.0120"));
        assert!(p.graph.ends_with("[v0][a0][v1][a1]concat=n=2:v=1:a=1[vout][aout]"));
        // native sources are not rescaled
        assert!(!p.graph.contains("scale=480:270"));
        let env2 = Env { rubberband: false, native: false };
        let p2 = plan_chunk(&[s], true, false, &opts, &env2, &audio);
        assert!(p2.graph.contains("scale=480:270:force_original_aspect_ratio=decrease,pad=480:270"));
        assert!(p2.graph.contains("fps=25"));
    }

    #[test]
    fn effects_land_in_the_graph() {
        let audio = AudioCache::new(&NoAudio);
        let opts = GenerationOptions::from_json(&serde_json::Value::Null);
        let env = Env { rubberband: false, native: true };
        let mut s = seg("loong", 1.0, 1.4);
        s.stretch = Some(2.0);
        s.subword = true;
        s.reverse = true;
        s.pitch = Some(3.0);
        s.gain_db = Some(6.0);
        s.glitch = vec!["shake".into(), "earrape".into()];
        let p = plan_chunk(&[s], true, true, &opts, &env, &audio);
        let g = &p.graph;
        assert!(g.contains("setpts=PTS*2.00000"));
        assert!(g.contains("reverse"));
        assert!(g.contains("crop=w=440:h=250"));
        assert!(g.contains("drawtext=fontfile='C\\:/Windows/Fonts/arial.ttf':text='loong'"));
        assert!(g.contains("areverse"));
        assert!(g.contains("volume=6.00dB"));
        assert!(g.contains("asetrate=52444"));
        assert!(g.contains("alimiter=limit=0.95"));
        assert!(!g.contains(",apad,")); // a subword piece is not padded to the picture
        // a held piece reads on past its end
        assert!(p.inputs[0].read > 0.12);
    }

    #[test]
    fn captions_are_escaped() {
        let f = drawtext("it's a:b\\c", "C:/f.ttf", Some((0.0, 1.0)));
        assert!(f.contains("text='it’s a\\:b\\\\c'"));
        assert!(f.contains("enable='between(t,0.000,1.000)'"));
    }
}
