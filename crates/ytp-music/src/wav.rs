//! Minimal 16-bit PCM WAV reading and writing.

use std::fmt;

#[derive(Debug)]
pub struct WavError(pub String);

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wav: {}", self.0)
    }
}

impl std::error::Error for WavError {}

/// A decoded 16-bit PCM file; samples are interleaved.
#[derive(Clone, Debug)]
pub struct Wav {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// `np.clip(np.round(a * 32767.0), -32768, 32767)` — how the golden fixtures were quantised.
pub fn quantize(a: f64) -> i16 {
    (a * 32767.0).round_ties_even().clamp(-32768.0, 32767.0) as i16
}

pub fn quantize_f32(x: &[f32]) -> Vec<i16> {
    x.iter().map(|v| quantize(*v as f64)).collect()
}

/// Parse a RIFF/WAVE file holding 16-bit PCM.
pub fn read(bytes: &[u8]) -> Result<Wav, WavError> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError("not a RIFF/WAVE file".into()));
    }
    let mut pos = 12;
    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + len).min(bytes.len())];
        match id {
            b"fmt " if body.len() >= 16 => {
                channels = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                bits = u16::from_le_bytes([body[14], body[15]]);
            }
            b"data" => data = Some(body),
            _ => {}
        }
        pos += 8 + len + (len & 1);
    }
    if bits != 16 {
        return Err(WavError(format!("expected 16-bit PCM, got {bits} bits")));
    }
    let data = data.ok_or_else(|| WavError("no data chunk".into()))?;
    let samples = data.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    Ok(Wav { sample_rate: rate, channels, samples })
}

/// Serialise 16-bit PCM as a canonical 44-byte-header WAV.
pub fn write(wav: &Wav) -> Vec<u8> {
    let data_len = (wav.samples.len() * 2) as u32;
    let block = wav.channels * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&wav.channels.to_le_bytes());
    out.extend_from_slice(&wav.sample_rate.to_le_bytes());
    out.extend_from_slice(&(wav.sample_rate * block as u32).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &wav.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
