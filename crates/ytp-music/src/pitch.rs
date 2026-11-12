//! Pitch perfect: measure what note a clip is sung on, then sing it on another
//! (port of `app/ytpmv/pitch.py`, minus the ffmpeg decoding).
//!
//! * [`track_f0`] — YIN, frame by frame, so the wobble is measured and not averaged
//! * [`pitch_marks`] — one mark per glottal pulse, following that wobble
//! * [`Voice::render`] — TD-PSOLA re-lays those pulses at exactly the note's
//!   period; `tape` mode resamples instead (the chipmunk sound)

use std::sync::OnceLock;

use crate::dsp;
use crate::np::{self, C64};

pub const SR: u32 = 44100;
const FRAME: f64 = 0.040;
const HOP: f64 = 0.005;
const FMIN: f64 = 60.0;
const FMAX: f64 = 600.0;
const YIN_THRESH: f64 = 0.15;
pub const APERIODIC: f64 = 0.35;
const SILENCE: f64 = 0.05;
const UNVOICED_STEP: f64 = 0.010;
const PEAK_SPAN: f64 = 0.025;
const FADE_IN: f64 = 0.002;
const FADE_OUT: f64 = 0.010;
const MIN_NOTE: f64 = 0.030;

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

pub fn midi_to_hz(m: f64) -> f64 {
    440.0 * 2.0f64.powf((m - 69.0) / 12.0)
}

pub fn hz_to_midi(f: f64) -> f64 {
    69.0 + 12.0 * (f / 440.0).log2()
}

pub fn note_name(m: f64) -> String {
    let n = np::round_i64(m);
    format!("{}{}", NOTE_NAMES[n.rem_euclid(12) as usize], n.div_euclid(12) - 1)
}

/// RMS of each whole *win*-sample window of *x*, on the int16 scale.
pub fn rms_windows(x: &[f32], win: usize) -> Vec<f64> {
    let n = x.len().saturating_sub(1).checked_div(win).unwrap_or(0);
    (0..n)
        .map(|i| {
            let sq: Vec<f64> = x[i * win..(i + 1) * win]
                .iter()
                .map(|v| {
                    let s = *v as f64 * 32768.0;
                    s * s
                })
                .collect();
            np::mean_f64(&sq).sqrt()
        })
        .collect()
}

// ── Tracking ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct F0Track {
    /// frame centres, seconds
    pub times: Vec<f64>,
    /// Hz; meaningful only where voiced
    pub f0: Vec<f64>,
    pub voiced: Vec<bool>,
    pub rms: Vec<f64>,
    pub hop: f64,
}

impl F0Track {
    pub fn frame_at(&self, t: f64) -> usize {
        if self.times.is_empty() {
            return 0;
        }
        let i = np::round_i64((t - self.times[0]) / self.hop);
        i.clamp(0, self.times.len() as i64 - 1) as usize
    }
}

fn median_filter(a: &[f64], width: usize) -> Vec<f64> {
    if a.len() < width {
        return a.to_vec();
    }
    let pad = width / 2;
    let mut p = Vec::with_capacity(a.len() + 2 * pad);
    p.extend(std::iter::repeat_n(a[0], pad));
    p.extend_from_slice(a);
    p.extend(std::iter::repeat_n(a[a.len() - 1], pad));
    (0..a.len()).map(|i| np::median_f64(&p[i..i + width])).collect()
}

struct YinOut {
    f0: f64,
    aperiodicity: f64,
    rms: f64,
}

