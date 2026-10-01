//! Waveform peak extraction for the detail-panel player.
//!
//! Streams PCM samples (never buffers the whole `data` chunk) and downsamples
//! into `buckets` peak amplitudes in `0.0..=1.0`. Compressed formats are not
//! decoded here — the UI may fall back to Web Audio and cache the result.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{Error, Result};

pub const DEFAULT_BUCKETS: usize = 480;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeaksCache {
    pub version: u32,
    /// "pcm" when computed in-core, "web_audio" when the UI decoded it.
    pub source: String,
    pub peaks: Vec<f32>,
}

/// Compute peak amplitudes from a WAV (RIFF/PCM/IEEE float) file.
pub fn compute_wav_peaks(path: &Path, buckets: usize) -> Result<Vec<f32>> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);

    let mut riff = [0u8; 12];
    r.read_exact(&mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(Error::other("not a RIFF/WAVE file"));
    }

    let mut format: u16 = 1;
    let mut channels: u16 = 1;
    let mut bits: u16 = 16;
    let mut data_offset: Option<u64> = None;
    let mut data_size: u64 = 0;

    loop {
        let mut hdr = [0u8; 8];
        if r.read_exact(&mut hdr).is_err() {
            break;
        }
        let id = &hdr[0..4];
        let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
        let padded = size + (size % 2);

        if id == b"fmt " {
            let mut fmt = vec![0u8; size.min(40) as usize];
            r.read_exact(&mut fmt)?;
            if padded > fmt.len() as u64 {
                std::io::copy(
                    &mut r.by_ref().take(padded - fmt.len() as u64),
                    &mut std::io::sink(),
                )?;
            }
            if fmt.len() >= 16 {
                format = u16::from_le_bytes([fmt[0], fmt[1]]);
                channels = u16::from_le_bytes([fmt[2], fmt[3]]);
                bits = u16::from_le_bytes([fmt[14], fmt[15]]);
                // WAVE_FORMAT_EXTENSIBLE: real format lives in the subformat GUID.
                if format == 0xFFFE && fmt.len() >= 26 {
                    format = u16::from_le_bytes([fmt[24], fmt[25]]);
                }
            }
            continue;
        }

        if id == b"data" {
            let pos = r.stream_position()?;
            data_offset = Some(pos);
            data_size = size;
            break;
        }

        std::io::copy(&mut r.by_ref().take(padded), &mut std::io::sink())?;
    }

    let (offset, size) = match (data_offset, data_size) {
        (Some(o), s) if s > 0 => (o, s),
        _ => return Err(Error::other("WAV has no audio data chunk")),
    };
    if channels == 0 || bits == 0 {
        return Err(Error::other("WAV fmt has zero channels/bits"));
    }
    // Only integer PCM and IEEE float are sample-decodable without a codec.
    if !matches!(format, 1 | 3) {
        return Err(Error::UnsupportedFormat(format!(
            "wav format code {format}"
        )));
    }

    r.seek(SeekFrom::Start(offset))?;
    peaks_from_pcm(&mut r, size, channels, bits, format == 3, buckets)
}

