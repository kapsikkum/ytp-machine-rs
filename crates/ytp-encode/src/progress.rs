//! Parsing `ffmpeg -progress pipe:1` into a fraction of the input duration.
//!
//! `-progress` writes `key=value` lines; `out_time_us` (and older builds'
//! `out_time_ms`, which is also microseconds) is the timestamp encoded so far.

#[derive(Debug, Clone)]
pub struct ProgressParser {
    duration: f64,
    last: f32,
}

impl ProgressParser {
    pub fn new(duration: f64) -> Self {
        Self {
            duration,
            last: 0.0,
        }
    }

    /// Feed one line. Returns the fraction (`0..=1`) when the line said
    /// something new.
    pub fn push_line(&mut self, line: &str) -> Option<f32> {
        let (key, value) = line.split_once('=')?;
        let key = key.trim();
        let value = value.trim();
        let frac = match key {
            "out_time_us" | "out_time_ms" => {
                let us: f64 = value.parse().ok()?;
                if self.duration <= 0.0 {
                    return None;
                }
                (us / (self.duration * 1e6)).clamp(0.0, 1.0) as f32
            }
            "out_time" => {
                // HH:MM:SS.microseconds
                let seconds = parse_clock(value)?;
                if self.duration <= 0.0 {
                    return None;
                }
                (seconds / self.duration).clamp(0.0, 1.0) as f32
            }
            "progress" if value == "end" => 1.0,
            _ => return None,
        };
        if (frac - self.last).abs() < 1e-5 && frac != 1.0 {
            return None;
        }
        self.last = frac;
        Some(frac)
    }
}

fn parse_clock(text: &str) -> Option<f64> {
    let mut parts = text.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_and_microseconds() {
        assert_eq!(parse_clock("00:01:02.500000"), Some(62.5));
        let mut p = ProgressParser::new(10.0);
        assert_eq!(p.push_line("out_time_us=2500000"), Some(0.25));
        assert_eq!(p.push_line("out_time_us=2500001"), None);
        assert_eq!(p.push_line("out_time_ms=5000000"), Some(0.5));
        assert_eq!(p.push_line("progress=end"), Some(1.0));
        // clamped past the end
        let mut q = ProgressParser::new(1.0);
        assert_eq!(q.push_line("out_time_us=9999999"), Some(1.0));
        assert_eq!(q.push_line("bitrate=1.2kbits/s"), None);
        assert_eq!(q.push_line("nonsense"), None);
    }
}
