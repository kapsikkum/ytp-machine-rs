//! Reading a Standard MIDI File the way `mido` does (strictly, or clamping
//! data bytes above 127), and the repair pass for tracks written by a buggy
//! writer that ORed its channel count into the status byte
//! (`_repair` / `_unoverflow` / `_salvage` in `app/ytpmv/music.py`).
//!
//! Some writer, given more than sixteen parts, kept counting channels and
//! ORed the count straight into the status byte: `status = (type << 4) | c`
//! with `c` up to `0x7F`. Channel `0x34`'s note-on is then `0xB4`, which reads
//! as a controller; `0x3F`'s program change is `0xFF`, which reads as a meta
//! event; `0x4D`'s note-off and program change are both `0xCD`. A strict
//! reader refuses some of those tracks and misreads the rest — note-offs
//! taken for note-ons, so notes never end.
//!
//! Such a track is spotted by failing to read as plain MIDI (a data byte over
//! 127 is the telltale) and then read again for each possible overflow
//! `h = c >> 4`: a status `s` can be any type `t` with `t | h == s >> 4`.
//! That is ambiguous event by event (`0xCD`: note-off with two data bytes, or
//! program change with one), so the whole track is solved at once, backwards
//! — which positions can still reach the end cleanly — and read forwards
//! along a path that does. Running status is not allowed there; the writer
//! never used it, which keeps the solve cheap and certain. Of the overflows
//! that read, the largest is taken.

use crate::music::DRUM_CHANNEL;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Ev {
    Tempo(i64),
    TimeSig(u32, u32),
    Program(u8, u8),
    Pan(u8, u8),
    NoteOn(u8, u8, u8),
    NoteOff(u8, u8),
    /// any other non-meta message; only moves the song's end tick
    Other,
    /// a meta event that matters nowhere else: time passes, nothing happens
    Meta,
}

const K_NOTE_OFF: u32 = 1;
const K_NOTE_ON: u32 = 2;
const K_CONTROL: u32 = 8;
const K_AFTERTOUCH: u32 = 32;
const K_SYSEX: u32 = 128;
const K_OTHER: u32 = 256;

pub(crate) struct TrackData {
    pub events: Vec<(i64, Ev)>,
    pub name: Option<String>,
    /// bit set of the (non-meta) message kinds the track holds
    kinds: u32,
}

impl TrackData {
    /// Whether a track that reads may still be overflowed by `0x10`.
    ///
    /// With `h = 1` every status still reads: note-ons stay note-ons and the
    /// program change turns into channel pressure. Pressure with no program
    /// change, and nothing whose status lacks the `0x10` bit, is that track.
    fn h1(&self) -> bool {
        self.kinds & K_AFTERTOUCH != 0 && self.kinds & !(K_NOTE_ON | K_CONTROL | K_AFTERTOUCH | K_SYSEX) == 0
    }
}

#[derive(Debug)]
pub(crate) struct ReadError(pub String);

type R<T> = Result<T, ReadError>;

fn err<T>(m: &str) -> R<T> {
    Err(ReadError(m.to_string()))
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cur<'_> {
    fn byte(&mut self) -> R<u8> {
        let v = *self.b.get(self.i).ok_or_else(|| ReadError("unexpected end of file".into()))?;
        self.i += 1;
        Ok(v)
    }

    fn take(&mut self, n: usize) -> R<&[u8]> {
        if self.i.checked_add(n).is_none_or(|e| e > self.b.len()) {
            return err("unexpected end of file");
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }

    fn vlq(&mut self) -> R<i64> {
        let mut v: i64 = 0;
        for _ in 0..8 {
            let b = self.byte()?;
            v = (v << 7) | (b & 0x7F) as i64;
            if b < 0x80 {
                return Ok(v);
            }
        }
        err("variable-length number too long")
    }
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|b| *b as char).collect()
}

fn be32(b: &[u8]) -> usize {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
}