fn yin_frame(frame: &[f64], w: usize, tmin: usize, tmax: usize, sr: u32) -> YinOut {
    let l = frame.len();
    let nfft = crate::util::pow2_ceil(l + w);
    let a = np::rfft(&frame[..w], nfft);
    let b = np::rfft(frame, nfft);
    let prod: Vec<C64> = a.iter().zip(b.iter()).map(|(x, y)| x.conj() * *y).collect();
    let acf_full = np::irfft(&prod, nfft);
    let acf = &acf_full[..tmax + 1];

    let mut cs = Vec::with_capacity(l + 1);
    cs.push(0.0f64);
    let mut run = 0.0f64;
    for v in frame {
        run += v * v;
        cs.push(run);
    }
    let e0 = cs[w];
    let etau: Vec<f64> = (0..=tmax).map(|tau| cs[tau + w] - cs[tau]).collect();
    let mut d: Vec<f64> = (0..=tmax).map(|tau| (e0 + etau[tau] - 2.0 * acf[tau]).max(0.0)).collect();
    d[0] = 0.0;
    let mut cmnd = vec![1.0f64; tmax + 1];
    let mut cum = 0.0f64;
    for tau in 1..=tmax {
        cum += d[tau];
        cmnd[tau] = d[tau] * tau as f64 / cum.max(1e-12);
    }

    let sub = &cmnd[tmin..tmax];
    let first_below = sub.iter().position(|v| *v < YIN_THRESH);
    let mut t = match first_below {
        Some(i) => i,
        None => {
            let mut best = 0;
            for (i, v) in sub.iter().enumerate() {
                if *v < sub[best] {
                    best = i;
                }
            }
            best
        }
    };
    if first_below.is_some() {
        while t + 1 < sub.len() && sub[t + 1] < sub[t] {
            t += 1;
        }
    }
    let mut tau = t + tmin;

    let ncc = |k: usize| acf[k] / (e0 * etau[k]).max(1e-20).sqrt();
    let aperiodicity = cmnd[tau];
    let lo = tau as i64 - 2;
    let mut best_k = (lo.clamp(tmin as i64, tmax as i64 - 1)) as usize;
    let mut best_v = ncc(best_k);
    for off in -1i64..=2 {
        let k = ((tau as i64 + off).clamp(tmin as i64, tmax as i64 - 1)) as usize;
        let v = ncc(k);
        if v > best_v {
            best_v = v;
            best_k = k;
        }
    }
    tau = best_k;
    let (a_, b_, c_) = (ncc(tau - 1), ncc(tau), ncc((tau + 1).min(tmax)));
    let den = a_ - 2.0 * b_ + c_;
    let shift = if den.abs() > 1e-12 { 0.5 * (a_ - c_) / den } else { 0.0 };
    let shift = shift.clamp(-1.0, 1.0);
    YinOut { f0: sr as f64 / (tau as f64 + shift), aperiodicity, rms: (e0 / w as f64).sqrt() }
}

