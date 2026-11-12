//! Render-speed bench: `cargo bench -p ytp-music --bench render`.
//!
//! For each chip it renders the same four-note sequence the parity tests use
//! (C3 G3 C4 E4, 0.18 s each; the SNES gets its two 0.05 s notes), repeatedly,
//! and reports the realtime factor: seconds of audio produced per second of
//! wall-clock time (higher is faster; 1.0 is exactly realtime). The audio
//! length is what the chip returns, tails included. Also times the other
//! stages (drum shaping, mastering, pitch tracking, PSOLA).

use std::hint::black_box;
use std::time::{Duration, Instant};

use ytp_music::chip::{chip_for, NoteEvent};
use ytp_music::pitch::{self, Mode};
use ytp_music::{drums, master, SR};

fn measure(label: &str, mut f: impl FnMut() -> f64) {
    // warm up (also fills the lazily built tables), then run for ~1 s
    black_box(f());
    let start = Instant::now();
    let mut audio = 0.0f64;
    let mut runs = 0u32;
    while start.elapsed() < Duration::from_millis(1000) || runs < 3 {
        audio += black_box(f());
        runs += 1;
    }
    let wall = start.elapsed().as_secs_f64();
    println!(
        "{label:<22} {:>9.1}x realtime   ({runs} runs, {:.2} ms/run, {:.2} s audio/run)",
        audio / wall,
        wall * 1000.0 / runs as f64,
        audio / runs as f64
    );
}

fn main() {
    println!("ytp-music render bench (release, single thread)");
    let cases: [(&str, &str, &str, &[i32], f64); 8] = [
        ("nes", "nes", "pulse", &[48, 55, 60, 64], 0.18),
        ("gb", "gb", "gb-pulse", &[48, 55, 60, 64], 0.18),
        ("sms", "sms", "psg", &[48, 55, 60, 64], 0.18),
        ("sid", "sid", "sid-lead", &[48, 55, 60, 64], 0.18),
        ("opl2", "opl2", "opl2", &[48, 55, 60, 64], 0.18),
        ("opl3", "opl3", "opl3", &[48, 55, 60, 64], 0.18),
        ("ym2612", "md", "lead", &[48, 55, 60, 64], 0.18),
        ("snes", "snes", "snes", &[48, 60], 0.05),
    ];
    for (label, machine, voice, notes, secs) in cases {
        let chip = chip_for(machine).unwrap();
        measure(label, || {
            let mut n = 0usize;
            for m in notes {
                let mut ev = NoteEvent::from_midi(*m as f64, secs);
                ev.program = Some(0);
                n += chip.render(voice, &ev).unwrap().len();
            }
            n as f64 / SR as f64
        });
    }

    // the other stages
    let x: Vec<f32> = (0..SR as usize / 2)
        .map(|i| (0.4 * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / SR as f64).sin()) as f32)
        .collect();
    measure("drums.shape (kick)", || {
        black_box(drums::shape(&x, "kick"));
        x.len() as f64 / SR as f64
    });
    measure("pitch.detect (0.5 s)", || {
        black_box(pitch::detect_pitch(&x, SR));
        x.len() as f64 / SR as f64
    });
    let voice = pitch::Voice::new(&x, SR);
    measure("psola render (0.3 s)", || {
        black_box(voice.render(Some(262.0), 0.3, Mode::Perfect, 0.0, true));
        0.3
    });
    measure("master.finish (stereo)", || {
        let n = 2 * SR as usize;
        let mut mix: Vec<f64> = (0..n * 2).map(|i| 0.3 * ((i / 2) as f64 * 0.05).sin()).collect();
        master::finish(&mut mix, 2, SR);
        black_box(&mix);
        n as f64 / SR as f64
    });
}
