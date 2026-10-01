use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::error::{Error, Result};
use crate::metadata::audio::layout_for_channels;
use crate::models::{AudioMeta, GpsMeta};

pub struct WavParse {
    pub audio: AudioMeta,
    pub gps: Option<GpsMeta>,
    pub raw: BTreeMap<String, serde_json::Value>,
    pub warning: Option<String>,
}

/// Parse RIFF/WAVE: `fmt ` for technical params, `bext` for BWF,
/// `iXML` for production metadata. Unknown chunks land in `raw` — never dropped.
///
/// Large chunks (especially `data` PCM payloads) are skipped in-place and never
/// buffered whole — a 2 GB recording must not allocate 2 GB just to parse headers.
pub fn parse_wav(path: &Path) -> Result<WavParse> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);

    let mut riff = [0u8; 12];
    r.read_exact(&mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(Error::other("not a RIFF/WAVE file"));
    }

    let mut audio = AudioMeta {
        container: Some("WAV".into()),
        codec: Some("PCM".into()),
        ..Default::default()
    };
    let mut gps = None;
    let mut raw: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut warning = None;
    let mut data_size: Option<u32> = None;

    // Cap how much of a metadata chunk we buffer. Anything larger is skipped
    // with a warning — never a process-killing allocation.
    const MAX_META_CHUNK: usize = 8 * 1024 * 1024;

    loop {
        let mut hdr = [0u8; 8];
        if r.read_exact(&mut hdr).is_err() {
            break;
        }
        let id = String::from_utf8_lossy(&hdr[0..4]).to_string();
        let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        // Word-aligned after the payload.
        let padded = size + (size % 2);

        // `data` is the audio body — record size and skip without buffering.
        if id == "data" {
            data_size = Some(size as u32);
            let skipped = std::io::copy(&mut r.by_ref().take(padded as u64), &mut std::io::sink())?;
            if skipped < padded as u64 {
                warning = Some("truncated data chunk".into());
                break;
            }
            continue;
        }

        if size > MAX_META_CHUNK {
            // Skip oversized metadata; preserve a summary only.
            let mut preview = vec![0u8; 64];
            let n = r.read(&mut preview).unwrap_or(0);
            preview.truncate(n);
            let skipped = std::io::copy(
                &mut r.by_ref().take(padded.saturating_sub(n) as u64),
                &mut std::io::sink(),
            )?;
            let _ = skipped;
            raw.insert(
                format!("chunk_{id}"),
                serde_json::json!({
                    "size": size,
                    "skipped": true,
                    "preview_lossy": lossy_preview(&preview),
                }),
            );
            if let Some(w) = warning.as_mut() {
                *w = format!("{w}; oversized chunk {id} skipped");
            } else {
                warning = Some(format!("oversized chunk {id} skipped"));
            }
            continue;
        }

        let mut data = vec![0u8; size];
        if r.read_exact(&mut data).is_err() {
            warning = Some(format!("truncated chunk {id}"));
            break;
        }
        if size % 2 == 1 {
            let mut pad = [0u8; 1];
            let _ = r.read_exact(&mut pad);
        }

        match id.as_str() {
            "fmt " => {
                if data.len() >= 16 {
                    let format = u16::from_le_bytes([data[0], data[1]]);
                    let channels = u16::from_le_bytes([data[2], data[3]]);
                    let sample_rate = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                    let byte_rate = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
                    let bits = u16::from_le_bytes([data[14], data[15]]);
                    audio.channels = Some(channels);
                    audio.sample_rate = Some(sample_rate);
                    audio.bit_depth = Some(bits);
                    audio.channel_layout = layout_for_channels(channels);
                    audio.codec = Some(match format {
                        1 => "PCM".into(),
                        3 => "IEEE_FLOAT".into(),
                        0xFFFE => "EXTENSIBLE".into(),
                        other => format!("format_{other}"),
                    });
                    if byte_rate > 0 {
                        audio.bitrate = Some(byte_rate.saturating_mul(8));
                    }
                }
            }
            "bext" => {
                apply_bext(&data, &mut audio, &mut raw);
            }
            "iXML" => {
                if let Some(g) = apply_ixml(&data, &mut raw) {
                    gps = Some(g);
                }
            }
            "LIST" => {
                raw.insert(
                    format!("chunk_LIST_{}", raw.len()),
                    serde_json::json!({ "size": size, "preview_lossy": lossy_preview(&data) }),
                );
            }
            other => {
                raw.insert(
                    format!("chunk_{other}"),
                    serde_json::json!({ "size": size, "preview_lossy": lossy_preview(&data) }),
                );
            }
        }
    }

    if let (Some(sr), Some(data_size)) = (audio.sample_rate, data_size) {
        if sr > 0 {
            if let Some(ch) = audio.channels {
                let frame_size = (ch as u32) * (audio.bit_depth.unwrap_or(16) as u32 / 8).max(1);
                if frame_size > 0 {
                    audio.duration_ms =
                        Some(data_size as u64 * 1000 / (frame_size as u64 * sr as u64).max(1));
                    // More precisely: duration = samples / sample_rate
                    let samples = data_size as u64 / frame_size as u64;
                    audio.duration_ms = Some(samples * 1000 / sr as u64);
                }
            }
        }
    }

    Ok(WavParse {
        audio,
        gps,
        raw,
        warning,
    })
}

