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

// ── OGG (Vorbis / Opus) ─────────────────────────────────────────────────────

pub struct OggParse {
    pub audio: AudioMeta,
    pub raw: BTreeMap<String, serde_json::Value>,
}

/// Parse OGG container: either Vorbis or Opus identification + comment headers.
pub fn parse_ogg(path: &Path) -> Result<OggParse> {
    let file = File::open(path)?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut r = BufReader::new(file);

    let mut raw: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut audio = AudioMeta {
        container: Some("OGG".into()),
        ..Default::default()
    };

    // First page payload = first packet (identification header).
    let first_packet = read_ogg_first_packet(&mut r)?;
    if first_packet.len() >= 7 && &first_packet[0..7] == b"\x01vorbis" {
        parse_vorbis_id(&first_packet, &mut audio, &mut raw)?;
    } else if first_packet.len() >= 8 && &first_packet[0..8] == b"OpusHead" {
        parse_opus_head(&first_packet, &mut audio, &mut raw)?;
    } else {
        return Err(Error::other("ogg: no vorbis/opus identification header"));
    }

    // Second page usually carries the comment header.
    if let Ok(comment) = read_ogg_second_packet(&mut r) {
        if comment.len() >= 7 && &comment[0..7] == b"\x03vorbis" {
            parse_vorbis_comment(&comment[7..], &mut raw);
        } else if comment.len() >= 8 && &comment[0..8] == b"OpusTags" {
            parse_opus_tags(&comment[8..], &mut raw);
        }
    }

    // Duration: prefer last-page granule position.
    // Opus granule is always 48 kHz units (RFC 7845); Vorbis granule is in
    // stream sample-rate units.
    if let Some(total_samples) = last_ogg_granule(path, file_len) {
        let is_opus = audio.codec.as_deref() == Some("Opus");
        let rate = if is_opus {
            48_000u64
        } else {
            audio.sample_rate.unwrap_or(48000) as u64
        };
        if rate > 0 {
            audio.duration_ms = Some(total_samples.saturating_mul(1000) / rate);
        }
    } else if let Some(br) = audio.bitrate {
        if br > 0 && file_len > 0 {
            audio.duration_ms = Some(file_len.saturating_mul(8) / br as u64);
        }
    }

    Ok(OggParse { audio, raw })
}

/// Read the first Ogg page and return its packet payload.
fn read_ogg_first_packet<R: Read>(r: &mut R) -> Result<Vec<u8>> {
    let (payload, _) = read_ogg_page(r)?;
    Ok(payload)
}

/// Read the second Ogg page payload (comment header packet).
fn read_ogg_second_packet<R: Read>(r: &mut R) -> Result<Vec<u8>> {
    let (payload, _) = read_ogg_page(r)?;
    Ok(payload)
}

/// Read one Ogg page. Returns (payload, granule_position).
fn read_ogg_page<R: Read>(r: &mut R) -> Result<(Vec<u8>, u64)> {
    let mut hdr = [0u8; 27];
    r.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"OggS" {
        return Err(Error::other("ogg: missing page capture pattern"));
    }
    let granule = u64::from_le_bytes([
        hdr[6], hdr[7], hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13],
    ]);
    let nseg = hdr[26] as usize;
    let mut seg_table = vec![0u8; nseg];
    r.read_exact(&mut seg_table)?;
    let body_len: usize = seg_table.iter().map(|&s| s as usize).sum();
    let mut body = vec![0u8; body_len];
    r.read_exact(&mut body)?;
    Ok((body, granule))
}

fn parse_vorbis_id(
    packet: &[u8],
    audio: &mut AudioMeta,
    raw: &mut BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    // 0x01 "vorbis" version(4) channels(1) rate(4) max(4) nom(4) min(4) block(1) framing(1)
    if packet.len() < 30 {
        return Err(Error::other("vorbis id packet too short"));
    }
    let channels = packet[11];
    let sample_rate = u32::from_le_bytes([packet[12], packet[13], packet[14], packet[15]]);
    let nominal = i32::from_le_bytes([packet[20], packet[21], packet[22], packet[23]]);

    audio.codec = Some("Vorbis".into());
    if sample_rate > 0 {
        audio.sample_rate = Some(sample_rate);
    }
    if channels > 0 {
        audio.channels = Some(channels as u16);
        audio.channel_layout = layout_for_channels(channels as u16);
    }
    if nominal > 0 {
        audio.bitrate = Some(nominal as u32);
    }
    raw.insert(
        "vorbis_id".into(),
        serde_json::json!({
            "channels": channels,
            "sample_rate": sample_rate,
            "bitrate_nominal": nominal,
        }),
    );
    Ok(())
}

