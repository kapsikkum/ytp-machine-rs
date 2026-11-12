//! Parity beyond the golden audio: numpy semantics, the random generator,
//! drum shaping, PSOLA with a held vowel, MIDI analysis and loudness
//! balancing. Fixtures come from `tools/gen_fixtures.py`.

use std::path::PathBuf;

use serde_json::Value;
use ytp_music::np::{self, Rng};
use ytp_music::pitch::{self, Mode};
use ytp_music::{drums, master, music, SR};

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

fn arr(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(num).collect()
}

fn close(got: f64, want: f64, tol: f64, what: &str) {
    assert!((got - want).abs() <= tol * (1.0 + want.abs()), "{what}: got {got}, want {want} (tol {tol})");
}

/// Compare a rendered buffer with a `stats()` record from the generator.
fn check_stats(got: &[f32], want: &Value, tol: f64, what: &str) {
    assert_eq!(got.len() as u64, want["len"].as_u64().unwrap(), "{what}: len");
    let g: Vec<f64> = got.iter().map(|v| *v as f64).collect();
    let peak = g.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let rms = (g.iter().map(|v| v * v).sum::<f64>() / g.len().max(1) as f64).sqrt();
    close(peak, num(&want["peak"]), tol, &format!("{what} peak"));
    close(rms, num(&want["rms"]), tol, &format!("{what} rms"));
    close(g.iter().sum::<f64>() / g.len().max(1) as f64, num(&want["dc"]), tol, &format!("{what} dc"));
    let idx = want["idx"].as_array().unwrap();
    let val = arr(&want["val"]);
    for (i, v) in idx.iter().zip(val.iter()) {
        let i = i.as_u64().unwrap() as usize;
        assert!((g[i] - v).abs() <= tol * (1.0 + v.abs()), "{what}[{i}]: got {}, want {v}", g[i]);
    }
}

fn check_stats_f64(got: &[f64], want: &Value, tol: f64, what: &str) {
    let g: Vec<f32> = got.iter().map(|v| *v as f32).collect();
    // f64 data: compare through the same record without narrowing the values
    assert_eq!(g.len() as u64, want["len"].as_u64().unwrap(), "{what}: len");
    let idx = want["idx"].as_array().unwrap();
    let val = arr(&want["val"]);
    for (i, v) in idx.iter().zip(val.iter()) {
        let i = i.as_u64().unwrap() as usize;
        assert!((got[i] - v).abs() <= tol * (1.0 + v.abs()), "{what}[{i}]: got {}, want {v}", got[i]);
    }
}

#[test]
fn numpy_sums_are_bit_identical() {
    let g = fixture("numpy.json");
    let mut rng = Rng::new(3);
    for case in g["sums"].as_array().unwrap() {
        let n = case["n"].as_u64().unwrap() as usize;
        let a32: Vec<f32> = (0..n).map(|_| (rng.standard_normal() * 0.37) as f32).collect();
        let a64: Vec<f64> = a32.iter().map(|v| *v as f64 * 1.000001).collect();
        assert_eq!(np::sum_f32(&a32) as f64, num(&case["f32_sum"]), "f32 sum n={n}");
        assert_eq!(np::mean_f32(&a32) as f64, num(&case["f32_mean"]), "f32 mean n={n}");
        assert_eq!(np::sum_f64(&a64), num(&case["f64_sum"]), "f64 sum n={n}");
        assert_eq!(np::mean_f64(&a64), num(&case["f64_mean"]), "f64 mean n={n}");
        assert_eq!(np::std_f64(&a64), num(&case["f64_std"]), "f64 std n={n}");
    }
}

#[test]
fn numpy_ramps_and_interp() {
    let g = fixture("numpy.json");
    for case in g["linspace"].as_array().unwrap() {
        let v = np::linspace(num(&case["a"]), num(&case["b"]), case["n"].as_u64().unwrap() as usize);
        assert_eq!(v, arr(&case["v"]));
    }
    assert_eq!(np::hanning(9), arr(&g["hanning"]));
    let i = &g["interp"];
    assert_eq!(np::interp(&arr(&i["x"]), &arr(&i["xp"]), &arr(&i["fp"])), arr(&i["y"]));
    let ia = &g["interp_arange"];
    let y: Vec<f64> = arr(&ia["y"]).iter().map(|v| *v as f32 as f64).collect();
    assert_eq!(np::interp_arange(&arr(&ia["pos"]), &y), arr(&ia["out"]));
}

