//! Header-only parsers for compressed containers the MVP imports but does
//! not fully decode: MP3 (MPEG audio + ID3), M4A/AAC (ISO BMFF), CAF.
//!
//! Technical params (duration / sample rate / channels / bitrate) and common
//! ID3 text frames are enough for browse/search. Unknown frames stay in
//! `raw` — never dropped. A parse failure must not block import.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{Error, Result};
use crate::metadata::audio::layout_for_channels;
use crate::models::AudioMeta;

// ── MP3 ─────────────────────────────────────────────────────────────────────

const MP3_BITRATE_KBPS: [[u32; 15]; 6] = [
    // Rows: MPEG1 L3, MPEG1 L2, MPEG1 L1, MPEG2/2.5 L3, MPEG2/2.5 L2, MPEG2/2.5 L1
    [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ],
    [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ],
];

const MP3_SAMPLE_RATES: [[u32; 3]; 3] = [
    [44100, 48000, 32000], // MPEG1
    [22050, 24000, 16000], // MPEG2
    [11025, 12000, 8000],  // MPEG2.5
];

pub struct Mp3Parse {
    pub audio: AudioMeta,
    pub raw: BTreeMap<String, serde_json::Value>,
}

/// Parse MP3: optional ID3v2 + first MPEG frame header + optional ID3v1.
pub fn parse_mp3(path: &Path) -> Result<Mp3Parse> {
    let file = File::open(path)?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut r = BufReader::new(file);

    let mut raw: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut audio = AudioMeta {
        container: Some("MP3".into()),
        codec: Some("MPEG audio".into()),
        ..Default::default()
    };

    let mut start = 0u64;

    // ID3v2 header: "ID3" + ver(2) + flags(1) + synchsafe size(4)
    let mut head = [0u8; 10];
    if r.read_exact(&mut head).is_ok() && &head[0..3] == b"ID3" {
        let flags = head[5];
        let size = synchsafe_u32(&head[6..10]) as u64;
        let mut id3v2_size = 10 + size;
        if flags & 0x40 != 0 {
            // extended header — skip its size too
            let mut ext = [0u8; 4];
            if r.read_exact(&mut ext).is_ok() {
                let esz = synchsafe_u32(&ext) as u64;
                id3v2_size += esz.min(size);
            }
        }
        start = id3v2_size;
        // Read a bounded slice of the tag for common text frames.
        parse_id3v2(&mut r, head[3], flags, size, &mut raw, &mut audio);
    }

    // Find the first MPEG frame sync at/after `start`.
    r.seek(SeekFrom::Start(start))?;
    let mut window = [0u8; 8];
    if r.read_exact(&mut window).is_err() {
        return Err(Error::other("mp3 too short"));
    }
    let mut pos = start;
    let mut frame: Option<Mp3Frame> = None;
    loop {
        if let Some(f) = try_frame(&window) {
            frame = Some(f);
            break;
        }
        window.copy_within(1.., 0);
        let mut b = [0u8; 1];
        if r.read_exact(&mut b).is_err() {
            break;
        }
        window[7] = b[0];
        pos += 1;
        if pos > start + 256 * 1024 {
            break;
        }
    }

    let f = frame.ok_or_else(|| Error::other("no MPEG audio frame found"))?;
    audio.sample_rate = Some(f.sample_rate);
    audio.channels = Some(f.channels);
    audio.channel_layout = layout_for_channels(f.channels);
    audio.codec = Some(match f.layer {
        1 => "MPEG Layer I".into(),
        2 => "MPEG Layer II".into(),
        3 => "MPEG Layer III".into(),
        _ => "MPEG audio".into(),
    });
    if f.bitrate_kbps > 0 {
        audio.bitrate = Some(f.bitrate_kbps * 1000);
    }
    // Duration estimate: (audio bytes * 8) / bitrate. Good enough for browse.
    if f.bitrate_kbps > 0 && file_len > start {
        let audio_bytes = file_len - start;
        let ms = (audio_bytes.saturating_mul(8)) / (f.bitrate_kbps as u64);
        audio.duration_ms = Some(ms);
    }
    raw.insert(
        "mp3_frame".into(),
        serde_json::json!({
            "mpeg_version": f.mpeg_version,
            "layer": f.layer,
            "bitrate_kbps": f.bitrate_kbps,
            "sample_rate": f.sample_rate,
            "channels": f.channels,
            "frame_offset": pos,
        }),
    );

    // ID3v1 lives in the last 128 bytes.
    if file_len >= 128 {
        r.seek(SeekFrom::Start(file_len - 128))?;
        let mut tag = [0u8; 128];
        if r.read_exact(&mut tag).is_ok() && &tag[0..3] == b"TAG" {
            let title = id3v1_str(&tag[3..33]);
            let artist = id3v1_str(&tag[33..63]);
            let album = id3v1_str(&tag[63..93]);
            if !title.is_empty() {
                raw.insert("id3v1_title".into(), serde_json::json!(title));
            }
            if !artist.is_empty() {
                raw.insert("id3v1_artist".into(), serde_json::json!(artist));
            }
            if !album.is_empty() {
                raw.insert("id3v1_album".into(), serde_json::json!(album));
            }
        }
    }

    Ok(Mp3Parse { audio, raw })
}

