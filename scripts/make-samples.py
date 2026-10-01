#!/usr/bin/env python3
"""Generate small valid WAV files for soundbuch local testing."""
import math
import struct
import sys
from pathlib import Path


def write_wav(path: Path, freq: float, seconds: float, rate: int = 48000, channels: int = 1):
    n = int(rate * seconds)
    frames = bytearray()
    for i in range(n):
        v = int(0.2 * 32767 * math.sin(2 * math.pi * freq * i / rate))
        for _ in range(channels):
            frames += struct.pack("<h", v)
    data = bytes(frames)
    block_align = channels * 2
    byte_rate = rate * block_align
    riff_size = 36 + len(data)
    buf = b"RIFF" + struct.pack("<I", riff_size) + b"WAVE"
    buf += b"fmt " + struct.pack("<IHHIIHH", 16, 1, channels, rate, byte_rate, block_align, 16)
    buf += b"data" + struct.pack("<I", len(data)) + data
    path.write_bytes(buf)
    print(f"  wrote {path} ({len(buf)} bytes, {freq:.0f} Hz, {seconds}s)")


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/soundhub-samples")
    out.mkdir(parents=True, exist_ok=True)
    print(f"Generating samples in {out}")
    write_wav(out / "ocean_wave.wav", 220, 1.5)
    write_wav(out / "field_wind.wav", 440, 1.5)
    write_wav(out / "night_rain.wav", 330, 1.5)
    write_wav(out / "jeju_seagull.wav", 523, 1.5)
    write_wav(out / "city_traffic_low.wav", 110, 1.5, rate=44100)
    write_wav(out / "hi_res_tone.wav", 880, 1.5, rate=96000, channels=2)
    print("Done.")


if __name__ == "__main__":
    main()