#[test]
fn numpy_random_matches() {
    let g = fixture("rng.json");
    for (seed, case) in g.as_object().unwrap() {
        let mut rng = Rng::new(seed.parse().unwrap());
        let z = rng.standard_normal_vec(case["n"].as_u64().unwrap() as usize);
        assert_eq!(z[..32].to_vec(), arr(&case["first"]), "seed {seed}: first values");
        let mut h: u64 = 1469598103934665603;
        for v in z.iter().step_by(7) {
            h = (h ^ v.to_bits()).wrapping_mul(1099511628211);
        }
        assert_eq!(h.to_string(), case["fnv_every7"].as_str().unwrap(), "seed {seed}: stream hash");
        close(z.iter().cloned().fold(f64::MIN, f64::max), num(&case["max"]), 0.0, "max");
        close(z.iter().cloned().fold(f64::MAX, f64::min), num(&case["min"]), 0.0, "min");
    }
}

fn word() -> Vec<f32> {
    let n = (0.5 * SR as f64) as usize;
    let r1 = Rng::new(5).standard_normal_vec(n);
    let r2 = Rng::new(6).standard_normal_vec(n);
    let mut x = vec![0.0f32; (0.03 * SR as f64) as usize];
    for i in 0..n {
        let t = i as f64 / SR as f64;
        let burst = 0.6 * (-t / 0.01).exp() * r1[i];
        let vowel = 0.4
            * (2.0 * std::f64::consts::PI * 140.0 * t).sin()
            * (if t > 0.05 { 1.0 } else { 0.0 })
            * (-(t - 0.05).max(0.0) / 0.2).exp();
        let hiss = 0.3 * (if t > 0.25 && t < 0.4 { 1.0 } else { 0.0 }) * r2[i];
        x.push((burst + vowel + hiss) as f32);
    }
    x
}

#[test]
fn drum_shaping() {
    let g = fixture("drums.json");
    let x = word();
    assert_eq!(x.len() as u64, g["input"]["len"].as_u64().unwrap());
    assert_eq!(drums::onset(&x) as u64, g["onset"].as_u64().unwrap());
    let hiss = drums::find_hiss(&x, 0.35).map(|(a, b)| vec![a as u64, b as u64]);
    let want: Option<Vec<u64>> = g["hiss"].as_array().map(|a| a.iter().map(|v| v.as_u64().unwrap()).collect());
    assert_eq!(hiss, want);
    let (share, level) = drums::hiss_profile(&x);
    check_stats_f64(&share, &g["profile"]["share"], 1e-9, "share");
    check_stats_f64(&level, &g["profile"]["level"], 1e-9, "level");
    for group in drums::GROUPS {
        let (y, start) = drums::shape(&x, group);
        assert_eq!(start as u64, g[group]["start"].as_u64().unwrap(), "{group} start");
        // float32 FFT and tanh in numpy vs double here: a few float32 ulps
        check_stats(&y, &g[group], 2e-4, group);
    }
}

fn glide() -> Vec<f32> {
    let n = (0.4 * SR as f64) as usize;
    let tau = 2.0 * std::f64::consts::PI;
    let mut cum = 0.0f64;
    let mut body = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / SR as f64;
        let f0 = 140.0 + 40.0 * t / 0.4 + 3.0 * (tau * 5.0 * t).sin();
        cum += f0;
        let ph = tau * cum / SR as f64;
        let mut x = 0.0f64;
        for (k, a) in [(1.0, 1.0), (2.0, 0.5), (3.0, 0.3), (4.0, 0.15)] {
            x += a * (k * ph).sin();
        }
        body.push(x);
    }
    let peak = body.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let mut out = vec![0.0f32; (0.02 * SR as f64) as usize];
    for (i, x) in body.iter().enumerate() {
        let t = i as f64 / SR as f64;
        let env = (t / 0.03).min(1.0) * ((0.4 - t) / 0.05).min(1.0);
        out.push((0.4 * x * env / peak) as f32);
    }
    out
}