/// Frame-by-frame pitch by YIN, with a voiced/unvoiced decision.
pub fn track_f0(x: &[f32], sr: u32, fmin: f64, fmax: f64, aperiodic: f64) -> F0Track {
    let w = (FRAME * sr as f64) as usize;
    let hop = (HOP * sr as f64) as usize;
    let tmin = ((sr as f64 / fmax) as usize).max(2);
    let tmax = (sr as f64 / fmin).ceil() as usize;
    let l = w + tmax;
    let mut xd: Vec<f64> = x.iter().map(|v| *v as f64).collect();
    if xd.len() < l {
        xd.resize(l, 0.0);
    }
    let n = 1 + (xd.len() - l) / hop;

    let mut f0 = Vec::with_capacity(n);
    let mut ap = Vec::with_capacity(n);
    let mut rms = Vec::with_capacity(n);
    for i in 0..n {
        let o = yin_frame(&xd[i * hop..i * hop + l], w, tmin, tmax, sr);
        f0.push(o.f0);
        ap.push(o.aperiodicity);
        rms.push(o.rms);
    }

    let rms_max = rms.iter().cloned().fold(0.0f64, f64::max);
    let floor = (1e-4f64).max(SILENCE * rms_max);
    let mut voiced: Vec<bool> = (0..n).map(|i| rms[i] > floor && ap[i] < aperiodic).collect();

    // a voiced run shorter than 3 frames is a click YIN happened to like
    let mut i = 0;
    while i < voiced.len() {
        if voiced[i] {
            let mut j = i;
            while j < voiced.len() && voiced[j] {
                j += 1;
            }
            if j - i < 3 {
                for v in &mut voiced[i..j] {
                    *v = false;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }

    if voiced.iter().any(|v| *v) {
        let fv: Vec<f64> = (0..n).filter(|i| voiced[*i]).map(|i| f0[i]).collect();
        let med = np::median_f64(&fv);
        for v in &mut f0 {
            if *v > 1.8 * med {
                *v /= 2.0;
            }
        }
        for v in &mut f0 {
            if *v < 0.55 * med {
                *v *= 2.0;
            }
        }
        let fv: Vec<f64> = (0..n).filter(|i| voiced[*i]).map(|i| f0[i]).collect();
        let smoothed = median_filter(&fv, 5);
        let mut k = 0;
        for i in 0..n {
            if voiced[i] {
                f0[i] = smoothed[k];
                k += 1;
            }
        }
    }

    let times = (0..n).map(|i| (i as f64 * hop as f64 + w as f64 / 2.0) / sr as f64).collect();
    F0Track { times, f0, voiced, rms, hop: HOP }
}

pub fn track_f0_default(x: &[f32], sr: u32) -> F0Track {
    track_f0(x, sr, FMIN, FMAX, APERIODIC)
}

#[derive(Clone, Debug)]
pub struct PitchInfo {
    /// median pitch of the voiced frames, Hz
    pub f0: Option<f64>,
    pub midi: Option<f64>,
    pub note: Option<String>,
    /// voiced frames / audible frames
    pub voiced_ratio: f64,
    /// spread of the pitch, cents (std); lower is steadier. Infinite when unvoiced.
    pub stability: f64,
    pub duration: f64,
}

pub fn describe(track: &F0Track, duration: f64) -> PitchInfo {
    let rms_max = track.rms.iter().cloned().fold(0.0f64, f64::max);
    let floor = (1e-4f64).max(SILENCE * rms_max);
    let loud = track.rms.iter().filter(|r| **r > floor).count();
    let nv = track.voiced.iter().filter(|v| **v).count();
    let ratio = nv as f64 / loud.max(1) as f64;
    if nv == 0 {
        return PitchInfo { f0: None, midi: None, note: None, voiced_ratio: 0.0, stability: f64::INFINITY, duration };
    }
    let f: Vec<f64> = track.f0.iter().zip(track.voiced.iter()).filter(|(_, v)| **v).map(|(f, _)| *f).collect();
    let med = np::median_f64(&f);
    let cents: Vec<f64> = f.iter().map(|v| 1200.0 * (v / med).log2()).collect();
    let m = hz_to_midi(med);
    PitchInfo {
        f0: Some(med),
        midi: Some(m),
        note: Some(note_name(m)),
        voiced_ratio: ratio.min(1.0),
        stability: np::std_f64(&cents),
        duration,
    }
}

pub fn detect_pitch(x: &[f32], sr: u32) -> PitchInfo {
    describe(&track_f0_default(x, sr), x.len() as f64 / sr as f64)
}

// ── Marks and re-synthesis ───────────────────────────────────────────────────

/// Where the period after the one at *prev* lines up best with it.
fn match_period(x: &[f32], prev: f64, p: f64) -> f64 {
    let n = x.len() as i64;
    let h = ((p / 2.0) as i64).max(2);
    let r = ((0.2 * p) as i64).max(1);
    let p0 = np::round_i64(prev);
    let guess = np::round_i64(prev + p);
    if p0 - h < 0 || guess + r + h > n {
        return prev + p;
    }
    let rf: Vec<f64> = x[(p0 - h) as usize..(p0 + h) as usize].iter().map(|v| *v as f64).collect();
    let seg: Vec<f64> = x[(guess - r - h) as usize..(guess + r + h) as usize].iter().map(|v| *v as f64).collect();
    let lags = seg.len() - rf.len() + 1;
    let ref_energy: f64 = rf.iter().map(|v| v * v).sum();
    let mut e = Vec::with_capacity(seg.len() + 1);
    e.push(0.0f64);
    let mut run = 0.0f64;
    for v in &seg {
        run += v * v;
        e.push(run);
    }
    let two_h = 2 * h as usize;
    let ncc: Vec<f64> = (0..lags)
        .map(|k| {
            let dot: f64 = (0..rf.len()).map(|j| seg[k + j] * rf[j]).sum();
            let energy = e[two_h + k] - e[k];
            dot / (energy * ref_energy).max(1e-20).sqrt()
        })
        .collect();
    let mut k = 0;
    for (i, v) in ncc.iter().enumerate() {
        if *v > ncc[k] {
            k = i;
        }
    }
    let mut shift = 0.0;
    if k > 0 && k + 1 < ncc.len() {
        let (a, b, c) = (ncc[k - 1], ncc[k], ncc[k + 1]);
        let den = a - 2.0 * b + c;
        if den.abs() > 1e-12 {
            shift = (0.5 * (a - c) / den).clamp(-1.0, 1.0);
        }
    }
    (guess - r + k as i64) as f64 + shift
}

/// Sample positions, local periods (in samples) and voicedness of each pitch mark.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    pub marks: Vec<i64>,
    pub periods: Vec<f64>,
    pub voiced: Vec<bool>,
}

/// One mark per pitch period through voiced stretches, a fixed hop elsewhere.
pub fn pitch_marks(x: &[f32], track: &F0Track, sr: u32) -> Marks {
    let n = x.len();
    let mut marks: Vec<f64> = Vec::new();
    let mut periods = Vec::new();
    let mut voiced: Vec<bool> = Vec::new();
    let unv = (UNVOICED_STEP * sr as f64) as usize;
    let mut pos = 0.0f64;
    while pos < n as f64 {
        let i = track.frame_at(pos / sr as f64);
        if track.voiced[i] && track.f0[i] > 0.0 {
            let p = sr as f64 / track.f0[i];
            
            let m = if voiced.last().copied().unwrap_or(false) && pos - marks[marks.len() - 1] < 1.5 * p {
                match_period(x, marks[marks.len() - 1], p)
            } else {
                let a = pos as usize;
                let b = ((pos + p) as usize + 1).min(n);
                if b <= a {
                    break;
                }
                let mut best = 0;
                for (j, v) in x[a..b].iter().enumerate() {
                    if *v > x[a + best] {
                        best = j;
                    }
                }
                (a + best) as f64
            };
            let m = match marks.last() {
                Some(last) if m <= *last => last + 1.0,
                _ => m,
            };
            marks.push(m);
            periods.push(p);
            voiced.push(true);
            pos = m + p;
        } else {
            marks.push(pos);
            periods.push(unv as f64);
            voiced.push(false);
            pos += unv as f64;
        }
    }
    Marks { marks: marks.iter().map(|m| np::round_i64(*m)).collect(), periods, voiced }
}

/// Where in the sample a moment of the rendered note comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Warp {
    pub v0: f64,
    pub v1: f64,
    pub s: f64,
    /// tape speed; when not 1, it replaces the rest
    pub rate: f64,
    pub hold: f64,
    pub p: f64,
}

impl Default for Warp {
    fn default() -> Self {
        Warp { v0: 0.0, v1: 0.0, s: 1.0, rate: 1.0, hold: 0.0, p: 0.0 }
    }
}

impl Warp {
    pub fn src(&self, t: f64) -> f64 {
        if self.rate != 1.0 {
            return t * self.rate;
        }
        if self.hold > 0.0 {
            if t < self.v0 {
                return t;
            }
            return if t < self.v0 + self.hold { self.v0 } else { t - self.hold };
        }
        if self.s == 1.0 || t < self.v0 {
            return t;
        }
        let span = (self.v1 - self.v0) * self.s;
        if t < self.v0 + span {
            return self.v0 + (t - self.v0) / self.s;
        }
        self.v1 + (t - self.v0 - span)
    }
}

fn hann(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()).collect()
}

