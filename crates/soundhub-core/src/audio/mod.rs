pub mod peaks;

pub use peaks::{
    compute_aiff_peaks, compute_file_peaks, compute_wav_peaks, PeaksCache, DEFAULT_BUCKETS,
};