struct Mp3Frame {
    mpeg_version: u8,
    layer: u8,
    bitrate_kbps: u32,
    sample_rate: u32,
    channels: u16,
}

fn try_frame(w: &[u8; 8]) -> Option<Mp3Frame> {
    if w[0] != 0xFF || (w[1] & 0xE0) != 0xE0 {
        return None;
    }
    let version_id = (w[1] >> 3) & 0x03; // 0=2.5, 2=2, 3=1
    let layer_id = (w[1] >> 1) & 0x03; // 1=III, 2=II, 3=I
    if version_id == 1 || layer_id == 0 {
        return None;
    }
    let bitrate_idx = ((w[2] >> 4) & 0x0F) as usize;
    let sr_idx = ((w[2] >> 2) & 0x03) as usize;
    if bitrate_idx == 0x0F || sr_idx == 0x03 {
        return None;
    }
    let channel_mode = (w[3] >> 6) & 0x03;
    let channels = if channel_mode == 3 { 1u16 } else { 2 };

    let mpeg_row = match version_id {
        // MPEG1: Layer I / II / III
        3 => match layer_id {
            3 => 2,
            2 => 1,
            1 => 0,
            _ => return None,
        },
        // MPEG2 / MPEG2.5 share the low-rate tables
        2 | 0 => match layer_id {
            3 => 5,
            2 => 4,
            1 => 3,
            _ => return None,
        },
        _ => return None,
    };
    let bitrate_kbps = MP3_BITRATE_KBPS.get(mpeg_row)?.get(bitrate_idx).copied()?;
    let sr_row = match version_id {
        3 => 0,
        2 => 1,
        0 => 2,
        _ => return None,
    };
    let sample_rate = MP3_SAMPLE_RATES.get(sr_row)?.get(sr_idx).copied()?;
    let layer = match layer_id {
        3 => 1,
        2 => 2,
        1 => 3,
        _ => return None,
    };
    Some(Mp3Frame {
        mpeg_version: match version_id {
            3 => 1,
            2 => 2,
            _ => 25,
        },
        layer,
        bitrate_kbps,
        sample_rate,
        channels,
    })
}

fn synchsafe_u32(b: &[u8]) -> u32 {
    if b.len() < 4 {
        return 0;
    }
    ((b[0] as u32 & 0x7F) << 21)
        | ((b[1] as u32 & 0x7F) << 14)
        | ((b[2] as u32 & 0x7F) << 7)
        | (b[3] as u32 & 0x7F)
}