fn grain(x: &[f32], centre: i64, half: usize) -> Vec<f64> {
    let (a, b) = (centre - half as i64, centre + half as i64);
    let mut g = vec![0.0f64; 2 * half];
    let (lo, hi) = (a.max(0), b.min(x.len() as i64));
    if hi > lo {
        for i in lo..hi {
            g[(i - a) as usize] = x[i as usize] as f64;
        }
    }
    let w = hann(2 * half);
    for (v, h) in g.iter_mut().zip(w.iter()) {
        *v *= h;
    }
    g
}

fn fade(y: &mut [f32], sr: u32, end: Option<usize>) {
    let fi = y.len().min((FADE_IN * sr as f64) as usize);
    if fi > 0 {
        let ramp = np::linspace(0.0, 1.0, fi);
        for i in 0..fi {
            y[i] = (y[i] as f64 * ramp[i]) as f32;
        }
    }
    let end = end.map(|e| e.min(y.len())).unwrap_or(y.len());
    let fo = end.min((FADE_OUT * sr as f64) as usize);
    if fo > 0 {
        let ramp = np::linspace(1.0, 0.0, fo);
        for (v, r) in y[end - fo..end].iter_mut().zip(ramp.iter()) {
            *v = (*v as f64 * r) as f32;
        }
        for v in &mut y[end..] {
            *v = 0.0;
        }
    }
}