/// Compute peak amplitudes from an uncompressed AIFF (PCM) file.
pub fn compute_aiff_peaks(path: &Path, buckets: usize) -> Result<Vec<f32>> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);

    let mut header = [0u8; 12];
    r.read_exact(&mut header)?;
    if &header[0..4] != b"FORM" {
        return Err(Error::other("not an AIFF file"));
    }

    let mut channels: u16 = 1;
    let mut bits: u16 = 16;
    let mut data_offset: Option<u64> = None;
    let mut data_size: u64 = 0;

    loop {
        let mut hdr = [0u8; 8];
        if r.read_exact(&mut hdr).is_err() {
            break;
        }
        let id = &hdr[0..4];
        let size = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
        let padded = size + (size % 2);

        if id == b"COMM" {
            let mut data = vec![0u8; size.min(26) as usize];
            r.read_exact(&mut data)?;
            if padded > data.len() as u64 {
                std::io::copy(
                    &mut r.by_ref().take(padded - data.len() as u64),
                    &mut std::io::sink(),
                )?;
            }
            if data.len() >= 8 {
                channels = u16::from_be_bytes([data[0], data[1]]);
                bits = u16::from_be_bytes([data[6], data[7]]);
            }
            continue;
        }

        if id == b"SSND" {
            // SSND: offset(4) + blockSize(4) + samples
            let mut lead = [0u8; 8];
            r.read_exact(&mut lead)?;
            let offset = u32::from_be_bytes([lead[0], lead[1], lead[2], lead[3]]) as u64;
            let pos = r.stream_position()? + offset;
            data_offset = Some(pos);
            data_size = size.saturating_sub(8 + offset);
            break;
        }

        std::io::copy(&mut r.by_ref().take(padded), &mut std::io::sink())?;
    }

    let (offset, size) = match (data_offset, data_size) {
        (Some(o), s) if s > 0 => (o, s),
        _ => return Err(Error::other("AIFF has no SSND samples")),
    };
    if channels == 0 || bits == 0 {
        return Err(Error::other("AIFF COMM has zero channels/bits"));
    }

    r.seek(SeekFrom::Start(offset))?;
    peaks_from_pcm_be(&mut r, size, channels, bits, buckets)
}

/// Dispatch peak extraction by extension (WAV/AIFF only).
pub fn compute_file_peaks(path: &Path, buckets: usize) -> Result<Vec<f32>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "wav" | "bwf" => compute_wav_peaks(path, buckets),
        "aif" | "aiff" => compute_aiff_peaks(path, buckets),
        other => Err(Error::UnsupportedFormat(format!("peaks for .{other}"))),
    }
}

fn peaks_from_pcm<R: Read>(
    r: &mut R,
    data_size: u64,
    channels: u16,
    bits: u16,
    is_float: bool,
    buckets: usize,
) -> Result<Vec<f32>> {
    let bytes_per_sample = bits.div_ceil(8) as u64;
    let frame_bytes = bytes_per_sample * channels as u64;
    if frame_bytes == 0 {
        return Err(Error::other("zero frame size"));
    }
    let total_frames = data_size / frame_bytes;
    if total_frames == 0 {
        return Ok(vec![0.0; buckets.max(1)]);
    }

    let buckets = buckets.max(1);
    let mut peaks = vec![0.0f32; buckets];
    // ~64 KiB read buffer.
    let mut buf = vec![0u8; 64 * 1024];
    let mut frames_seen: u64 = 0;
    let mut remaining = data_size;

    while remaining > 0 {
        let want = buf.len().min(remaining as usize);
        let n = r.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        remaining -= n as u64;
        let mut i = 0usize;
        while i + frame_bytes as usize <= n {
            let mut frame_peak = 0.0f32;
            for ch in 0..channels as usize {
                let s0 = i + ch * bytes_per_sample as usize;
                if s0 + bytes_per_sample as usize > n {
                    break;
                }
                let amp = sample_to_f32(&buf[s0..s0 + bytes_per_sample as usize], bits, is_float);
                frame_peak = frame_peak.max(amp.abs());
            }
            let bucket =
                ((frames_seen * buckets as u64) / total_frames).min(buckets as u64 - 1) as usize;
            if frame_peak > peaks[bucket] {
                peaks[bucket] = frame_peak;
            }
            frames_seen += 1;
            i += frame_bytes as usize;
        }
    }

    Ok(peaks)
}