fn id3v1_str(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

fn parse_id3v2<R: Read>(
    r: &mut R,
    version: u8,
    flags: u8,
    size: u64,
    raw: &mut BTreeMap<String, serde_json::Value>,
    audio: &mut AudioMeta,
) {
    // Re-read the tag body (we already consumed the 10-byte header).
    let mut body = vec![0u8; size.min(2 * 1024 * 1024) as usize];
    if r.read_exact(&mut body).is_err() {
        return;
    }
    let mut i = 0usize;
    // ID3v2.2 uses 3-byte ids and 3-byte sizes.
    let v22 = version == 2;
    let id_len = if v22 { 3 } else { 4 };
    let size_len = if v22 { 3 } else { 4 };
    let hdr_len = id_len + size_len + if v22 { 0 } else { 2 };

    while i + hdr_len <= body.len() {
        let id_bytes = &body[i..i + id_len];
        if id_bytes[0] == 0 {
            break;
        }
        let id = String::from_utf8_lossy(id_bytes).to_string();
        let sz = if v22 {
            ((body[i + 3] as usize) << 16) | ((body[i + 4] as usize) << 8) | (body[i + 5] as usize)
        } else {
            ((body[i + 4] as usize) << 24)
                | ((body[i + 5] as usize) << 16)
                | ((body[i + 6] as usize) << 8)
                | (body[i + 7] as usize)
        };
        let data_start = i + hdr_len;
        let data_end = data_start.saturating_add(sz);
        if data_end > body.len() {
            break;
        }
        let data = &body[data_start..data_end];

        // Keep a short lossy preview of every frame; map common text frames.
        if id.starts_with('T') && !data.is_empty() {
            let encoding = data[0];
            let text = decode_id3_text(encoding, &data[1..]);
            match id.as_str() {
                "TIT2" | "TT2" => {
                    raw.insert("id3_title".into(), serde_json::json!(text));
                }
                "TPE1" | "TP1" => {
                    raw.insert("id3_artist".into(), serde_json::json!(text));
                }
                "TALB" | "TAL" => {
                    raw.insert("id3_album".into(), serde_json::json!(text));
                }
                "TCON" | "TCO" => {
                    raw.insert("id3_genre".into(), serde_json::json!(text));
                }
                "TYER" | "TYE" | "TDRC" => {
                    raw.insert("id3_year".into(), serde_json::json!(text));
                }
                _ => {
                    raw.insert(format!("id3_{id}"), serde_json::json!(text));
                }
            }
        } else if id == "COMM" || id == "COM" {
            // encoding + 3-byte language + short description + text
            if data.len() > 4 {
                let text = decode_id3_text(data[0], &data[4..]);
                raw.insert("id3_comment".into(), serde_json::json!(text));
            }
        } else {
            let preview: String = data.iter().take(64).map(|b| *b as char).collect();
            raw.insert(
                format!("id3_{id}"),
                serde_json::json!({ "size": sz, "preview_lossy": preview }),
            );
        }

        i = data_end;
    }

    // TLEN is duration in milliseconds as text.
    if let Some(serde_json::Value::String(tlen)) = raw.get("id3_TLEN") {
        if let Ok(ms) = tlen.trim().parse::<u64>() {
            if ms > 0 {
                audio.duration_ms = Some(ms);
            }
        }
    }
    let _ = flags;
}

fn decode_id3_text(encoding: u8, b: &[u8]) -> String {
    match encoding {
        0 | 3 => String::from_utf8_lossy(b)
            .trim_end_matches('\0')
            .trim()
            .to_string(),
        1 | 2 => {
            // UTF-16 with BOM (1) or UTF-16BE (2)
            let (be, slice) = if encoding == 1 && b.len() >= 2 {
                match (b[0], b[1]) {
                    (0xFF, 0xFE) => (false, &b[2..]),
                    (0xFE, 0xFF) => (true, &b[2..]),
                    _ => (false, b),
                }
            } else {
                (true, b)
            };
            let units: Vec<u16> = slice
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| {
                    if be {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                })
                .take_while(|u| *u != 0)
                .collect();
            String::from_utf16_lossy(&units).trim().to_string()
        }
        _ => String::from_utf8_lossy(b).trim().to_string(),
    }
}

// ── M4A / MP4 ───────────────────────────────────────────────────────────────

pub struct Mp4Parse {
    pub audio: AudioMeta,
    pub raw: BTreeMap<String, serde_json::Value>,
}

/// Parse ISO BMFF (M4A/MP4) headers: `mvhd`/`mdhd` for duration, `mp4a` for
/// sample rate / channels. Does not touch coded sample data.
pub fn parse_mp4_audio(path: &Path) -> Result<Mp4Parse> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);
    let mut raw: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut audio = AudioMeta {
        container: Some("M4A".into()),
        codec: Some("AAC".into()),
        ..Default::default()
    };

    let file_len = file_length(&mut r)?;
    walk_mp4_boxes(&mut r, 0, file_len, &mut audio, &mut raw, true)?;

    if audio.sample_rate.is_none() && audio.duration_ms.is_some() {
        // keep duration even without mp4a
    }
    Ok(Mp4Parse { audio, raw })
}