/// One `MTrk` chunk (with its 8-byte head) as mido's `read_track` reads it.
fn read_track(chunk: &[u8], clip: bool) -> R<TrackData> {
    if chunk.len() < 8 || &chunk[..4] != b"MTrk" {
        return err("no MTrk header at start of track");
    }
    let size = be32(&chunk[4..8]);
    let mut cur = Cur { b: &chunk[8..], i: 0 };
    let mut events = Vec::new();
    let mut name = None;
    let mut kinds = 0u32;
    let mut last_status: Option<u8> = None;
    loop {
        if cur.i == size {
            break;
        }
        let delta = cur.vlq()?;
        let first = cur.byte()?;
        let (status, mut peek) = if first < 0x80 {
            match last_status {
                Some(s) => (s, vec![first]),
                None => return err("running status without last_status"),
            }
        } else {
            if first != 0xFF {
                last_status = Some(first);
            }
            (first, Vec::new())
        };
        if status == 0xFF {
            let ty = cur.byte()?;
            let len = cur.vlq()? as usize;
            let data = cur.take(len)?.to_vec();
            let need = match ty {
                0x20 | 0x21 => 1,
                0x51 => 3,
                0x54 => 5,
                0x58 => 4,
                0x59 => 2,
                _ => 0,
            };
            if data.len() < need {
                return err("short meta message");
            }
            if ty == 0x59 && !((data[0] as i8) >= -7 && (data[0] as i8) <= 7 && data[1] <= 1) {
                return err("bad key signature");
            }
            let ev = match ty {
                0x2F => {
                    events.push((delta, Ev::Meta));
                    break;
                }
                0x51 => Ev::Tempo(((data[0] as i64) << 16) | ((data[1] as i64) << 8) | data[2] as i64),
                0x58 => Ev::TimeSig(data[0] as u32, 1u32.checked_shl(data[1] as u32).unwrap_or(0)),
                0x03 => {
                    let s = latin1(&data).trim().to_string();
                    if !s.is_empty() {
                        name = Some(s);
                    }
                    Ev::Meta
                }
                _ => Ev::Meta,
            };
            events.push((delta, ev));
        } else if status == 0xF0 || status == 0xF7 {
            let len = cur.vlq()? as usize;
            cur.take(len)?;
            kinds |= K_SYSEX;
            events.push((delta, Ev::Other));
        } else {
            let n = match status >> 4 {
                0x8 | 0x9 | 0xA | 0xB | 0xE => 2,
                0xC | 0xD => 1,
                _ => match status {
                    0xF1 | 0xF3 => 1,
                    0xF2 => 2,
                    0xF6 | 0xF8 | 0xFA | 0xFB | 0xFC | 0xFE => 0,
                    _ => return err("undefined status byte"),
                },
            };
            let mut data = std::mem::take(&mut peek);
            let more = n - data.len().min(n);
            data.extend_from_slice(cur.take(more)?);
            for b in data.iter_mut() {
                if *b > 127 {
                    if clip {
                        *b = 127;
                    } else {
                        return err("data byte must be in range 0..127");
                    }
                }
            }
            let ch = status & 0x0F;
            let ev = match status >> 4 {
                0x8 => {
                    kinds |= K_NOTE_OFF;
                    Ev::NoteOff(ch, data[0])
                }
                0x9 => {
                    kinds |= K_NOTE_ON;
                    Ev::NoteOn(ch, data[0], data[1])
                }
                0xB => {
                    kinds |= K_CONTROL;
                    if data[0] == 10 {
                        Ev::Pan(ch, data[1])
                    } else {
                        Ev::Other
                    }
                }
                0xC => {
                    kinds |= 16;
                    Ev::Program(ch, data[0])
                }
                0xD => {
                    kinds |= K_AFTERTOUCH;
                    Ev::Other
                }
                _ => {
                    kinds |= K_OTHER;
                    Ev::Other
                }
            };
            events.push((delta, ev));
        }
    }
    Ok(TrackData { events, name, kinds })
}

