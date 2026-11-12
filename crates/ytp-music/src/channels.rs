//! How many notes each machine could actually sound at once, and on what
//! (the channel half of `app/ytpmv/tone.py`).
//!
//! With "accurate" on, a song gets no more than that: a note that finds every
//! channel of its kind busy takes one over and cuts off what was on it, which
//! is exactly what a driver did when a song asked for more than the chip had.

use std::collections::HashMap;

use crate::music::Part;
use crate::tone::resolve;
use crate::{gb, nes, sid, sms, ym2612};

/// What each machine calls "the voice, played off this machine".
pub const SAMPLED: &[(&str, &str)] = &[
    ("md", "dac"),
    ("nes", "dpcm"),
    ("sms", "psg-pcm"),
    ("snes", "brr"),
    ("gb", "gb-pcm"),
    ("gbc", "gb-pcm"),
    ("opl2", "sb-pcm"),
    ("opl3", "sb16-pcm"),
    ("sid", "sid-digi"),
    ("sid8580", "sid-digi"),
];

/// The sampled-voice tone for *machine*.
pub fn sampled_tone(machine: &str) -> Option<&'static str> {
    SAMPLED.iter().find(|(m, _)| *m == machine).map(|(_, t)| *t)
}

/// A machine and the kind of channel a tone is played on.
pub type ChannelKind = (&'static str, &'static str);

pub const CHANNELS: &[(ChannelKind, usize)] = &[
    (("md", "fm"), 6),
    (("md", "dac"), 1),
    (("nes", "pulse"), 2),
    (("nes", "triangle"), 1),
    (("nes", "noise"), 1),
    (("nes", "dpcm"), 1),
    (("sms", "tone"), 3),
    (("sms", "noise"), 1),
    (("sms", "pcm"), 1),
    (("gb", "pulse"), 2),
    (("gb", "wave"), 1),
    (("gb", "noise"), 1),
    (("sid", "voice"), 3),
    (("sid", "digi"), 1),
    (("opl2", "fm"), 9),
    (("opl2", "pcm"), 1),
    (("opl3", "fm"), 18),
    (("opl3", "pcm"), 1),
    (("snes", "voice"), 8),
];

fn channel_size(ch: ChannelKind) -> usize {
    CHANNELS.iter().find(|(c, _)| *c == ch).map(|(_, n)| *n).unwrap_or(1)
}

/// The (machine, kind of channel) *tone* is played on; `None` for the voice.
pub fn channel(tone: Option<&str>) -> Option<ChannelKind> {
    let t = resolve(tone?)?;
    let t = t.as_str();
    if t == "clean" {
        return None;
    }
    if t == "dac" || ym2612::SAMPLED_DRUMS.contains(&t) {
        return Some(("md", "dac"));
    }
    if ym2612::patch_by_name(t).is_some() {
        return Some(("md", "fm"));
    }
    if t == "dpcm" {
        return Some(("nes", "dpcm"));
    }
    if let Some(v) = nes::voice(t) {
        return Some((
            "nes",
            match v.kind {
                nes::Kind::Pulse => "pulse",
                nes::Kind::Triangle => "triangle",
                nes::Kind::Noise => "noise",
            },
        ));
    }
    if t == "psg-pcm" {
        return Some(("sms", "pcm"));
    }
    if let Some(v) = sms::voice(t) {
        return Some(("sms", if v.kind == sms::Kind::Tone { "tone" } else { "noise" }));
    }
    if t == "gb-pcm" {
        return Some(("gb", "wave"));
    }
    if let Some(v) = gb::voice(t) {
        return Some((
            "gb",
            match v.kind {
                gb::Kind::Pulse => "pulse",
                gb::Kind::Wave => "wave",
                gb::Kind::Noise => "noise",
            },
        ));
    }
    if t == "sid-digi" {
        return Some(("sid", "digi"));
    }
    if sid::voice(t).is_some() {
        return Some(("sid", "voice"));
    }
    match t {
        "opl2" => Some(("opl2", "fm")),
        "opl3" => Some(("opl3", "fm")),
        "sb-pcm" => Some(("opl2", "pcm")),
        "sb16-pcm" => Some(("opl3", "pcm")),
        "snes" | "brr" => Some(("snes", "voice")),
        _ => None,
    }
}