/// BWF `bext` chunk (EBU Tech 3285).
/// Offsets: description 0..256, originator 256..288, originator_reference 288..320,
/// origination_date 320..330, origination_time 330..338, time_reference 338..346,
/// version 346..348, umid 348..413, loudness 413..425, ...
fn apply_bext(data: &[u8], audio: &mut AudioMeta, raw: &mut BTreeMap<String, serde_json::Value>) {
    let _ = audio;
    let get = |start: usize, end: usize| -> String {
        if data.len() >= end {
            let s = &data[start..end];
            let s = s
                .iter()
                .take_while(|b| **b != 0)
                .copied()
                .collect::<Vec<u8>>();
            String::from_utf8_lossy(&s).trim().to_string()
        } else {
            String::new()
        }
    };

    let description = get(0, 256);
    let originator = get(256, 288);
    let originator_reference = get(288, 320);
    let origination_date = get(320, 330);
    let origination_time = get(330, 338);
    let mut time_reference: Option<u64> = None;
    if data.len() >= 346 {
        let lo = u32::from_le_bytes([data[338], data[339], data[340], data[341]]);
        let hi = u32::from_le_bytes([data[342], data[343], data[344], data[345]]);
        time_reference = Some(((hi as u64) << 32) | lo as u64);
    }
    let mut umid = String::new();
    if data.len() >= 413 {
        umid = hex::encode(&data[348..413]);
    }
    let coding_history = if data.len() > 602 {
        let s = &data[602..];
        let s = s
            .iter()
            .take_while(|b| **b != 0)
            .copied()
            .collect::<Vec<u8>>();
        String::from_utf8_lossy(&s).trim().to_string()
    } else {
        String::new()
    };

    if !description.is_empty() {
        raw.insert("bwf_description".into(), serde_json::json!(description));
    }
    if !originator.is_empty() {
        raw.insert("bwf_originator".into(), serde_json::json!(originator));
    }
    if !originator_reference.is_empty() {
        raw.insert(
            "bwf_originator_reference".into(),
            serde_json::json!(originator_reference),
        );
    }
    if !origination_date.is_empty() {
        raw.insert(
            "bwf_origination_date".into(),
            serde_json::json!(origination_date),
        );
    }
    if !origination_time.is_empty() {
        raw.insert(
            "bwf_origination_time".into(),
            serde_json::json!(origination_time),
        );
    }
    if let Some(tr) = time_reference {
        raw.insert("bwf_time_reference".into(), serde_json::json!(tr));
    }
    if !umid.is_empty() {
        raw.insert("bwf_umid".into(), serde_json::json!(umid));
    }
    if !coding_history.is_empty() {
        raw.insert(
            "bwf_coding_history".into(),
            serde_json::json!(coding_history),
        );
    }
    raw.insert("bwf_present".into(), serde_json::json!(true));
}