/// Ticks per beat from the header's division field.
fn ticks_per_beat(division: [u8; 2]) -> i64 {
    let d = u16::from_be_bytes(division);
    if d & 0x8000 != 0 || d == 0 {
        480
    } else {
        d as i64
    }
}

/// The whole file as mido's `MidiFile` reads it: `clip` clamps data bytes
/// above 127, otherwise one is an error.
pub(crate) fn read_file(data: &[u8], clip: bool) -> R<(i64, Vec<TrackData>)> {
    if data.len() < 14 || &data[..4] != b"MThd" {
        return err("MThd not found. Probably not a MIDI file");
    }
    let hlen = be32(&data[4..8]);
    if hlen < 6 || data.len() < 8 + hlen {
        return err("bad header");
    }
    let ntracks = u16::from_be_bytes([data[10], data[11]]) as usize;
    let tpb = ticks_per_beat([data[12], data[13]]);
    let mut pos = 8 + hlen;
    let mut tracks = Vec::with_capacity(ntracks.min(256));
    while tracks.len() < ntracks {
        if pos + 8 > data.len() {
            return err("unexpected end of file");
        }
        let n = be32(&data[pos + 4..pos + 8]);
        let end = (pos + 8).saturating_add(n).min(data.len());
        if &data[pos..pos + 4] == b"MTrk" {
            tracks.push(read_track(&data[pos..end], clip)?);
        }
        pos = pos.saturating_add(8).saturating_add(n);
    }
    Ok((tpb, tracks))
}

/// `(header, chunks)` of a MIDI file, each chunk with its 8-byte head.
fn chunks(data: &[u8]) -> Option<(&[u8], Vec<&[u8]>)> {
    if data.len() < 8 || &data[..4] != b"MThd" {
        return None;
    }
    let hlen = be32(&data[4..8]);
    let head_end = (8 + hlen).min(data.len());
    let mut i = 8usize.saturating_add(hlen);
    let mut out = Vec::new();
    while i.saturating_add(8) <= data.len() {
        let n = be32(&data[i + 4..i + 8]);
        out.push(&data[i..(i.saturating_add(8).saturating_add(n)).min(data.len())]);
        i = i.saturating_add(8).saturating_add(n);
    }
    Some((&data[..head_end], out))
}

fn division_of(head: &[u8]) -> [u8; 2] {
    if head.len() >= 14 {
        [head[12], head[13]]
    } else {
        [1, 0xE0]
    }
}

fn one_track(head: &[u8], chunk: &[u8], clip: bool) -> R<TrackData> {
    let mut one = b"MThd".to_vec();
    one.extend_from_slice(&6u32.to_be_bytes());
    one.extend_from_slice(&[0, 1, 0, 1]);
    one.extend_from_slice(&division_of(head));
    one.extend_from_slice(chunk);
    let (_, mut tracks) = read_file(&one, clip)?;
    tracks.pop().ok_or_else(|| ReadError("no track".into()))
}

/// The tracks of *data* that read, when one that does not sinks the file.
///
/// A Deltarune arrangement had a stray `0xF4` — a status byte MIDI never
/// defined — in one short track of fifty-nine, and a strict reader refuses
/// the whole file for it. Each track is tried on its own and the broken ones
/// left out.
pub(crate) fn salvage(data: &[u8]) -> R<(i64, Vec<TrackData>)> {
    let Some((head, chunks)) = chunks(data) else {
        return err("not a MIDI file");
    };
    let good: Vec<TrackData> = chunks
        .iter()
        .filter(|c| c.len() >= 4 && &c[..4] == b"MTrk")
        .filter_map(|c| one_track(head, c, true).ok())
        .collect();
    if good.is_empty() {
        return err("no track in the file could be read");
    }
    let d = u16::from_be_bytes(division_of(head)) & 0x7FFF;
    Ok((if d == 0 { 480 } else { d as i64 }, good))
}

// ── Repair ───────────────────────────────────────────────────────────────────