fn parse_opus_head(
    packet: &[u8],
    audio: &mut AudioMeta,
    raw: &mut BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    // "OpusHead" version(1) channels(1) pre_skip(2) input_rate(4) gain(2) mapping(1)
    if packet.len() < 19 {
        return Err(Error::other("OpusHead too short"));
    }
    let channels = packet[9];
    let pre_skip = u16::from_le_bytes([packet[10], packet[11]]);
    let input_rate = u32::from_le_bytes([packet[12], packet[13], packet[14], packet[15]]);
    let gain = i16::from_le_bytes([packet[16], packet[17]]);
    let mapping = packet[18];

    audio.codec = Some("Opus".into());
    // Opus always decodes at 48 kHz; input_sample_rate is the original capture rate.
    audio.sample_rate = Some(if input_rate > 0 { input_rate } else { 48000 });
    if channels > 0 {
        audio.channels = Some(channels as u16);
        audio.channel_layout = layout_for_channels(channels as u16);
    }
    raw.insert(
        "opus_head".into(),
        serde_json::json!({
            "channels": channels,
            "pre_skip": pre_skip,
            "input_sample_rate": input_rate,
            "output_gain_db": gain as f32 / 256.0,
            "mapping_family": mapping,
        }),
    );
    Ok(())
}

/// Parse Vorbis comment block (after the 0x03 "vorbis" prefix).
fn parse_vorbis_comment(data: &[u8], raw: &mut BTreeMap<String, serde_json::Value>) {
    parse_comment_list(data, "vorbis_", raw);
}

/// Parse OpusTags body (after "OpusTags").
fn parse_opus_tags(data: &[u8], raw: &mut BTreeMap<String, serde_json::Value>) {
    parse_comment_list(data, "opus_", raw);
}

/// Shared "vendor + KEY=value list" parser (Vorbis comment / OpusTags).
fn parse_comment_list(data: &[u8], prefix: &str, raw: &mut BTreeMap<String, serde_json::Value>) {
    let mut i = 0usize;
    if data.len() < 4 {
        return;
    }
    let vendor_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    i += 4;
    if i + vendor_len > data.len() {
        return;
    }
    let vendor = String::from_utf8_lossy(&data[i..i + vendor_len]).to_string();
    i += vendor_len;
    raw.insert(format!("{prefix}vendor"), serde_json::json!(vendor));

    if i + 4 > data.len() {
        return;
    }
    let count = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
    i += 4;
    for _ in 0..count {
        if i + 4 > data.len() {
            break;
        }
        let len = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if i + len > data.len() {
            break;
        }
        let entry = String::from_utf8_lossy(&data[i..i + len]).to_string();
        i += len;
        if let Some((k, v)) = entry.split_once('=') {
            let key = format!("{prefix}{}", k.trim().to_ascii_lowercase());
            raw.insert(key, serde_json::json!(v));
        }
    }
}

/// Locate the last OggS page and return its granule position (total samples).
fn last_ogg_granule(path: &Path, file_len: u64) -> Option<u64> {
    if file_len < 27 {
        return None;
    }
    let mut file = File::open(path).ok()?;
    // Search a tail window for the last "OggS".
    let window = file_len.min(64 * 1024);
    file.seek(SeekFrom::Start(file_len - window)).ok()?;
    let mut buf = vec![0u8; window as usize];
    file.read_exact(&mut buf).ok()?;
    let mut idx = None;
    let mut i = 0;
    while i + 4 <= buf.len() {
        if &buf[i..i + 4] == b"OggS" {
            idx = Some(i);
        }
        i += 1;
    }
    let off = idx?;
    if off + 14 > buf.len() {
        return None;
    }
    let granule = u64::from_le_bytes([
        buf[off + 6],
        buf[off + 7],
        buf[off + 8],
        buf[off + 9],
        buf[off + 10],
        buf[off + 11],
        buf[off + 12],
        buf[off + 13],
    ]);
    // 0xFFFFFFFFFFFFFFFF = "no packet finishes on this page".
    if granule == u64::MAX {
        return None;
    }
    Some(granule)
}

// ── WMA / ASF ───────────────────────────────────────────────────────────────