/// iXML chunk (XML). Extract BWF-like + GPS when present; keep full text in raw.
fn apply_ixml(data: &[u8], raw: &mut BTreeMap<String, serde_json::Value>) -> Option<GpsMeta> {
    let text = String::from_utf8_lossy(data).to_string();
    raw.insert(
        "ixml_raw".into(),
        serde_json::json!(text.trim_end_matches('\0')),
    );

    // Very small targeted extractions — no full XML dependency in MVP.
    let mut gps = GpsMeta {
        raw: Some(serde_json::json!({ "ixml_present": true })),
        ..Default::default()
    };
    if let Some(lat) = extract_tag(&text, "LATITUDE").and_then(|s| s.parse().ok()) {
        gps.latitude = Some(lat);
    }
    if let Some(lon) = extract_tag(&text, "LONGITUDE").and_then(|s| s.parse().ok()) {
        gps.longitude = Some(lon);
    }
    if let Some(alt) = extract_tag(&text, "ALTITUDE").and_then(|s| s.parse().ok()) {
        gps.altitude = Some(alt);
    }
    if let Some(note) = extract_tag(&text, "NOTE") {
        raw.insert("ixml_note".into(), serde_json::json!(note));
    }
    if let Some(scene) = extract_tag(&text, "SCENE") {
        raw.insert("ixml_scene".into(), serde_json::json!(scene));
    }
    if let Some(take) = extract_tag(&text, "TAKE") {
        raw.insert("ixml_take".into(), serde_json::json!(take));
    }

    if gps.latitude.is_some() || gps.longitude.is_some() {
        Some(gps)
    } else {
        None
    }
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

fn lossy_preview(data: &[u8]) -> String {
    let n = data.len().min(64);
    String::from_utf8_lossy(&data[..n]).replace('\0', "·")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_min_wav(file: &mut File, sample_rate: u32, channels: u16, bits: u16, frames: u32) {
        let block_align = channels * bits / 8;
        let byte_rate = sample_rate * block_align as u32;
        let data_size = frames * block_align as u32;
        let riff_size = 36 + data_size;
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&riff_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits.to_le_bytes());
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_size.to_le_bytes());
        buf.extend_from_slice(&vec![0u8; data_size as usize]);
        file.write_all(&buf).unwrap();
    }

    #[test]
    fn parses_basic_wav_technical_params() {
        let mut f = NamedTempFile::new().unwrap();
        write_min_wav(f.as_file_mut(), 48000, 2, 24, 48000); // 1 second
        let p = parse_wav(f.path()).unwrap();
        assert_eq!(p.audio.sample_rate, Some(48000));
        assert_eq!(p.audio.bit_depth, Some(24));
        assert_eq!(p.audio.channels, Some(2));
        assert_eq!(p.audio.container.as_deref(), Some("WAV"));
        assert_eq!(p.audio.duration_ms, Some(1000));
    }

    #[test]
    fn ixml_gps_and_unknown_chunks_preserved() {
        let mut f = NamedTempFile::new().unwrap();
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&100u32.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        // fmt
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&44100u32.to_le_bytes());
        buf.extend_from_slice(&88200u32.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        // iXML
        let xml = b"<BWF><LATITUDE>33.4996</LATITUDE><LONGITUDE>126.5312</LONGITUDE><SCENE>01</SCENE></BWF>";
        buf.extend_from_slice(b"iXML");
        buf.extend_from_slice(&(xml.len() as u32).to_le_bytes());
        buf.extend_from_slice(xml);
        if xml.len() % 2 == 1 {
            buf.push(0);
        }
        // unknown chunk
        buf.extend_from_slice(b"junk");
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(b"abcd");
        // data
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&0u32.to_le_bytes());
        f.write_all(&buf).unwrap();

        let p = parse_wav(f.path()).unwrap();
        let gps = p.gps.expect("gps from ixml");
        assert!((gps.latitude.unwrap() - 33.4996).abs() < 1e-6);
        assert!((gps.longitude.unwrap() - 126.5312).abs() < 1e-6);
        assert!(p.raw.contains_key("ixml_raw"));
        assert!(p.raw.contains_key("chunk_junk"));
        assert_eq!(p.raw.get("ixml_scene").unwrap(), &serde_json::json!("01"));
    }

    /// A `data` chunk that *claims* a huge size must not allocate that much.
    /// (Previously `vec![0u8; size]` ran before reading — OOM / process abort.)
    #[test]
    fn huge_claimed_data_chunk_does_not_oom() {
        let mut f = NamedTempFile::new().unwrap();
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&64u32.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&48000u32.to_le_bytes());
        buf.extend_from_slice(&96000u32.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        // Claim 512 MiB of PCM but only write a few bytes.
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&((512u32) * 1024 * 1024).to_le_bytes());
        buf.extend_from_slice(&[0u8; 8]);
        f.write_all(&buf).unwrap();

        let p = parse_wav(f.path()).unwrap();
        assert_eq!(p.audio.sample_rate, Some(48000));
        assert_eq!(p.audio.channels, Some(1));
    }

    /// Oversized non-data metadata chunk is skipped, not buffered whole.
    #[test]
    fn oversized_meta_chunk_is_skipped() {
        let mut f = NamedTempFile::new().unwrap();
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&64u32.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&44100u32.to_le_bytes());
        buf.extend_from_slice(&88200u32.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        // 20 MiB "junk" chunk — over the 8 MiB metadata cap.
        buf.extend_from_slice(b"junk");
        buf.extend_from_slice(&((20u32) * 1024 * 1024).to_le_bytes());
        buf.extend_from_slice(&[0u8; 128]);
        f.write_all(&buf).unwrap();

        let p = parse_wav(f.path()).unwrap();
        assert_eq!(p.audio.sample_rate, Some(44100));
    }
}