/// The meta types MIDI defines, with the length each has (`None` = any).
fn meta_len(ty: u8) -> Option<Option<usize>> {
    match ty {
        0x00 => Some(Some(2)),
        0x20 | 0x21 => Some(Some(1)),
        0x2F => Some(Some(0)),
        0x51 => Some(Some(3)),
        0x54 => Some(Some(5)),
        0x58 => Some(Some(4)),
        0x59 => Some(Some(2)),
        1..=9 | 0x7F => Some(None),
        _ => None,
    }
}

/// When more than one reading gets to the end, the likeliest is taken. This
/// writer puts a program change first and then only notes, so: the program
/// change first, then notes, then notes with a velocity over 127, then
/// anything else.
const TYPES: [u8; 7] = [9, 8, 12, 11, 14, 13, 10];

fn vlq_at(b: &[u8], mut i: usize) -> (Option<u64>, usize) {
    let mut v: u64 = 0;
    for _ in 0..4 {
        if i >= b.len() {
            break;
        }
        let x = b[i];
        i += 1;
        v = (v << 7) | (x & 0x7F) as u64;
        if x < 0x80 {
            return (Some(v), i);
        }
    }
    (None, i)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Meta,
    Chan(u8),
}

/// Every way the event whose status is `b[p]` reads, as `(kind, end)`, likeliest first.
fn readings(b: &[u8], p: usize, h: u8, first: bool) -> Vec<(Kind, usize)> {
    let s = b[p];
    let mut out: Vec<(i32, usize, Kind, usize)> = Vec::new();
    for (i, &t) in TYPES.iter().enumerate() {
        let end = p + if t == 12 || t == 13 { 2 } else { 3 };
        let note = t == 8 || t == 9;
        if t | h == s >> 4 && end <= b.len() {
            let upto = end - note as usize;
            if b[p + 1..upto].iter().all(|x| *x < 0x80) {
                let rank = if first && t == 12 {
                    -1
                } else if note {
                    (b[end - 1] >= 0x80) as i32
                } else {
                    2
                };
                out.push((rank, i, Kind::Chan(t), end));
            }
        }
    }
    let (ln, q) = if s == 0xFF {
        vlq_at(b, p + 2)
    } else if s == 0xF0 || s == 0xF7 {
        vlq_at(b, p + 1)
    } else {
        (None, 0)
    };
    if let Some(ln) = ln {
        let ln = ln as usize;
        if q + ln <= b.len() {
            let known = s == 0xFF && b.get(p + 1).is_some_and(|t| meta_len(*t).is_some());
            if !known {
                out.push((3, 0, Kind::Meta, q + ln));
            } else {
                let ty = b[p + 1];
                let len_ok = meta_len(ty).flatten().is_none_or(|l| l == ln);
                if len_ok && (ty != 0x2F || q + ln == b.len()) {
                    out.push((-2, 0, Kind::Meta, q + ln));
                }
            }
        }
    }
    out.sort_by_key(|(r, i, _, _)| (*r, *i));
    out.into_iter().map(|(_, _, k, e)| (k, e)).collect()
}