/// Play *x* at *ratio* times the speed (and pitch).
pub fn resample_tape(x: &[f32], ratio: f64) -> Vec<f32> {
    if ratio == 1.0 || x.len() < 2 {
        return x.to_vec();
    }
    let stop = (x.len() - 1) as f64;
    let count = if stop <= 0.0 { 0 } else { (stop / ratio).ceil() as usize };
    let xd: Vec<f64> = x.iter().map(|v| *v as f64).collect();
    (0..count).map(|i| np::interp_arange_at(i as f64 * ratio, &xd) as f32).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Perfect,
    Tape,
    Raw,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "perfect" => Mode::Perfect,
            "tape" => Mode::Tape,
            _ => Mode::Raw,
        }
    }
}

/// A sample, measured once, that can be sung on any note.
pub struct Voice {
    pub x: Vec<f32>,
    pub sr: u32,
    pub track: F0Track,
    pub info: PitchInfo,
    pub marks: Marks,
    pub v0: f64,
    pub v1: f64,
    peak: OnceLock<Option<(f64, f64)>>,
}

impl Voice {
    pub fn new(x: &[f32], sr: u32) -> Self {
        Self::with_aperiodic(x, sr, APERIODIC)
    }

    pub fn with_aperiodic(x: &[f32], sr: u32, aperiodic: f64) -> Self {
        let track = track_f0(x, sr, FMIN, FMAX, aperiodic);
        let info = describe(&track, x.len() as f64 / sr as f64);
        let marks = pitch_marks(x, &track, sr);
        let vt: Vec<f64> = track.times.iter().zip(track.voiced.iter()).filter(|(_, v)| **v).map(|(t, _)| *t).collect();
        let (v0, v1) = if vt.is_empty() { (0.0, 0.0) } else { (vt[0], vt[vt.len() - 1]) };
        Voice { x: x.to_vec(), sr, track, info, marks, v0, v1, peak: OnceLock::new() }
    }

    pub fn duration(&self) -> f64 {
        self.x.len() as f64 / self.sr as f64
    }

    /// (seconds, period in samples) of the loudest voiced pulse.
    pub fn peak(&self) -> Option<(f64, f64)> {
        *self.peak.get_or_init(|| {
            let v: Vec<usize> = (0..self.marks.voiced.len()).filter(|i| self.marks.voiced[*i]).collect();
            if v.is_empty() {
                return None;
            }
            let loud: Vec<f64> = v
                .iter()
                .map(|i| self.track.rms[self.track.frame_at(self.marks.marks[*i] as f64 / self.sr as f64)])
                .collect();
            let mut best = 0;
            for (k, l) in loud.iter().enumerate() {
                if *l > loud[best] {
                    best = k;
                }
            }
            let m_best = self.marks.marks[v[best]];
            let near: Vec<f64> = v
                .iter()
                .filter(|i| (self.marks.marks[**i] - m_best).abs() as f64 <= PEAK_SPAN * self.sr as f64)
                .map(|i| self.marks.periods[*i])
                .collect();
            Some((m_best as f64 / self.sr as f64, np::median_f64(&near)))
        })
    }

    pub fn plan(&self, dur: f64, stretch: bool) -> Warp {
        let l = self.duration();
        if !stretch || dur <= l {
            return Warp { v0: self.v0, v1: self.v1, ..Warp::default() };
        }
        match self.peak() {
            None => Warp { v0: self.v0, v1: self.v1, ..Warp::default() },
            Some((at, p)) => Warp { v0: at, v1: at, hold: dur - l, p, ..Warp::default() },
        }
    }