#[test]
fn pitch_marks_and_psola() {
    let g = fixture("pitch.json");
    let x = glide();
    assert_eq!(x.len() as u64, g["input"]["len"].as_u64().unwrap());
    let v = pitch::Voice::new(&x, SR);
    close(v.info.f0.unwrap(), num(&g["info"]["f0"]), 1e-9, "f0");
    assert_eq!(v.info.note.as_deref(), g["info"]["note"].as_str());
    close(v.info.voiced_ratio, num(&g["info"]["voiced_ratio"]), 1e-9, "voiced_ratio");
    close(v.info.stability, num(&g["info"]["stability"]), 1e-6, "stability");

    let tr = pitch::track_f0_default(&x, SR);
    assert_eq!(tr.f0.len() as u64, g["track"]["frames"].as_u64().unwrap());
    assert_eq!(tr.voiced.iter().filter(|v| **v).count() as u64, g["track"]["voiced"].as_u64().unwrap());
    check_stats_f64(&tr.f0, &g["track"]["f0"], 1e-9, "track f0");
    check_stats_f64(&tr.rms, &g["track"]["rms"], 1e-9, "track rms");

    let m = &v.marks;
    assert_eq!(m.marks.len() as u64, g["marks"]["n"].as_u64().unwrap(), "mark count");
    let first: Vec<i64> = g["marks"]["first"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(m.marks[..first.len()].to_vec(), first, "first marks");
    for (a, b) in m.periods.iter().zip(arr(&g["marks"]["periods"]).iter()) {
        close(*a, *b, 1e-9, "period");
    }
    assert_eq!(m.voiced.iter().filter(|v| **v).count() as u64, g["marks"]["voiced_count"].as_u64().unwrap());
    close(v.v0, num(&g["v0"]), 1e-12, "v0");
    close(v.v1, num(&g["v1"]), 1e-12, "v1");
    let pk = v.peak().unwrap();
    let want = arr(&g["peak"]);
    close(pk.0, want[0], 1e-9, "peak at");
    close(pk.1, want[1], 1e-9, "peak period");

    for (name, case) in g["renders"].as_object().unwrap() {
        let hz = case["hz"].as_f64();
        let mode = Mode::parse(case["mode"].as_str().unwrap());
        let (y, w) = v.render(hz, num(&case["dur"]), mode, num(&case["semis"]), case["stretch"].as_bool().unwrap());
        close(w.hold, num(&case["warp"]["hold"]), 1e-9, &format!("{name} hold"));
        close(w.rate, num(&case["warp"]["rate"]), 1e-9, &format!("{name} rate"));
        close(w.v0, num(&case["warp"]["v0"]), 1e-9, &format!("{name} v0"));
        close(w.p, num(&case["warp"]["P"]), 1e-9, &format!("{name} P"));
        check_stats(&y, case, 1e-5, name);
    }
}

#[test]
fn midi_analysis() {
    let g = fixture("music.json");
    let data = std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/song.mid")).unwrap();
    let song = music::parse(&data, "fixture").unwrap();
    close(song.duration, num(&g["duration"]), 1e-9, "duration");
    close(song.bpm, num(&g["bpm"]), 1e-9, "bpm");
    assert_eq!(song.time_signature.0 as u64, g["time_signature"][0].as_u64().unwrap());
    assert_eq!(song.time_signature.1 as u64, g["time_signature"][1].as_u64().unwrap());
    assert_eq!(song.key, g["key"].as_str().unwrap());
    close(song.key_confidence, num(&g["key_confidence"]), 1e-9, "key confidence");
    assert_eq!(song.tempo_changes as u64, g["tempo_changes"].as_u64().unwrap());
    assert_eq!(song.describe(), g["description"].as_str().unwrap());

    let want_parts = g["parts"].as_array().unwrap();
    assert_eq!(song.parts.len(), want_parts.len());
    for (p, w) in song.parts.iter().zip(want_parts) {
        let s = p.summary();
        assert_eq!(s.id, w["id"].as_str().unwrap());
        assert_eq!(s.name, w["name"].as_str().unwrap());
        assert_eq!(s.role, w["role"].as_str().unwrap());
        assert_eq!(s.instrument, w["instrument"].as_str().unwrap());
        assert_eq!(s.is_drums, w["is_drums"].as_bool().unwrap());
        assert_eq!(s.notes as u64, w["notes"].as_u64().unwrap());
        assert_eq!(s.median_midi, num(&w["median_midi"]));
        assert_eq!(s.common_midi as u64, w["common_midi"].as_u64().unwrap());
        assert_eq!(s.polyphony as u64, w["polyphony"].as_u64().unwrap());
        close(s.typical_polyphony, num(&w["typical_polyphony"]), 1e-9, "typical polyphony");
        assert_eq!(s.pan, w["pan"].as_f64());
        assert_eq!(s.first, w["first"].as_f64());
        assert_eq!(p.program.map(|v| v as u64), w["program"].as_u64());
        for (n, h) in p.notes.iter().zip(w["notes_head"].as_array().unwrap()) {
            let h = arr(h);
            close(n.start, h[0], 1e-12, "note start");
            close(n.dur, h[1], 1e-12, "note dur");
            assert_eq!(n.pitch as f64, h[2]);
            assert_eq!(n.velocity as f64, h[3]);
        }
    }
}

#[test]
fn midi_garbage_is_an_error() {
    assert!(music::parse(b"not midi at all", "x").is_err());
    assert!(music::parse(b"", "x").is_err());
}

#[test]
fn loudness_and_balance() {
    let g = fixture("master.json");
    let n = 6 * SR as usize;
    let mono: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            let gate = if (2.0 * std::f64::consts::PI * 0.7 * t).sin() > -0.3 { 1.0 } else { 0.0 };
            0.2 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() * gate
        })
        .collect();
    let bp = master::block_power(&mono, 1, SR);
    check_stats_f64(&bp, &g["block_power"], 1e-9, "block_power");
    close(master::loudness(&mono, 1, SR), num(&g["loudness"]), 1e-9, "loudness");
    let kw = master::k_weight(&mono[..SR as usize], SR);
    check_stats_f64(&kw, &g["k_weight"], 1e-9, "k_weight");

    // parts
    let mut parts = Vec::new();
    for p in g["balance_inputs"].as_array().unwrap() {
        // rebuild the same synthetic parts the generator measured
        let i: usize = p["id"].as_str().unwrap()[1..].parse().unwrap();
        let amp = [0.2, 0.3, 0.05, 0.4, 0.1, 0.12][i];
        let x: Vec<f64> = (0..n)
            .map(|k| {
                let t = k as f64 / SR as f64;
                let env = if (2.0 * std::f64::consts::PI * (0.3 + 0.11 * i as f64) * t).sin() > -0.2 { 1.0 } else { 0.0 };
                amp * env * (2.0 * std::f64::consts::PI * (110.0 * (i + 1) as f64) * t).sin()
            })
            .collect();
        let power = master::block_power(&x, 1, SR);
        check_stats_f64(&power, &p["power"], 1e-9, "part power");
        parts.push(master::PartPower {
            id: p["id"].as_str().unwrap().to_string(),
            role: p["role"].as_str().unwrap().to_string(),
            power,
            trim_db: num(&p["trim_db"]),
        });
    }
    let bal = master::balance(&parts);
    for (id, want) in g["balance"].as_object().unwrap() {
        let got = &bal[id];
        close(got.lufs, num(&want["lufs"]), 1e-9, &format!("{id} lufs"));
        close(got.gain, num(&want["gain"]), 1e-9, &format!("{id} gain"));
        check_stats_f64(&got.ride, &want["ride"], 1e-9, &format!("{id} ride"));
    }

    // limiter
    let t_len = 2 * SR as usize;
    let mut hot: Vec<f64> =
        (0..t_len).map(|i| 1.6 * (2.0 * std::f64::consts::PI * 200.0 * (i as f64 / SR as f64)).sin()).collect();
    for v in &mut hot[SR as usize..SR as usize + 200] {
        *v *= 3.0;
    }
    master::limit(&mut hot, 1, master::CEILING, SR);
    check_stats_f64(&hot, &g["limit"]["out"], 1e-9, "limit");

    let env = master::envelope(&[1.0, 0.5, 0.25], 100, 40000, SR);
    check_stats(&env, &g["envelope"], 1e-6, "envelope");
}