fn peaks_from_pcm_be<R: Read>(
    r: &mut R,
    data_size: u64,
    channels: u16,
    bits: u16,
    buckets: usize,
) -> Result<Vec<f32>> {
    // AIFF stores big-endian integers. Read frames and swap per sample width.
    let bytes_per_sample = bits.div_ceil(8) as u64;
    let frame_bytes = bytes_per_sample * channels as u64;
    if frame_bytes == 0 {
        return Err(Error::other("zero frame size"));
    }
    let total_frames = data_size / frame_bytes;
    if total_frames == 0 {
        return Ok(vec![0.0; buckets.max(1)]);
    }

    let buckets = buckets.max(1);
    let mut peaks = vec![0.0f32; buckets];
    let mut buf = vec![0u8; 64 * 1024];
    let mut frames_seen: u64 = 0;
    let mut remaining = data_size;

    while remaining > 0 {
        let want = buf.len().min(remaining as usize);
        let n = r.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        remaining -= n as u64;
        let mut i = 0usize;
        while i + frame_bytes as usize <= n {
            let mut frame_peak = 0.0f32;
            for ch in 0..channels as usize {
                let s0 = i + ch * bytes_per_sample as usize;
                if s0 + bytes_per_sample as usize > n {
                    break;
                }
                let mut tmp = [0u8; 4];
                let w = bytes_per_sample as usize;
                for k in 0..w {
                    tmp[k] = buf[s0 + w - 1 - k];
                }
                let amp = sample_to_f32(&tmp[..w], bits, false);
                frame_peak = frame_peak.max(amp.abs());
            }
            let bucket =
                ((frames_seen * buckets as u64) / total_frames).min(buckets as u64 - 1) as usize;
            if frame_peak > peaks[bucket] {
                peaks[bucket] = frame_peak;
            }
            frames_seen += 1;
            i += frame_bytes as usize;
        }
    }

    Ok(peaks)
}

fn sample_to_f32(b: &[u8], bits: u16, is_float: bool) -> f32 {
    if is_float {
        return match (b.len(), bits) {
            (4, _) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (8, _) => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
            _ => 0.0,
        };
    }
    match (b.len(), bits) {
        (1, _) => (b[0] as f32 - 128.0) / 128.0,
        (2, _) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
        (3, _) => {
            let v = i32::from_le_bytes([b[0], b[1], b[2], 0]) >> 8;
            v as f32 / 8_388_608.0
        }
        (4, 32) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        (4, _) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        _ => 0.0,
    }
}

/// Build a tiny 16-bit mono WAV (for tests).
#[cfg(test)]
pub fn write_test_wav(path: &Path, samples: &[i16]) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 2) as u32;
    let mut f = File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&44100u32.to_le_bytes())?;
    f.write_all(&(44100u32 * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_peaks_reflect_loud_and_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        // First half silent, second half full scale.
        let mut samples = vec![0i16; 4410];
        samples.extend(std::iter::repeat_n(30000i16, 4410));
        write_test_wav(&path, &samples).unwrap();

        let peaks = compute_wav_peaks(&path, 8).unwrap();
        assert_eq!(peaks.len(), 8);
        // Quiet early buckets.
        assert!(peaks[0] < 0.05);
        assert!(peaks[1] < 0.05);
        // Loud late buckets.
        assert!(peaks[6] > 0.8);
        assert!(peaks[7] > 0.8);
        for p in &peaks {
            assert!(*p <= 1.0);
        }
    }

    #[test]
    fn wav_peaks_reject_non_riff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.wav");
        std::fs::write(&path, b"not a wav at all").unwrap();
        assert!(compute_wav_peaks(&path, 16).is_err());
    }

    #[test]
    fn huge_claimed_data_size_does_not_oom() {
        // Lying data-size must not allocate the claimed buffer; peaks stream.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lie.wav");
        let samples = vec![1000i16; 256];
        write_test_wav(&path, &samples).unwrap();
        // Patch data chunk size to 0xFFFF_F000 (still short file → stream stops).
        let mut bytes = std::fs::read(&path).unwrap();
        let n = bytes.len();
        bytes[n - 256 * 2 - 4..n - 256 * 2].copy_from_slice(&0xFFFF_F000u32.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        // Must not abort the process; either peaks or a clean error.
        let _ = compute_wav_peaks(&path, 32);
    }
}