fn file_length<R: Seek>(r: &mut R) -> Result<u64> {
    let end = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    Ok(end)
}

fn walk_mp4_boxes<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    audio: &mut AudioMeta,
    raw: &mut BTreeMap<String, serde_json::Value>,
    top_level: bool,
) -> Result<()> {
    let mut pos = start;
    while pos + 8 <= end {
        r.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 8];
        if r.read_exact(&mut hdr).is_err() {
            break;
        }
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let kind = String::from_utf8_lossy(&hdr[4..8]).to_string();
        let mut body_start = pos + 8;
        if size == 1 {
            let mut ext = [0u8; 8];
            if r.read_exact(&mut ext).is_err() {
                break;
            }
            size = u64::from_be_bytes(ext);
            body_start = pos + 16;
        } else if size == 0 {
            size = end.saturating_sub(pos);
        }
        if size < 8 {
            break;
        }
        let body_end = pos + size;

        match kind.as_str() {
            "moov" | "trak" | "mdia" | "minf" | "stbl" | "udta" | "ilst" => {
                walk_mp4_boxes(r, body_start, body_end.min(end), audio, raw, false)?;
            }
            "mvhd" => {
                read_mvhd(r, body_start, body_end, audio)?;
            }
            "mdhd" => {
                read_mdhd(r, body_start, body_end, audio)?;
            }
            "mp4a" => {
                read_mp4a(r, body_start, body_end, audio, raw)?;
            }
            "ftyp" => {
                let mut brand = [0u8; 4];
                if r.seek(SeekFrom::Start(body_start)).is_ok() && r.read_exact(&mut brand).is_ok() {
                    raw.insert(
                        "mp4_brand".into(),
                        serde_json::json!(String::from_utf8_lossy(&brand)),
                    );
                }
            }
            other => {
                if top_level {
                    raw.insert(
                        format!("mp4_box_{other}"),
                        serde_json::json!({ "size": size }),
                    );
                }
            }
        }

        pos = body_end;
    }
    Ok(())
}

fn read_mvhd<R: Read + Seek>(r: &mut R, start: u64, end: u64, audio: &mut AudioMeta) -> Result<()> {
    r.seek(SeekFrom::Start(start))?;
    let mut buf = [0u8; 32];
    let n = r.read(&mut buf)?;
    if n < 20 {
        return Ok(());
    }
    let version = buf[0];
    if version == 1 {
        if n < 32 {
            return Ok(());
        }
        let timescale = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
        let duration = u64::from_be_bytes([
            buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31],
        ]);
        if timescale > 0 {
            audio.duration_ms = Some(duration.saturating_mul(1000) / timescale as u64);
        }
    } else {
        if n < 20 {
            return Ok(());
        }
        let timescale = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let duration = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
        if timescale > 0 {
            audio.duration_ms = Some(duration as u64 * 1000 / timescale as u64);
        }
    }
    let _ = end;
    Ok(())
}

fn read_mdhd<R: Read + Seek>(
    r: &mut R,
    start: u64,
    _end: u64,
    audio: &mut AudioMeta,
) -> Result<()> {
    // Prefer mvhd; only fill duration if still empty.
    if audio.duration_ms.is_some() {
        return Ok(());
    }
    r.seek(SeekFrom::Start(start))?;
    let mut buf = [0u8; 32];
    let n = r.read(&mut buf)?;
    if n < 20 {
        return Ok(());
    }
    let version = buf[0];
    if version == 1 {
        if n < 32 {
            return Ok(());
        }
        let timescale = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
        let duration = u64::from_be_bytes([
            buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31],
        ]);
        if timescale > 0 {
            audio.duration_ms = Some(duration.saturating_mul(1000) / timescale as u64);
        }
    } else if n >= 20 {
        let timescale = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let duration = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
        if timescale > 0 {
            audio.duration_ms = Some(duration as u64 * 1000 / timescale as u64);
        }
    }
    Ok(())
}