/// *body* rewritten as plain MIDI, read with overflow *h*; `None` if it will not read.
fn unoverflow(body: &[u8], h: u8) -> Option<Vec<u8>> {
    let n = body.len();
    // Forwards: where each event reachable from the start could end. Then
    // backwards: which of those can still get to the end of the track.
    let mut reach = vec![false; n + 1];
    let mut ends: Vec<Option<Vec<usize>>> = vec![None; n + 1];
    reach[0] = true;
    for pos in 0..n {
        if reach[pos] {
            let (d, p) = vlq_at(body, pos);
            let e: Vec<usize> = if d.is_some() && p < n {
                readings(body, p, h, false).into_iter().map(|(_, e)| e).collect()
            } else {
                Vec::new()
            };
            for &x in &e {
                reach[x] = true;
            }
            ends[pos] = Some(e);
        }
    }
    if !reach[n] {
        return None;
    }
    let mut ok = vec![false; n + 1];
    ok[n] = true;
    for pos in (0..n).rev() {
        if let Some(e) = &ends[pos] {
            ok[pos] = e.iter().any(|x| ok[*x]);
        }
    }
    let mut out = Vec::with_capacity(n);
    let (mut pos, mut first) = (0usize, true);
    while pos < n {
        let (_d, p) = vlq_at(body, pos);
        if p >= n {
            return None;
        }
        let (kind, end) = readings(body, p, h, first).into_iter().find(|r| ok[r.1])?;
        first = first && kind == Kind::Meta;
        let data = &body[p + 1..end];
        // The low nibble is the channel. Channel 9 there is not a drum kit —
        // the writer's kits all sit on a true channel 9, and a count of 0x19
        // is its 26th melodic part — so a rewritten track is never drums.
        let lo = body[p] & 0x0F;
        let ch = if lo != DRUM_CHANNEL { lo } else { 8 };
        match kind {
            Kind::Meta => out.extend_from_slice(&body[pos..end]),
            Kind::Chan(t @ (8 | 9)) => {
                // With h odd a note-on and a note-off share one status, and
                // only the velocity tells them apart: this writer releases
                // with 0. It also stacks the same pitch on itself, so
                // "already sounding" would not mean "being released".
                let off = t == 8 || data[1] == 0;
                out.extend_from_slice(&body[pos..p]);
                out.extend_from_slice(&[if off { 0x80 | ch } else { 0x90 | ch }, data[0], data[1].min(127)]);
            }
            Kind::Chan(t) => {
                out.extend_from_slice(&body[pos..p]);
                out.push((t << 4) | ch);
                out.extend_from_slice(data);
            }
        }
        pos = end;
    }
    Some(out)
}

fn plain(head: &[u8], chunk: &[u8]) -> bool {
    match one_track(head, chunk, false) {
        Ok(t) => !t.h1(),
        Err(_) => {
            // A velocity over 127 alone, as in "Chaos King", is no overflow.
            if unoverflow(&chunk[8.min(chunk.len())..], 0).is_none() {
                return false;
            }
            one_track(head, chunk, true).is_ok_and(|t| !t.h1())
        }
    }
}

/// *data* with every channel-overflowed track rewritten as plain MIDI.
pub(crate) fn repair(data: &[u8]) -> Vec<u8> {
    let Some((head, chunks)) = chunks(data) else {
        return data.to_vec();
    };
    let mut fixed: Vec<Vec<u8>> = Vec::with_capacity(chunks.len());
    let mut changed = false;
    for chunk in chunks {
        let mut new = None;
        if chunk.len() >= 4 && &chunk[..4] == b"MTrk" && !plain(head, chunk) {
            new = (1..=7).rev().find_map(|h| unoverflow(&chunk[8.min(chunk.len())..], h));
        }
        match new {
            Some(body) => {
                let mut c = b"MTrk".to_vec();
                c.extend_from_slice(&(body.len() as u32).to_be_bytes());
                c.extend_from_slice(&body);
                fixed.push(c);
                changed = true;
            }
            None => fixed.push(chunk.to_vec()),
        }
    }
    if !changed {
        return data.to_vec();
    }
    let mut out = head.to_vec();
    for c in fixed {
        out.extend_from_slice(&c);
    }
    out
}

/// The file's tracks: read as is when it reads strictly and shows no sign of
/// overflowed channels, otherwise repaired first (clamping data bytes), and
/// finally track by track.
pub(crate) fn read_tracks(data: &[u8]) -> R<(i64, Vec<TrackData>)> {
    if let Ok((tpb, tracks)) = read_file(data, false) {
        if !tracks.iter().any(TrackData::h1) {
            return Ok((tpb, tracks));
        }
    }
    let fixed = repair(data);
    match read_file(&fixed, true) {
        Ok(r) => Ok(r),
        Err(first) => salvage(&fixed).map_err(|e| if e.0.contains("no track") { first } else { e }),
    }
}
