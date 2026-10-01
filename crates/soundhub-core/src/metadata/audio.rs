use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::error::{Error, Result};
use crate::models::AudioMeta;

/// Minimal AIFF FORM/COMM parser (PCM only). Enough for technical params.
pub fn parse_aiff_header(path: &Path) -> Result<AudioMeta> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);
    let mut header = [0u8; 12];
    r.read_exact(&mut header)?;
    if &header[0..4] != b"FORM" {
        return Err(Error::other("not an AIFF file"));
    }
    // header[8..12] is AIFC or AIFF
    loop {
        let mut chunk_hdr = [0u8; 8];
        if r.read_exact(&mut chunk_hdr).is_err() {
            break;
        }
        let id = &chunk_hdr[0..4];
        let size = u32::from_be_bytes([chunk_hdr[4], chunk_hdr[5], chunk_hdr[6], chunk_hdr[7]]) as usize;
        if id == b"COMM" {
            let mut data = vec![0u8; size.min(64)];
            r.read_exact(&mut data)?;
            if data.len() < 18 {
                return Err(Error::other("COMM chunk too small"));
            }
            let channels = u16::from_be_bytes([data[0], data[1]]);
            let num_frames = u32::from_be_bytes([data[2], data[3], data[4], data[5]]);
            let sample_size = u16::from_be_bytes([data[6], data[7]]);
            let sample_rate = f64_from_extended(&data[8..18]);
            return Ok(AudioMeta {
                duration_ms: if sample_rate > 0.0 {
                    Some((num_frames as f64 / sample_rate * 1000.0) as u64)
                } else {
                    None
                },
                sample_rate: Some(sample_rate as u32),
                bit_depth: Some(sample_size),
                channels: Some(channels),
                channel_layout: layout_for_channels(channels),
                codec: Some("PCM".into()),
                container: Some("AIFF".into()),
                bitrate: None,
                compression: None,
            });
        }
        // Skip chunk (chunks are word-aligned).
        let skip = size + (size % 2);
        std::io::copy(&mut r.by_ref().take(skip as u64), &mut std::io::sink())?;
    }
    Err(Error::other("COMM chunk not found"))
}

/// FLAC STREAMINFO block (must be first metadata block).
pub fn parse_flac_streaminfo(path: &Path) -> Result<AudioMeta> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"fLaC" {
        return Err(Error::other("not a FLAC file"));
    }
    let mut block_hdr = [0u8; 4];
    r.read_exact(&mut block_hdr)?;
    let length = ((block_hdr[1] as usize) << 16)
        | ((block_hdr[2] as usize) << 8)
        | (block_hdr[3] as usize);
    if length < 34 {
        return Err(Error::other("STREAMINFO too small"));
    }
    let mut data = vec![0u8; length];
    r.read_exact(&mut data)?;

    let min_block = u16::from_be_bytes([data[0], data[1]]);
    let max_block = u16::from_be_bytes([data[2], data[3]]);
    let _ = (min_block, max_block);
    let sample_rate = ((data[10] as u32) << 12) | ((data[11] as u32) << 4) | ((data[12] as u32) >> 4);
    let channels = (((data[12] >> 1) & 0x07) + 1) as u16;
    let bit_depth = ((((data[12] & 0x01) as u16) << 4) | (((data[13] >> 4) & 0x0f) as u16)) + 1;
    let total_samples: u64 = (((data[13] & 0x0f) as u64) << 32)
        | ((data[14] as u64) << 24)
        | ((data[15] as u64) << 16)
        | ((data[16] as u64) << 8)
        | (data[17] as u64);

    Ok(AudioMeta {
        duration_ms: if sample_rate > 0 && total_samples > 0 {
            Some(total_samples * 1000 / sample_rate as u64)
        } else {
            None
        },
        sample_rate: Some(sample_rate),
        bit_depth: Some(bit_depth),
        channels: Some(channels),
        channel_layout: layout_for_channels(channels),
        codec: Some("FLAC".into()),
        container: Some("FLAC".into()),
        bitrate: None,
        compression: Some("lossless".into()),
    })
}

pub fn layout_for_channels(ch: u16) -> Option<String> {
    match ch {
        1 => Some("mono".into()),
        2 => Some("stereo".into()),
        6 => Some("5.1".into()),
        8 => Some("7.1".into()),
        _ => None,
    }
}

/// 80-bit IEEE 754 extended float used by AIFF sample rate.
fn f64_from_extended(b: &[u8]) -> f64 {
    if b.len() < 10 {
        return 0.0;
    }
    let exp = ((b[0] as i32) << 8 | b[1] as i32) & 0x7fff;
    let hi = ((b[2] as u64) << 24) | ((b[3] as u64) << 16) | ((b[4] as u64) << 8) | b[5] as u64;
    let lo = ((b[6] as u64) << 24) | ((b[7] as u64) << 16) | ((b[8] as u64) << 8) | b[9] as u64;
    if exp == 0 && hi == 0 && lo == 0 {
        return 0.0;
    }
    let mantissa = ((hi as f64) * (1u64 << 32) as f64) + lo as f64;
    mantissa * 2f64.powi(exp - 16383 - 63)
}