fn read_mp4a<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    audio: &mut AudioMeta,
    raw: &mut BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    // SampleEntry: 6 reserved + 2 data_ref_index, then AudioSampleEntry.
    r.seek(SeekFrom::Start(start + 8))?;
    let mut buf = [0u8; 28];
    let n = r.read(&mut buf)?;
    if n < 20 {
        return Ok(());
    }
    let channels = u16::from_be_bytes([buf[16], buf[17]]);
    let sample_size = u16::from_be_bytes([buf[18], buf[19]]);
    // sample rate is 16.16 fixed at bytes 24..28 of AudioSampleEntry (offset 16+8)
    if n >= 28 {
        let sr_fixed = u32::from_be_bytes([buf[24], buf[25], buf[26], buf[27]]);
        let sample_rate = sr_fixed >> 16;
        if sample_rate > 0 {
            audio.sample_rate = Some(sample_rate);
        }
    }
    if channels > 0 {
        audio.channels = Some(channels);
        audio.channel_layout = layout_for_channels(channels);
    }
    if sample_size > 0 {
        audio.bit_depth = Some(sample_size);
    }
    raw.insert(
        "mp4a".into(),
        serde_json::json!({ "channels": channels, "sample_size": sample_size, "end": end }),
    );
    Ok(())
}

// ── CAF (Core Audio Format) ─────────────────────────────────────────────────

