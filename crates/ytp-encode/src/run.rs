//! Running ffmpeg: capture, `-progress pipe:1` parsing and cooperative
//! cancellation.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::progress::ProgressParser;
use crate::{CancelFn, ProgressFn};

#[derive(Debug)]
pub(crate) enum RunError {
    Spawn(std::io::Error),
    Timeout,
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct Output {
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

/// Run a command to completion, capturing both streams. Cancelling kills it.
pub(crate) fn capture(cmd: &mut Command, timeout: Duration) -> Result<Output, RunError> {
    prepare(cmd);
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
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            return Err(RunError::Timeout);
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

#[derive(Debug)]
pub(crate) struct Ticked {
    pub success: bool,
    pub code: Option<i32>,
    pub stderr: String,
}

/// Run ffmpeg with `-progress pipe:1`, turning `out_time_us` into a fraction
/// of `duration`. `progress` is called while it runs; `cancel` returning
/// `true` kills the child and yields [`RunError::Cancelled`].
pub(crate) fn run_ticked(
    program: &Path,
    args: &[String],
    duration: f64,
    progress: ProgressFn,
    cancel: CancelFn,
    timeout: Duration,
) -> Result<Ticked, RunError> {
    if cancel() {
        return Err(RunError::Cancelled);
    }
    let mut cmd = Command::new(program);
    cmd.args(args);
    prepare(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    let stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel::<f32>();
    let reader = std::thread::spawn(move || {
        let mut parser = ProgressParser::new(duration);
        if let Some(out) = stdout {
            let mut reader = BufReader::new(out);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&line);
                        if let Some(f) = parser.push_line(text.trim_end()) {
                            if tx.send(f).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    });
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let mut last = -1.0f32;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                let _ = err.join();
                return Err(RunError::Spawn(e));
            }
        }
        let mut fresh = false;
        while let Ok(f) = rx.try_recv() {
            last = f;
            fresh = true;
        }
        if fresh {
            progress(last.max(0.0));
        }
        if cancel() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = err.join();
            return Err(RunError::Cancelled);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = err.join();
            return Err(RunError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    let _ = reader.join();
    while let Ok(f) = rx.try_recv() {
        last = f;
    }
    progress(last.max(0.0));
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
    let status = status.expect("the loop only breaks with a status");
    Ok(Ticked {
        success: status.success(),
        code: status.code(),
        stderr,
    })
}

fn prepare(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
}

/// Keep the beginning and the end of ffmpeg's complaint.
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