pub struct WmaParse {
    pub audio: AudioMeta,
    pub raw: BTreeMap<String, serde_json::Value>,
}

const ASF_HEADER: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
const ASF_FILE_PROPS: [u8; 16] = [
    0xA1, 0xDC, 0xAB, 0x8C, 0x47, 0xA9, 0xCF, 0x11, 0x8E, 0xE4, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65,
];
const ASF_STREAM_PROPS: [u8; 16] = [
    0x91, 0x07, 0xDC, 0xB7, 0xB7, 0xA9, 0xCF, 0x11, 0x8E, 0xE6, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65,
];
const ASF_AUDIO_MEDIA: [u8; 16] = [
    0x40, 0x9E, 0x69, 0xF8, 0x4D, 0x5B, 0xD2, 0x11, 0xAE, 0x00, 0x00, 0xC0, 0x3C, 0x02, 0xD8, 0x53,
];
const ASF_CONTENT_DESC: [u8; 16] = [
    0x33, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
const ASF_EXT_CONTENT: [u8; 16] = [
    0x40, 0xA4, 0xD0, 0xD2, 0x07, 0xE3, 0xD2, 0x11, 0x97, 0xF0, 0x00, 0xA0, 0xC9, 0x5E, 0xA8, 0x50,
];

/// Parse WMA/ASF header objects: file props, stream props, content descriptions.
pub fn parse_wma(path: &Path) -> Result<WmaParse> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);

    let mut raw: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut audio = AudioMeta {
        container: Some("WMA".into()),
        codec: Some("WMA".into()),
        ..Default::default()
    };

    // Top-level Header Object:
    // GUID(16) + size(8) + object_count(4) + reserved(2) = 30 bytes.
    // Child objects begin at offset 30.
    let mut hdr = [0u8; 30];
    r.read_exact(&mut hdr)?;
    if hdr[0..16] != ASF_HEADER {
        return Err(Error::other("not an ASF/WMA file"));
    }
    let header_size = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
    let body_end = header_size.max(30);

    // Walk child objects inside the header.
    let mut pos = 30u64;
    while pos + 24 <= body_end {
        let mut obj_hdr = [0u8; 24];
        if r.read_exact(&mut obj_hdr).is_err() {
            break;
        }
        let guid = &obj_hdr[0..16];
        let obj_size = u64::from_le_bytes(obj_hdr[16..24].try_into().unwrap());
        if obj_size < 24 {
            break;
        }
        let data_len = (obj_size - 24).min(body_end.saturating_sub(pos + 24)) as usize;
        let mut data = vec![0u8; data_len];
        if r.read_exact(&mut data).is_err() {
            break;
        }

        if guid == ASF_FILE_PROPS {
            parse_asf_file_props(&data, &mut audio, &mut raw);
        } else if guid == ASF_STREAM_PROPS {
            parse_asf_stream_props(&data, &mut audio, &mut raw);
        } else if guid == ASF_CONTENT_DESC {
            parse_asf_content_desc(&data, &mut raw);
        } else if guid == ASF_EXT_CONTENT {
            parse_asf_ext_content(&data, &mut raw);
        }

        pos += obj_size;
    }

    Ok(WmaParse { audio, raw })
}

fn parse_asf_file_props(
    data: &[u8],
    audio: &mut AudioMeta,
    raw: &mut BTreeMap<String, serde_json::Value>,
) {
    // file_id(16) file_size(8) creation(8) packets(8) play_duration(8) send(8)
    // preroll(8) flags(4) min_pkt(4) max_pkt(4) max_bitrate(4) = 80 bytes
    if data.len() < 80 {
        return;
    }
    let play_duration = u64::from_le_bytes(data[40..48].try_into().unwrap());
    // ASF File Properties has Maximum Bitrate (offset 76), not average.
    let max_bitrate = u32::from_le_bytes(data[76..80].try_into().unwrap());
    // play_duration is in 100-nanosecond units.
    if play_duration > 0 {
        audio.duration_ms = Some(play_duration / 10_000);
    }
    if max_bitrate > 0 {
        audio.bitrate = Some(max_bitrate);
    }
    raw.insert(
        "asf_file_props".into(),
        serde_json::json!({
            "play_duration_100ns": play_duration,
            "max_bitrate_bps": max_bitrate,
        }),
    );
}

