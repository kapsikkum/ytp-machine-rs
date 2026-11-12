//! Target-size maths: how much of a byte budget the video may have, and how
//! far the resolution has to come down to spend it well.

/// Container overhead and the safety margin under the target (4 %).
pub const CONTAINER_MARGIN: f64 = 0.04;
/// Below this many bits per pixel per frame H.264 looks like mush, so the
/// resolution steps down instead of the bitrate going lower.
pub const MIN_BITS_PER_PIXEL: f64 = 0.045;
/// Never ask libx264 for less than this.
pub const MIN_VIDEO_KBPS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetPlan {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_kbps: u32,
    pub target_bytes: u64,
    /// The resolution or frame rate had to come down to fit.
    pub scaled: bool,
}

/// The video bitrate that fits `target_bytes` over `duration`, after the audio
/// and a container margin.
pub fn target_video_kbps(target_bytes: u64, duration: f64, audio_kbps: u32, margin: f64) -> f64 {
    if duration <= 0.0 {
        return 0.0;
    }
    let usable = target_bytes as f64 * 8.0 * (1.0 - margin);
    let audio = audio_kbps as f64 * 1000.0 * duration;
    ((usable - audio) / duration / 1000.0).max(0.0)
}

pub fn bits_per_pixel(video_kbps: u32, width: u32, height: u32, fps: f64) -> f64 {
    if width == 0 || height == 0 || fps <= 0.0 {
        return 0.0;
    }
    video_kbps as f64 * 1000.0 / (width as f64 * height as f64 * fps)
}

fn even(v: f64) -> u32 {
    ((v.round() as i64).max(2) as u32) & !1
}

/// Scale `w`x`h` down to fit `max_w`x`max_h` (0 = no limit), keeping the
/// aspect ratio and even dimensions. Never upscales.
pub fn fit_even(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (w & !1, h & !1);
    }
    let sw = if max_w > 0 {
        (w as f64).min(max_w as f64)
    } else {
        w as f64
    };
    let sh = if max_h > 0 {
        (h as f64).min(max_h as f64)
    } else {
        h as f64
    };
    if sw >= w as f64 && sh >= h as f64 {
        return (even(w as f64), even(h as f64));
    }
    let k = (sw / w as f64).min(sh / h as f64);
    (even(w as f64 * k), even(h as f64 * k))
}

pub fn cap_fps(src: f64, cap: f64) -> f64 {
    let src = if src > 0.0 { src } else { 30.0 };
    if cap > 0.0 && src > cap {
        cap
    } else {
        src
    }
}

/// Step the resolution down until the video bitrate is not absurdly low for
/// it. `fps` stays at the capped source rate.
pub fn plan_target_size(
    duration: f64,
    target_bytes: u64,
    audio_kbps: u32,
    src_w: u32,
    src_h: u32,
    src_fps: f64,
    max_w: u32,
    max_h: u32,
    fps_cap: f64,
) -> Result<TargetPlan, String> {
    if duration <= 0.0 {
        return Err("the input has no readable duration".into());
    }
    let total = target_video_kbps(target_bytes, duration, audio_kbps, CONTAINER_MARGIN);
    let usable = target_bytes as f64 * 8.0 * (1.0 - CONTAINER_MARGIN);
    let audio = audio_kbps as f64 * 1000.0 * duration;
    if audio >= usable * 0.95 || total < 1.0 {
        return Err(format!(
            "{duration:.0} s cannot fit in {:.1} MB even with no picture; pick a larger target or a shorter clip",
            target_bytes as f64 / 1_000_000.0
        ));
    }
    let fps = cap_fps(src_fps, fps_cap);
    let (mut w, h) = fit_even(src_w, src_h, max_w, max_h);
    let start = (w, h);
    let mut candidates = vec![(w, h, fps)];
    for cand_h in [720u32, 480, 360, 270, 180] {
        if cand_h >= h {
            continue;
        }
        let cand_w = even(w as f64 * cand_h as f64 / h as f64);
        candidates.push((cand_w, cand_h, fps));
    }
    let mut chosen = *candidates.last().unwrap();
    for (cw, ch, cfps) in &candidates {
        let kbps = total.round() as u32;
        if bits_per_pixel(kbps, *cw, *ch, *cfps) >= MIN_BITS_PER_PIXEL {
            chosen = (*cw, *ch, *cfps);
            break;
        }
    }
    let video_kbps = (total.round() as u32).max(MIN_VIDEO_KBPS);
    let scaled = chosen.0 != start.0 || chosen.1 != start.1;
    w = chosen.0;
    let h = chosen.1;
    Ok(TargetPlan {
        width: w,
        height: h,
        fps: chosen.2,
        video_kbps,
        target_bytes,
        scaled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_math_reserves_audio_and_overhead() {
        // 10 MB over 60 s with 96 kbps audio: (10e6*8*0.96 - 96000*60)/60/1000
        let kbps = target_video_kbps(10_000_000, 60.0, 96, CONTAINER_MARGIN);
        assert!((kbps - 1184.0).abs() < 1.0, "{kbps}");
        assert_eq!(target_video_kbps(10_000_000, 0.0, 96, CONTAINER_MARGIN), 0.0);
        // the audio alone is bigger than a 0.5 MB target over 60 s
        assert_eq!(target_video_kbps(500_000, 60.0, 128, CONTAINER_MARGIN), 0.0);
    }

    #[test]
    fn fit_keeps_the_aspect_ratio_and_even_sizes() {
        assert_eq!(fit_even(1920, 1080, 1920, 720), (1280, 720));
        assert_eq!(fit_even(1920, 1080, 854, 480), (852, 480));
        assert_eq!(fit_even(1920, 1080, 0, 0), (1920, 1080));
        assert_eq!(fit_even(1280, 720, 3840, 2160), (1280, 720));
        assert_eq!(fit_even(481, 271, 0, 0), (480, 270));
    }

    #[test]
    fn a_low_target_steps_the_resolution_down() {
        // 3 MB over 120 s with 96 kbps audio: ~96 kbps for video, so 1080p is
        // hopeless and 180p is the first that is not mush.
        let plan = plan_target_size(120.0, 3_000_000, 96, 1920, 1080, 30.0, 1920, 1080, 30.0).unwrap();
        assert!(plan.scaled, "{plan:?}");
        assert!(plan.width < 1920 && plan.height < 1080, "{plan:?}");
        assert!(bits_per_pixel(plan.video_kbps, plan.width, plan.height, plan.fps) >= MIN_BITS_PER_PIXEL);
        // A roomy target keeps the source size.
        let plan = plan_target_size(30.0, 50_000_000, 128, 1920, 1080, 30.0, 1920, 1080, 30.0).unwrap();
        assert_eq!((plan.width, plan.height), (1920, 1080));
        assert!(!plan.scaled);
        // A tiny target for a long clip is refused with something readable.
        let e = plan_target_size(600.0, 1_000_000, 192, 1920, 1080, 30.0, 1920, 1080, 30.0).unwrap_err();
        assert!(e.contains("cannot fit"), "{e}");
    }
}
