//! Port of `tests/test_midi_overflow.py`: a MIDI file whose writer counted
//! past channel 16, built by hand.
//!
//! The writer ORed its channel count into the status byte, so channel 0x34's
//! program change is 0xF4 — a status MIDI never defined — and its note-on and
//! note-off are both 0xB4, which reads as a controller. That track must still
//! come out as notes, at the right times, and a note nobody ever released
//! must not make the song last as long as its track does.

use ytp_music::music::{parse, MidiError};

fn mtrk(events: &[&[u8]], end: &[u8]) -> Vec<u8> {
    let mut body: Vec<u8> = events.iter().flat_map(|e| e.iter().copied()).collect();
    body.extend_from_slice(end);
    body.extend_from_slice(&[0xFF, 0x2F, 0x00]);
    let mut out = b"MTrk".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Format 1, 480 ticks a beat, 120 BPM.
fn midi(tracks: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"MThd".to_vec();
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, tracks.len() as u8, 0x01, 0xE0]);
    for t in tracks {
        out.extend_from_slice(t);
    }
    out
}

/// A plain piano on channel 0, one beat of middle C.
fn piano() -> Vec<u8> {
    mtrk(&[&[0x00, 0xC0, 0x00, 0x00, 0x90, 60, 90, 0x83, 0x60, 0x80, 60, 0]], &[0])
}

/// Channel 0x34: program change 0xC0|0x34 = 0xF4 (Fingered Bass), notes
/// 0x90|0x34 = 0x80|0x34 = 0xB4, released with velocity 0. One beat of E2,
/// then two of G2, struck with a velocity of 138 — this writer's go over 127.
fn bass() -> Vec<u8> {
    mtrk(
        &[
            b"\x00\xFF\x03\x04Bass",
            &[
                0x00, 0xF4, 33, //
                0x00, 0xB4, 40, 100, 0x83, 0x60, 0xB4, 40, 0, //
                0x00, 0xB4, 43, 0x8A, 0x87, 0x40, 0xB4, 43, 0,
            ],
        ],
        &[0],
    )
}

fn notes(song: &ytp_music::music::Song, track: usize) -> Vec<(f64, f64, u8)> {
    song.parts
        .iter()
        .filter(|p| p.track == track)
        .flat_map(|p| p.notes.iter().map(|n| (n.start, n.dur, n.pitch)))
        .collect()
}

#[test]
fn an_overflowed_track_is_read_as_notes() {
    let s = parse(&midi(&[piano(), bass()]), "overflow").unwrap();
    let b = s.parts.iter().find(|p| p.track == 1).expect("the overflowed track is read");
    let got: Vec<(f64, f64, u8)> = b.notes.iter().map(|n| (n.start, n.dur, n.pitch)).collect();
    assert_eq!(got, vec![(0.0, 0.5, 40), (0.5, 1.0, 43)], "its notes, times and lengths");
    assert_eq!(b.program, Some(33), "its program change");
    assert_eq!(b.notes.last().unwrap().velocity, 127, "a loud note is clamped");
    assert!(!b.is_drums(), "it is not a drum kit");
    assert_eq!(notes(&s, 0), vec![(0.0, 0.5, 60)], "the piano is untouched");
    assert_eq!(s.duration, 1.5, "the song lasts as long as its notes");
}

#[test]
fn a_stuck_note_does_not_stretch_the_song() {
    // One note let go, one never, and the track running on for 100 seconds.
    let stuck = mtrk(
        &[&[0x00, 0x90, 60, 90, 0x83, 0x60, 0x80, 60, 0, 0x00, 0x90, 62, 90]],
        &[0x85, 0xEA, 0x20], // 199 beats of nothing
    );
    let s = parse(&midi(&[stuck]), "stuck").unwrap();
    assert_eq!(s.duration, 4.5);
}

#[test]
fn a_velocity_over_127_alone_is_clamped_not_repaired() {
    let t = mtrk(&[&[0x00, 0x90, 60, 0xC8, 0x83, 0x60, 0x80, 60, 0]], &[0]);
    let s = parse(&midi(&[t]), "loud").unwrap();
    assert_eq!(s.parts[0].notes[0].velocity, 127);
    assert_eq!(s.parts[0].notes[0].pitch, 60);
    assert_eq!(s.duration, 0.5);
}

#[test]
fn an_undefined_status_costs_only_its_own_track() {
    // 0xF4 where no overflow reading can make sense of it: the broken track
    // is left out and the piano still plays.
    let broken = mtrk(&[&[0x00, 0xF4, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]], &[0]);
    let s = parse(&midi(&[piano(), broken]), "broken").unwrap();
    assert_eq!(notes(&s, 0), vec![(0.0, 0.5, 60)]);
}

#[test]
fn not_a_midi_file() {
    assert!(matches!(parse(b"RIFF....WAVEfmt ", "x"), Err(MidiError::NotMidi)));
}

#[test]
fn a_truncated_file_keeps_its_readable_tracks() {
    let mut data = midi(&[piano(), bass()]);
    data.truncate(data.len() - 6);
    let s = parse(&data, "cut").unwrap();
    assert_eq!(notes(&s, 0), vec![(0.0, 0.5, 60)]);
}