/// Parse CAF `desc` chunk (mSampleRate / mFormatID / channels).
pub fn parse_caf(path: &Path) -> Result<AudioMeta> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);

    let mut hdr = [0u8; 8];
    r.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"caff" {
        return Err(Error::other("not a CAF file"));
    }

    let mut audio = AudioMeta {
        container: Some("CAF".into()),
        ..Default::default()
    };

    loop {
        let mut chunk = [0u8; 12];
        if r.read_exact(&mut chunk).is_err() {
            break;
        }
        let id = String::from_utf8_lossy(&chunk[0..4]).to_string();
        // mChunkSize is int64 BE; -1 means "to end of file".
        let size = i64::from_be_bytes([
            chunk[4], chunk[5], chunk[6], chunk[7], chunk[8], chunk[9], chunk[10], chunk[11],
        ]);
        if id == "desc" {
            let mut data = [0u8; 32];
            let n = r.read(&mut data)?;
            if n >= 32 {
                let sample_rate = f64::from_be_bytes([
                    data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                ]);
                let format_id = String::from_utf8_lossy(&data[8..12]).to_string();
                let flags = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
                let bytes_per_packet = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
                let frames_per_packet =
                    u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
                let channels = u32::from_be_bytes([data[24], data[25], data[26], data[27]]);
                let bits = u32::from_be_bytes([data[28], data[29], data[30], data[31]]);
                if sample_rate > 0.0 {
                    audio.sample_rate = Some(sample_rate as u32);
                }
                if channels > 0 && channels <= u16::MAX as u32 {
                    audio.channels = Some(channels as u16);
                    audio.channel_layout = layout_for_channels(channels as u16);
                }
                if bits > 0 && bits <= u16::MAX as u32 {
                    audio.bit_depth = Some(bits as u16);
                }
                audio.codec = Some(match format_id.as_str() {
                    "lpcm" => "PCM".into(),
                    "aac " => "AAC".into(),
                    "alac" => "ALAC".into(),
                    other => other.trim().to_string(),
                });
                // frames_per_packet==0 for VBR; flags bit 0 = float, bit 1 = little endian.
                let _ = (flags, bytes_per_packet, frames_per_packet);
            }
            break;
        }
        if size < 0 {
            break;
        }
        let skip = size as u64;
        if std::io::copy(&mut r.by_ref().take(skip), &mut std::io::sink())? < skip {
            break;
        }
    }

    if audio.codec.is_none() {
        audio.codec = Some("unknown".into());
    }
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn mp3_frame_header_is_parsed() {
        // Hand-built MPEG1 Layer3 128kbps 44.1kHz stereo frame sync.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.mp3");
        let mut f = File::create(&path).unwrap();
        // ID3v2 empty tag
        f.write_all(b"ID3\x03\x00\x00\x00\x00\x00\x00").unwrap();
        // Frame header: FF FB 90 00 → MPEG1 L3, bitrate idx 9 (128k), 44100, stereo
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00])
            .unwrap();
        // pad some "audio" so duration estimate is non-zero
        for i in 0..4096u32 {
            f.write_all(&[(i % 251) as u8]).unwrap();
        }
        drop(f);

        let parsed = parse_mp3(&path).unwrap();
        assert_eq!(parsed.audio.sample_rate, Some(44100));
        assert_eq!(parsed.audio.channels, Some(2));
        assert_eq!(parsed.audio.bitrate, Some(128_000));
        assert!(parsed.audio.duration_ms.unwrap() > 0);
    }

    #[test]
    fn mp3_id3v2_text_frames_land_in_raw() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tagged.mp3");
        let mut body = Vec::new();
        // TIT2: encoding 0 + "Hello"
        let payload = b"\x00Hello";
        body.extend_from_slice(b"TIT2");
        body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        body.extend_from_slice(&[0, 0]); // flags
        body.extend_from_slice(payload);
        // TPE1: encoding 0 + "Artist"
        let payload2 = b"\x00Artist";
        body.extend_from_slice(b"TPE1");
        body.extend_from_slice(&(payload2.len() as u32).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(payload2);

        let mut f = File::create(&path).unwrap();
        f.write_all(b"ID3\x03\x00\x00").unwrap();
        f.write_all(&synchsafe_bytes(body.len() as u32)).unwrap();
        f.write_all(&body).unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00])
            .unwrap();
        for _ in 0..1024 {
            f.write_all(&[0u8]).unwrap();
        }
        drop(f);

        let parsed = parse_mp3(&path).unwrap();
        assert_eq!(
            parsed.raw.get("id3_title"),
            Some(&serde_json::json!("Hello"))
        );
        assert_eq!(
            parsed.raw.get("id3_artist"),
            Some(&serde_json::json!("Artist"))
        );
    }

    fn synchsafe_bytes(v: u32) -> [u8; 4] {
        [
            ((v >> 21) & 0x7F) as u8,
            ((v >> 14) & 0x7F) as u8,
            ((v >> 7) & 0x7F) as u8,
            (v & 0x7F) as u8,
        ]
    }

    #[test]
    fn caf_desc_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.caf");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"caff\x00\x01\x00\x00").unwrap();
        f.write_all(b"desc").unwrap();
        f.write_all(&32i64.to_be_bytes()).unwrap();
        // mSampleRate 48000.0
        f.write_all(&48000.0f64.to_be_bytes()).unwrap();
        f.write_all(b"lpcm").unwrap();
        f.write_all(&0u32.to_be_bytes()).unwrap(); // flags
        f.write_all(&2u32.to_be_bytes()).unwrap(); // bytes per packet
        f.write_all(&1u32.to_be_bytes()).unwrap(); // frames per packet
        f.write_all(&1u32.to_be_bytes()).unwrap(); // channels
        f.write_all(&16u32.to_be_bytes()).unwrap(); // bits
        drop(f);

        let audio = parse_caf(&path).unwrap();
        assert_eq!(audio.sample_rate, Some(48000));
        assert_eq!(audio.channels, Some(1));
        assert_eq!(audio.bit_depth, Some(16));
        assert_eq!(audio.codec.as_deref(), Some("PCM"));
    }

    #[test]
    fn mp4_mvhd_gives_duration() {
        // Minimal ftyp + moov/mvhd (version 0, timescale 1000, duration 2500 → 2.5s)
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.m4a");

        let mvhd_box = {
            let mut content = vec![0u8; 24];
            content[12..16].copy_from_slice(&1000u32.to_be_bytes());
            content[16..20].copy_from_slice(&2500u32.to_be_bytes());
            let total = (8 + content.len()) as u32;
            let mut b = Vec::new();
            b.extend_from_slice(&total.to_be_bytes());
            b.extend_from_slice(b"mvhd");
            b.extend_from_slice(&content);
            b
        };
        let moov_box = {
            let total = (8 + mvhd_box.len()) as u32;
            let mut b = Vec::new();
            b.extend_from_slice(&total.to_be_bytes());
            b.extend_from_slice(b"moov");
            b.extend_from_slice(&mvhd_box);
            b
        };

        let mut f = File::create(&path).unwrap();
        f.write_all(&20u32.to_be_bytes()).unwrap();
        f.write_all(b"ftyp").unwrap();
        f.write_all(b"M4A ").unwrap();
        f.write_all(&0u32.to_be_bytes()).unwrap();
        f.write_all(b"M4A ").unwrap();
        f.write_all(&moov_box).unwrap();
        drop(f);

        let parsed = parse_mp4_audio(&path).unwrap();
        assert_eq!(parsed.audio.duration_ms, Some(2500));
    }
}