#[test]
fn float32_rfft_rounds_like_numpy() {
    let g = fixture("fft32.json");
    let mut rng = Rng::new(17);
    for case in g.as_array().unwrap() {
        let n = case["n"].as_u64().unwrap() as usize;
        let n_in = case["n_in"].as_u64().unwrap() as usize;
        let x: Vec<f32> = (0..n_in).map(|_| (rng.standard_normal() * 0.5) as f32).collect();
        let spec: Vec<(f32, f32)> = np::rfft_f32(&x, n).iter().map(|c| (c.re as f32 + 0.0, c.im as f32 + 0.0)).collect();
        assert_eq!(spec.len(), n / 2 + 1);
        let mut h: u64 = 1469598103934665603;
        for (r, _) in &spec {
            h = (h ^ r.to_bits() as u64).wrapping_mul(1099511628211);
        }
        for (_, i) in &spec {
            h = (h ^ i.to_bits() as u64).wrapping_mul(1099511628211);
        }
        let re: Vec<u64> = case["re"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        let im: Vec<u64> = case["im"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        for k in 0..re.len().min(spec.len()) {
            assert_eq!(spec[k].0.to_bits() as u64, re[k], "n={n} n_in={n_in} re[{k}]");
            assert_eq!(spec[k].1.to_bits() as u64, im[k], "n={n} n_in={n_in} im[{k}]");
        }
        assert_eq!(h.to_string(), case["hash"].as_str().unwrap(), "n={n} n_in={n_in}: spectrum hash");
    }
}
