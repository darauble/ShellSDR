#!/usr/bin/env python3
"""Compare two WAVs (SDR++ reference vs ShellSDR) with basic stats + PSD."""
import sys
import wave
import numpy as np
from scipy.signal import welch

def load(path):
    with wave.open(path, 'rb') as w:
        sr = w.getframerate()
        n = w.getnframes()
        ch = w.getnchannels()
        sw = w.getsampwidth()
        raw = w.readframes(n)
    if sw == 2:
        data = np.frombuffer(raw, dtype='<i2').astype(np.float64) / 32768.0
    elif sw == 4:
        # could be int32 or float32; try float32 first (common for IEEE float WAV)
        data = np.frombuffer(raw, dtype='<f4').astype(np.float64)
        if np.max(np.abs(data)) > 10 or np.allclose(data, 0):
            data = np.frombuffer(raw, dtype='<i4').astype(np.float64) / (2**31)
    else:
        raise RuntimeError(f"unsupported sample width {sw}")
    if ch > 1:
        data = data.reshape(-1, ch).mean(axis=1)
    return sr, data

def stats(name, sr, x):
    rms = np.sqrt(np.mean(x**2))
    peak = np.max(np.abs(x))
    clip = np.mean(np.abs(x) >= 0.999) * 100
    dc = np.mean(x)
    print(f"{name}: sr={sr} len={len(x)} ({len(x)/sr:.1f}s) rms={rms:.4f} peak={peak:.4f} clip={clip:.3f}% dc={dc:.5f}")
    return rms

def band_energy(sr, x, lo, hi):
    f, p = welch(x, fs=sr, nperseg=min(8192, len(x)))
    mask = (f >= lo) & (f <= hi)
    return np.trapezoid(p[mask], f[mask]) if mask.any() else 0.0

def main():
    ref_path, shell_path = sys.argv[1], sys.argv[2]
    sr_r, ref = load(ref_path)
    sr_s, shell = load(shell_path)
    print(f"--- {ref_path} vs {shell_path} ---")
    rms_r = stats("REF  ", sr_r, ref)
    rms_s = stats("SHELL", sr_s, shell)

    # Normalize both to the same RMS for fair spectral shape comparison.
    ref_n = ref / (rms_r + 1e-12)
    shell_n = shell / (rms_s + 1e-12)

    for lo, hi, label in [(0, 300, "sub-300Hz(hum/rumble)"), (300, 3000, "speech-band"), (3000, 6000, "upper-mid"), (6000, 8000, "near-nyq-ish"), (8000, 12000, "HF-hiss")]:
        hi_r = min(hi, sr_r/2 - 1)
        hi_s = min(hi, sr_s/2 - 1)
        be_r = band_energy(sr_r, ref_n, lo, hi_r)
        be_s = band_energy(sr_s, shell_n, lo, hi_s)
        ratio = (be_s / be_r) if be_r > 0 else float('inf')
        print(f"  band {label:22s} [{lo:5d}-{hi:5d}Hz]: ref={be_r:.6f} shell={be_s:.6f} shell/ref={ratio:.2f}x")

    # Welch PSD peak frequency (dominant tone/carrier offset check).
    f_r, p_r = welch(ref_n, fs=sr_r, nperseg=min(16384, len(ref_n)))
    f_s, p_s = welch(shell_n, fs=sr_s, nperseg=min(16384, len(shell_n)))
    print(f"  REF   peak PSD at {f_r[np.argmax(p_r)]:.1f} Hz")
    print(f"  SHELL peak PSD at {f_s[np.argmax(p_s)]:.1f} Hz")
    print()

if __name__ == "__main__":
    main()
