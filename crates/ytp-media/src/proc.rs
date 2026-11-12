//! Running ffmpeg with a bounded wait and cooperative cancellation
//! (`app/media_process.py`).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum RunError {
    Spawn(std::io::Error),
    Timeout,
    Cancelled,
}

#[derive(Debug)]
pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    })
}

/// Run `cmd` to completion. `keep_going` is polled while it runs; returning
/// `false` kills the child. Both output streams are captured.
pub fn run(
    cmd: &mut Command,
    timeout: Duration,
    keep_going: &dyn Fn() -> bool,
) -> Result<Output, RunError> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    if !keep_going() {
        return Err(RunError::Cancelled);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => return Err(RunError::Spawn(e)),
        }
        let stop = if !keep_going() {
            Some(RunError::Cancelled)
        } else if Instant::now() >= deadline {
            Some(RunError::Timeout)
        } else {
            None
        };
        if let Some(e) = stop {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            return Err(e);
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    Ok(Output {
        success: status.success(),
        code: status.code(),
        stdout: out.join().unwrap_or_default(),
        stderr: String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned(),
    })
}

/// Keep the beginning and the end of ffmpeg's complaint: the first error is
/// the one that caused the failure.
pub fn trim_stderr(stderr: &str, limit: usize) -> String {
    let text = stderr.trim();
    if text.len() <= limit {
        return text.to_string();
    }
    let head_end = floor_boundary(text, limit * 2 / 3);
    let tail_start = ceil_boundary(text, text.len() - limit / 3);
    format!(
        "{}\n  … {} chars elided …\n{}",
        &text[..head_end],
        text.len() - limit,
        &text[tail_start..]
    )
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_long_errors_keeping_the_first_line() {
        let long = format!("first error\n{}", "x".repeat(5000));
        let t = trim_stderr(&long, 2000);
        assert!(t.starts_with("first error"));
        assert!(t.contains("chars elided"));
        assert!(t.len() < 2200);
        assert_eq!(trim_stderr("  short ", 2000), "short");
    }
}
