pub mod audio;
pub mod bwf;
pub mod compressed;

use std::collections::BTreeMap;
use std::path::Path;

use crate::models::{AudioMeta, FilesystemMeta, GpsMeta, MetadataSource, MetadataValue};

/// Extract everything we understand from a source file.
/// Returns raw JSON for anything we don't — never dropped.
/// Parse problems go into `errors` and must NOT abort the import.
/// A parser panic is caught so one bad file can never kill the process.
pub fn extract(path: &Path) -> (AudioMeta, GpsMeta, BTreeMap<String, serde_json::Value>, Vec<String>) {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract_inner(path))) {
        Ok(v) => v,
        Err(_) => (
            AudioMeta::default(),
            GpsMeta::default(),
            BTreeMap::new(),
            vec!["metadata parser panicked; import continues without technical fields".into()],
        ),
    }
}

fn extract_inner(
    path: &Path,
) -> (AudioMeta, GpsMeta, BTreeMap<String, serde_json::Value>, Vec<String>) {
    let mut errors = Vec::new();
    let mut raw: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    let mut audio = AudioMeta::default();
    let mut gps = GpsMeta::default();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "wav" | "bwf" => match bwf::parse_wav(path) {
            Ok(parsed) => {
                apply_audio(&mut audio, &parsed.audio);
                if let Some(g) = parsed.gps {
                    gps = g;
                }
                for (k, v) in parsed.raw {
                    raw.insert(k, v);
                }
                if let Some(err) = parsed.warning {
                    errors.push(err);
                }
            }
            Err(e) => errors.push(format!("wav parse: {e}")),
        },
        "aif" | "aiff" => match audio::parse_aiff_header(path) {
            Ok(a) => apply_audio(&mut audio, &a),
            Err(e) => errors.push(format!("aiff parse: {e}")),
        },
        "flac" => match audio::parse_flac_streaminfo(path) {
            Ok(a) => apply_audio(&mut audio, &a),
            Err(e) => errors.push(format!("flac parse: {e}")),
        },
        "mp3" => match compressed::parse_mp3(path) {
            Ok(parsed) => {
                apply_audio(&mut audio, &parsed.audio);
                for (k, v) in parsed.raw {
                    raw.insert(k, v);
                }
            }
            Err(e) => {
                audio.container = Some("MP3".into());
                errors.push(format!("mp3 parse: {e}; file preserved"));
            }
        },
        "m4a" | "aac" | "mp4" => match compressed::parse_mp4_audio(path) {
            Ok(parsed) => {
                apply_audio(&mut audio, &parsed.audio);
                for (k, v) in parsed.raw {
                    raw.insert(k, v);
                }
            }
            Err(e) => {
                audio.container = Some(ext.to_uppercase());
                errors.push(format!("mp4/m4a parse: {e}; file preserved"));
            }
        },
        "caf" => match compressed::parse_caf(path) {
            Ok(a) => apply_audio(&mut audio, &a),
            Err(e) => {
                audio.container = Some("CAF".into());
                errors.push(format!("caf parse: {e}; file preserved"));
            }
        },
        "wma" | "ogg" | "opus" => {
            // MVP: allow import, keep container hint, record that full parse
            // is deferred. File itself is preserved byte-for-byte.
            audio.container = Some(ext.to_uppercase());
            errors.push(format!(
                "full metadata parse not implemented for .{ext}; file preserved"
            ));
        }
        _ => {
            errors.push(format!("unknown container .{ext}"));
        }
    }

    if audio.container.is_none() {
        audio.container = Some(ext.to_uppercase());
    }

    (audio, gps, raw, errors)
}

fn apply_audio(dst: &mut AudioMeta, src: &AudioMeta) {
    if src.duration_ms.is_some() {
        dst.duration_ms = src.duration_ms;
    }
    if src.sample_rate.is_some() {
        dst.sample_rate = src.sample_rate;
    }
    if src.bit_depth.is_some() {
        dst.bit_depth = src.bit_depth;
    }
    if src.channels.is_some() {
        dst.channels = src.channels;
    }
    if src.channel_layout.is_some() {
        dst.channel_layout = src.channel_layout.clone();
    }
    if src.codec.is_some() {
        dst.codec = src.codec.clone();
    }
    if src.container.is_some() {
        dst.container = src.container.clone();
    }
    if src.bitrate.is_some() {
        dst.bitrate = src.bitrate;
    }
    if src.compression.is_some() {
        dst.compression = src.compression.clone();
    }
}

pub fn filesystem_meta(path: &Path, meta: &std::fs::Metadata) -> FilesystemMeta {
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase());
    let created_at = meta
        .created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
        .flatten();
    let modified_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
        .flatten();
    let accessed_at = meta
        .accessed()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
        .flatten();

    FilesystemMeta {
        filename,
        extension,
        file_size: meta.len(),
        created_at,
        modified_at,
        accessed_at,
        original_path: path.display().to_string(),
        source_volume: source_volume_of(path),
    }
}

/// Best-effort volume/root label for provenance. Cross-platform-ish.
pub fn source_volume_of(path: &Path) -> Option<String> {
    // On Windows: drive root like `D:\`. On Unix: first component after `/`.
    if let Some(root) = path.components().next() {
        return Some(root.as_os_str().to_string_lossy().to_string());
    }
    None
}

pub fn mime_for_extension(ext: &str) -> Option<String> {
    let m = match ext.to_ascii_lowercase().as_str() {
        "wav" | "bwf" => "audio/wav",
        "aif" | "aiff" => "audio/aiff",
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "caf" => "audio/x-caf",
        "wma" => "audio/x-ms-wma",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        _ => return None,
    };
    Some(m.to_string())
}

/// Build provenance-tracked metadata values.
pub fn build_metadata_values(
    fs: &FilesystemMeta,
    audio: &AudioMeta,
    gps: &GpsMeta,
) -> Vec<MetadataValue> {
    let mut out = Vec::new();
    let now = None;

    out.push(MetadataValue {
        key: "filename".into(),
        value: fs.filename.clone(),
        source: MetadataSource::Filesystem,
        recorded_at: now,
    });
    if let Some(sr) = audio.sample_rate {
        out.push(MetadataValue {
            key: "sample_rate".into(),
            value: sr.to_string(),
            source: MetadataSource::Filesystem,
            recorded_at: now,
        });
    }
    if let Some(bd) = audio.bit_depth {
        out.push(MetadataValue {
            key: "bit_depth".into(),
            value: bd.to_string(),
            source: MetadataSource::Filesystem,
            recorded_at: now,
        });
    }
    if let Some(ch) = audio.channels {
        out.push(MetadataValue {
            key: "channels".into(),
            value: ch.to_string(),
            source: MetadataSource::Filesystem,
            recorded_at: now,
        });
    }
    if let (Some(lat), Some(lon)) = (gps.latitude, gps.longitude) {
        out.push(MetadataValue {
            key: "latitude".into(),
            value: lat.to_string(),
            source: MetadataSource::Derived,
            recorded_at: now,
        });
        out.push(MetadataValue {
            key: "longitude".into(),
            value: lon.to_string(),
            source: MetadataSource::Derived,
            recorded_at: now,
        });
    }
    out
}