fn parse_asf_stream_props(
    data: &[u8],
    audio: &mut AudioMeta,
    raw: &mut BTreeMap<String, serde_json::Value>,
) {
    // stream_type(16) err_corr(16) time_offset(8) type_len(4) err_len(4) flags(2) reserved(4)
    // then type-specific data (audio: codec(2) channels(2) rate(4) avg_bytes(4) block(2) bits(2))
    if data.len() < 54 {
        return;
    }
    if data[0..16] != ASF_AUDIO_MEDIA {
        return;
    }
    let type_len = u32::from_le_bytes(data[40..44].try_into().unwrap()) as usize;
    let type_off = 54;
    if type_off + type_len > data.len() || type_len < 16 {
        return;
    }
    let tsd = &data[type_off..type_off + type_len];
    let codec_id = u16::from_le_bytes([tsd[0], tsd[1]]);
    let channels = u16::from_le_bytes([tsd[2], tsd[3]]);
    let sample_rate = u32::from_le_bytes(tsd[4..8].try_into().unwrap());
    let bits = u16::from_le_bytes([tsd[14], tsd[15]]);

    if sample_rate > 0 {
        audio.sample_rate = Some(sample_rate);
    }
    if channels > 0 {
        audio.channels = Some(channels);
        audio.channel_layout = layout_for_channels(channels);
    }
    if bits > 0 {
        audio.bit_depth = Some(bits);
    }
    audio.codec = Some(match codec_id {
        0x0160 => "WMA v1".into(),
        0x0161 => "WMA v2".into(),
        0x0162 => "WMA Pro".into(),
        0x0163 => "WMA Lossless".into(),
        other => format!("WMA codec 0x{other:04X}"),
    });
    raw.insert(
        "asf_stream_props".into(),
        serde_json::json!({
            "codec_id": codec_id,
            "channels": channels,
            "sample_rate": sample_rate,
            "bits_per_sample": bits,
        }),
    );
}

fn parse_asf_content_desc(data: &[u8], raw: &mut BTreeMap<String, serde_json::Value>) {
    // five u16 lengths (bytes, UTF-16LE incl. null) then five strings
    if data.len() < 10 {
        return;
    }
    let lens: Vec<usize> = (0..5)
        .map(|i| u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]) as usize)
        .collect();
    let names = ["title", "author", "copyright", "description", "rating"];
    let mut off = 10;
    for (name, len) in names.iter().zip(lens.iter()) {
        if off + len > data.len() {
            break;
        }
        let s = utf16le_str(&data[off..off + len]);
        off += len;
        if !s.is_empty() {
            raw.insert(format!("asf_{name}"), serde_json::json!(s));
        }
    }
}

fn parse_asf_ext_content(data: &[u8], raw: &mut BTreeMap<String, serde_json::Value>) {
    if data.len() < 2 {
        return;
    }
    let count = u16::from_le_bytes([data[0], data[1]]) as usize;
    let mut off = 2;
    for _ in 0..count {
        if off + 2 > data.len() {
            break;
        }
        let name_len = u16::from_le_bytes([data[off], data[off + 1]]) as usize;
        off += 2;
        if off + name_len + 2 > data.len() {
            break;
        }
        let name = utf16le_str(&data[off..off + name_len]);
        off += name_len;
        let value_type = u16::from_le_bytes([data[off], data[off + 1]]);
        off += 2;
        if off + 2 > data.len() {
            break;
        }
        let value_len = u16::from_le_bytes([data[off], data[off + 1]]) as usize;
        off += 2;
        if off + value_len > data.len() {
            break;
        }
        let value = match value_type {
            0 => serde_json::json!(utf16le_str(&data[off..off + value_len])),
            3 => {
                if value_len >= 4 {
                    serde_json::json!(u32::from_le_bytes(data[off..off + 4].try_into().unwrap()))
                } else {
                    serde_json::Value::Null
                }
            }
            4 => {
                if value_len >= 8 {
                    serde_json::json!(u64::from_le_bytes(data[off..off + 8].try_into().unwrap()))
                } else {
                    serde_json::Value::Null
                }
            }
            _ => serde_json::Value::Null,
        };
        off += value_len;
        if !name.is_empty() {
            let key = format!("asf_{}", name.to_ascii_lowercase().replace(' ', "_"));
            raw.insert(key, value);
        }
    }
}