fn priority(role: &str) -> i32 {
    match role.split(':').next().unwrap_or("") {
        "lead" => 4,
        "bass" => 3,
        "drums" => 3,
        _ => 1,
    }
}

/// Whether a part's own tone outranks the song-wide chip switch. `clean` does
/// not; `voice` ("clean" said on purpose) does.
pub fn overrides_switch(name: Option<&str>) -> bool {
    match name.and_then(resolve) {
        Some(t) if t == "clean" => name.map(|n| n.trim().to_lowercase() == "voice").unwrap_or(false),
        Some(_) => true,
        None => false,
    }
}

/// A note identified by (part id, written start in seconds as bits, pitch).
pub type NoteKey = (String, u64, u8);

/// When each note is cut off for want of a channel, in the rendered clock.
///
/// *parts* is `(part, tone)`. Returns the cut-off time of the notes that lose
/// their channel — at or before their own start for a note that never got
/// one. Notes not in it play out as written. Keys use the written start
/// (`start.to_bits()`), not the sped-up one.
pub fn allocate(parts: &[(&Part, &str)], speed: f64) -> HashMap<NoteKey, f64> {
    let dac_wanted = parts.iter().filter_map(|(_, t)| channel(Some(t))).any(|c| c == ("md", "dac"));

    struct Event {
        t0: f64,
        neg_rank: i32,
        pitch: u8,
        pid: String,
        t1: f64,
        ch: ChannelKind,
        start: f64,
    }
    let mut events: Vec<Event> = Vec::new();
    for (part, tone) in parts {
        let Some(ch) = channel(Some(tone)) else { continue };
        let rank = priority(&part.role);
        for n in &part.notes {
            events.push(Event {
                t0: n.start / speed,
                neg_rank: -rank,
                pitch: n.pitch,
                pid: part.id.clone(),
                t1: (n.start + n.dur) / speed,
                ch,
                start: n.start,
            });
        }
    }
    events.sort_by(|a, b| {
        a.t0.partial_cmp(&b.t0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.neg_rank.cmp(&b.neg_rank))
            .then(a.pitch.cmp(&b.pitch))
            .then_with(|| a.pid.cmp(&b.pid))
            .then(a.t1.partial_cmp(&b.t1).unwrap_or(std::cmp::Ordering::Equal))
    });

    type Slot = (f64, i32, NoteKey);
    let mut busy: HashMap<ChannelKind, Vec<Slot>> = HashMap::new();
    let mut cuts: HashMap<NoteKey, f64> = HashMap::new();
    for e in events {
        let rank = -e.neg_rank;
        let key: NoteKey = (e.pid.clone(), e.start.to_bits(), e.pitch);
        let size = if e.ch == ("md", "fm") && dac_wanted { 5 } else { channel_size(e.ch) };
        let slots = busy.entry(e.ch).or_default();
        if slots.len() < size {
            slots.push((e.t1, rank, key));
            continue;
        }
        let free: Vec<usize> = (0..slots.len()).filter(|i| slots[*i].0 <= e.t0).collect();
        let idx = if !free.is_empty() {
            let mut best = free[0];
            for i in &free {
                if slots[*i].0 < slots[best].0 {
                    best = *i;
                }
            }
            best
        } else {
            // everything is still sounding: take the least important, and of
            // those the one that has been going longest
            let mut best = 0;
            for i in 1..slots.len() {
                if (slots[i].1, slots[i].0) < (slots[best].1, slots[best].0) {
                    best = i;
                }
            }
            if slots[best].1 > rank {
                cuts.insert(key, e.t0);
                continue;
            }
            cuts.insert(slots[best].2.clone(), e.t0);
            best
        };
        slots[idx] = (e.t1, rank, key);
    }
    cuts
}