    /// The sample sung for *dur* seconds on *target_hz*.
    pub fn render(
        &self,
        target_hz: Option<f64>,
        dur: f64,
        mode: Mode,
        semitones: f64,
        stretch: bool,
    ) -> (Vec<f32>, Warp) {
        let dur = dur.max(MIN_NOTE);
        let sr = self.sr;
        let n_out = np::round_i64(dur * sr as f64).max(0) as usize;
        let target = target_hz.filter(|t| *t != 0.0);
        let f0 = self.info.f0.filter(|f| *f != 0.0);

        let (mut y, warp, end);
        if let (Mode::Perfect, Some(tgt), Some(_), false) = (mode, target, f0, self.marks.marks.is_empty()) {
            let w = self.plan(dur, stretch);
            y = self.psola(tgt, n_out, &w, 1.0);
            end = if stretch { n_out } else { n_out.min(np::round_i64(self.duration() * sr as f64) as usize) };
            warp = w;
        } else {
            let ratio = match (mode, target, f0) {
                (Mode::Tape, Some(t), Some(f)) => t / f,
                _ => 2.0f64.powf(semitones / 12.0),
            };
            warp = Warp { rate: ratio, ..Warp::default() };
            let z = resample_tape(&self.x, ratio);
            y = vec![0.0f32; n_out];
            let k = n_out.min(z.len());
            y[..k].copy_from_slice(&z[..k]);
            end = k;
        }
        fade(&mut y, sr, Some(end));
        (y, warp)
    }

    fn psola(&self, target_hz: f64, n_out: usize, warp: &Warp, formant: f64) -> Vec<f32> {
        let sr = self.sr as f64;
        let x = &self.x;
        let Marks { marks, periods, voiced } = &self.marks;
        let pt = sr / target_hz;
        let max_period = periods.iter().cloned().fold(0.0f64, f64::max);
        let pad = max_period.max(pt).max(UNVOICED_STEP * sr) as usize + 2;
        let mut out = vec![0.0f64; n_out + 2 * pad];
        let unv_half = (UNVOICED_STEP * sr) as usize;
        let mut t = 0.0f64;
        while t < n_out as f64 {
            let src = warp.src(t / sr) * sr;
            if src >= x.len() as f64 {
                break;
            }
            let mut k = marks.partition_point(|m| (*m as f64) < src);
            if k >= marks.len() || (k > 0 && src - (marks[k - 1] as f64) < (marks[k] as f64) - src) {
                k -= 1;
            }
            let c = np::round_i64(t) + pad as i64;
            if voiced[k] {
                let held = warp.hold > 0.0 && warp.v0 <= t / sr && t / sr < warp.v0 + warp.hold;
                let half = (np::round_i64(if held { warp.p } else { periods[k] })).max(2) as usize;
                let g_orig = grain(x, marks[k], half);
                let h2 = (np::round_i64(half as f64 / formant)).max(2) as usize;
                let g: Vec<f64> = if h2 != half {
                    let pos = np::linspace(0.0, (2 * half - 1) as f64, 2 * h2);
                    np::interp_arange(&pos, &g_orig)
                } else {
                    g_orig.clone()
                };
                let g = if formant != 1.0 && g.len() >= 16 {
                    formant_correct(&g, &g_orig, formant)
                } else {
                    g
                };
                let gain = (pt / h2 as f64).min(1.0);
                let start = (c - h2 as i64) as usize;
                for (i, v) in g.iter().enumerate() {
                    out[start + i] += v * gain;
                }
                t += pt;
            } else {
                let g = grain(x, np::round_i64(src), unv_half);
                let start = (c - unv_half as i64) as usize;
                for (i, v) in g.iter().enumerate() {
                    out[start + i] += v;
                }
                t += unv_half as f64;
            }
        }
        out[pad..pad + n_out].iter().map(|v| *v as f32).collect()
    }
}

fn formant_correct(g: &[f64], g_orig: &[f64], formant: f64) -> Vec<f64> {
    let n_fft = 128usize.max(crate::util::pow2_ceil(g.len().max(g_orig.len())));
    let env_orig = dsp::spectral_envelope_lifter(g_orig, n_fft, 16);
    let m = env_orig.len();
    let freqs = np::linspace(0.0, 1.0, m);
    let warped: Vec<f64> = freqs.iter().map(|f| np::clip(f / formant, 0.0, 1.0)).collect();
    let target_env = np::interp(&warped, &freqs, &env_orig);
    let curr_env = dsp::spectral_envelope_lifter(g, n_fft, 16);
    let mut spec = np::rfft(g, n_fft);
    for (s, (t, c)) in spec.iter_mut().zip(target_env.iter().zip(curr_env.iter())) {
        *s *= t / c.max(1e-6);
    }
    let out = np::irfft(&spec, n_fft);
    let w = hann(g.len());
    out.iter().take(g.len()).zip(w.iter()).map(|(a, b)| a * b).collect()
}