/// Decode UTF-16LE bytes (possibly with trailing null).
fn utf16le_str(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units).trim().to_string()
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

    // ── OGG / Opus ─────────────────────────────────────────────────────────

    /// Build a single-payload Ogg page.
    fn ogg_page(payload: &[u8], granule: u64, seq: u32) -> Vec<u8> {
        let mut seg_table = Vec::new();
        let mut remaining = payload.len();
        while remaining >= 255 {
            seg_table.push(255u8);
            remaining -= 255;
        }
        seg_table.push(remaining as u8);

        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.push(0); // version
        page.push(if seq == 0 { 0x02 } else { 0 }); // BOS on first page
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&1u32.to_le_bytes()); // serial
        page.extend_from_slice(&seq.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes()); // checksum (unused by parser)
        page.push(seg_table.len() as u8);
        page.extend_from_slice(&seg_table);
        page.extend_from_slice(payload);
        page
    }

    #[test]
    fn ogg_vorbis_id_and_comment_are_parsed() {
        // Vorbis id packet
        let mut id = vec![0x01];
        id.extend_from_slice(b"vorbis");
        id.extend_from_slice(&0u32.to_le_bytes()); // version
        id.push(2); // channels
        id.extend_from_slice(&44100u32.to_le_bytes());
        id.extend_from_slice(&(-1i32).to_le_bytes()); // max
        id.extend_from_slice(&128000i32.to_le_bytes()); // nominal
        id.extend_from_slice(&(-1i32).to_le_bytes()); // min
        id.push(0xB0); // blocksize 11, 8 → 0xB0? (4+4 bits: 8,11) use 0xB0
        id.push(1); // framing

        // Vorbis comment packet
        let mut cmt = vec![0x03];
        cmt.extend_from_slice(b"vorbis");
        let vendor = b"soundbuch-test";
        cmt.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        cmt.extend_from_slice(vendor);
        cmt.extend_from_slice(&1u32.to_le_bytes());
        let entry = "TITLE=上海·雨夜".as_bytes();
        cmt.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        cmt.extend_from_slice(entry);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.ogg");
        let mut f = File::create(&path).unwrap();
        f.write_all(&ogg_page(&id, 0, 0)).unwrap();
        f.write_all(&ogg_page(&cmt, 0, 1)).unwrap();
        // Final page with granule = 44100 samples → 1 second
        f.write_all(&ogg_page(&[0x00], 44100, 2)).unwrap();
        drop(f);

        let parsed = parse_ogg(&path).unwrap();
        assert_eq!(parsed.audio.codec.as_deref(), Some("Vorbis"));
        assert_eq!(parsed.audio.sample_rate, Some(44100));
        assert_eq!(parsed.audio.channels, Some(2));
        assert_eq!(parsed.audio.bitrate, Some(128_000));
        assert_eq!(parsed.audio.duration_ms, Some(1000));
        assert_eq!(
            parsed.raw.get("vorbis_title"),
            Some(&serde_json::json!("上海·雨夜"))
        );
    }

    #[test]
    fn ogg_opus_head_and_tags_are_parsed() {
        let mut head = Vec::new();
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(1); // channels
        head.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
        head.extend_from_slice(&48000u32.to_le_bytes()); // input sample rate
        head.extend_from_slice(&0i16.to_le_bytes()); // gain
        head.push(0); // mapping family

        let mut tags = Vec::new();
        tags.extend_from_slice(b"OpusTags");
        let vendor = b"libopus";
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&1u32.to_le_bytes());
        let entry = "ARTIST=山田太郎".as_bytes();
        tags.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        tags.extend_from_slice(entry);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.opus");
        let mut f = File::create(&path).unwrap();
        f.write_all(&ogg_page(&head, 0, 0)).unwrap();
        f.write_all(&ogg_page(&tags, 0, 1)).unwrap();
        // granule at 48 kHz: 96000 samples = 2 seconds
        f.write_all(&ogg_page(&[0x00], 96000, 2)).unwrap();
        drop(f);

        let parsed = parse_ogg(&path).unwrap();
        assert_eq!(parsed.audio.codec.as_deref(), Some("Opus"));
        assert_eq!(parsed.audio.sample_rate, Some(48000));
        assert_eq!(parsed.audio.channels, Some(1));
        assert_eq!(parsed.audio.duration_ms, Some(2000));
        assert_eq!(
            parsed.raw.get("opus_artist"),
            Some(&serde_json::json!("山田太郎"))
        );
    }

    // ── WMA / ASF ──────────────────────────────────────────────────────────

    fn asf_object(guid: [u8; 16], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&guid);
        v.extend_from_slice(&(24 + body.len() as u64).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn wma_file_and_stream_props_are_parsed() {
        // File Properties body: file_id(16) + size(8) + creation(8) + packets(8)
        // + play_duration(8) + send(8) + preroll(8) + flags(4) + min(4)+max(4)+avg(4)
        let mut file_body = vec![0u8; 40];
        // play_duration at offset 40: 5_000_000 * 100ns = 500 ms → wait, 500ms = 5_000_000/10_000
        // we want 3000ms → 30_000_000 (100-ns units)
        file_body.extend_from_slice(&30_000_000u64.to_le_bytes()); // play_duration
        file_body.extend_from_slice(&0u64.to_le_bytes()); // send
        file_body.extend_from_slice(&0u64.to_le_bytes()); // preroll
        file_body.extend_from_slice(&0u32.to_le_bytes()); // flags
        file_body.extend_from_slice(&0u32.to_le_bytes()); // min
        file_body.extend_from_slice(&0u32.to_le_bytes()); // max
        file_body.extend_from_slice(&64000u32.to_le_bytes()); // max bitrate

        // Stream Properties body
        let mut stream_body = Vec::new();
        stream_body.extend_from_slice(&ASF_AUDIO_MEDIA);
        stream_body.extend_from_slice(&[0u8; 16]); // error correction
        stream_body.extend_from_slice(&0u64.to_le_bytes()); // time offset
        stream_body.extend_from_slice(&16u32.to_le_bytes()); // type-specific length
        stream_body.extend_from_slice(&0u32.to_le_bytes()); // err corr length
        stream_body.extend_from_slice(&0u16.to_le_bytes()); // flags
        stream_body.extend_from_slice(&0u32.to_le_bytes()); // reserved

        // type-specific: codec(2) channels(2) rate(4) avg_bytes(4) block(2) bits(2)
        stream_body.extend_from_slice(&0x0161u16.to_le_bytes()); // WMA v2
        stream_body.extend_from_slice(&2u16.to_le_bytes()); // channels
        stream_body.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
        stream_body.extend_from_slice(&8000u32.to_le_bytes()); // avg bytes/sec
        stream_body.extend_from_slice(&0u16.to_le_bytes()); // block align
        stream_body.extend_from_slice(&16u16.to_le_bytes()); // bits

        // Content Description: title only
        let title = "上海·雨夜";
        let title_utf16: Vec<u8> = title
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .chain([0, 0])
            .collect();
        let mut cd_body = Vec::new();
        cd_body.extend_from_slice(&(title_utf16.len() as u16).to_le_bytes());
        cd_body.extend_from_slice(&0u16.to_le_bytes()); // author
        cd_body.extend_from_slice(&0u16.to_le_bytes());
        cd_body.extend_from_slice(&0u16.to_le_bytes());
        cd_body.extend_from_slice(&0u16.to_le_bytes());
        cd_body.extend_from_slice(&title_utf16);

        let file_obj = asf_object(ASF_FILE_PROPS, &file_body);
        let stream_obj = asf_object(ASF_STREAM_PROPS, &stream_body);
        let cd_obj = asf_object(ASF_CONTENT_DESC, &cd_body);

        let header_size = 24 + file_obj.len() + stream_obj.len() + cd_obj.len();
        let mut head = Vec::new();
        head.extend_from_slice(&ASF_HEADER);
        head.extend_from_slice(&(header_size as u64).to_le_bytes());
        head.extend_from_slice(&3u32.to_le_bytes()); // object count
        head.extend_from_slice(&0x0102u16.to_le_bytes());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wma");
        let mut f = File::create(&path).unwrap();
        f.write_all(&head).unwrap();
        f.write_all(&file_obj).unwrap();
        f.write_all(&stream_obj).unwrap();
        f.write_all(&cd_obj).unwrap();
        drop(f);

        let parsed = parse_wma(&path).unwrap();
        assert_eq!(parsed.audio.duration_ms, Some(3000));
        assert_eq!(parsed.audio.sample_rate, Some(44100));
        assert_eq!(parsed.audio.channels, Some(2));
        assert_eq!(parsed.audio.bit_depth, Some(16));
        assert_eq!(parsed.audio.bitrate, Some(64000));
        assert_eq!(parsed.audio.codec.as_deref(), Some("WMA v2"));
        assert_eq!(
            parsed.raw.get("asf_title"),
            Some(&serde_json::json!("上海·雨夜"))
        );
    }
}
