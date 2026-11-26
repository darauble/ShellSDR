#!/usr/bin/env python3
"""Objective A/B of two demodulated-audio WAVs (reference vs candidate)."""
import sys, wave
import numpy as np
from scipy.signal import welch, butter, sosfilt

def load(p):
    with wave.open(p, 'rb') as w:
        sr, n, ch, sw = w.getframerate(), w.getnframes(), w.getnchannels(), w.getsampwidth()
        raw = w.readframes(n)
    x = np.frombuffer(raw, '<i2').astype(np.float64) / 32768.0 if sw == 2 else np.frombuffer(raw, '<f4').astype(np.float64)
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    return sr, x

def db(v): return 10 * np.log10(np.maximum(v, 1e-20))

def frame_rms(x, sr, ms=20):
    n = int(sr * ms / 1000)
    k = len(x) // n
    return np.sqrt(np.mean(x[:k*n].reshape(k, n)**2, axis=1))

def analyze(name, sr, x):
    r = {}
    a = np.abs(x); pk = a.max()
    r['rms'] = np.sqrt(np.mean(x**2)); r['peak'] = pk
    r['crest_db'] = 20*np.log10(pk / r['rms'])
    r['rail_pct'] = 100*np.mean(a >= 0.999*pk)
    r['dc'] = np.mean(x)
    # Speech-band signal vs out-of-band noise (2.9-4 kHz has no SSB content).
    f, p = welch(x, fs=sr, nperseg=4096)
    band = lambda lo, hi: np.trapezoid(p[(f>=lo)&(f<hi)], f[(f>=lo)&(f<hi)])
    r['inband'] = band(300, 2400); r['oob'] = band(2900, 4000)
    r['inband_oob_db'] = db(r['inband']) - db(r['oob'] * (2100/1100))  # per-Hz
    # Spectral flatness in-band (1 = white noise, ->0 = tonal/speech-shaped).
    m = (f >= 300) & (f < 2400)
    r['flatness'] = np.exp(np.mean(np.log(p[m] + 1e-20))) / np.mean(p[m])
    # Dynamics: 20 ms frame-RMS percentiles -> speech/pause contrast (an SNR proxy).
    bp = sosfilt(butter(4, [300, 2400], btype='band', fs=sr, output='sos'), x)
    fr = frame_rms(bp, sr)
    p10, p50, p90 = np.percentile(fr, [10, 50, 90])
    r['frame_p90_p10_db'] = 20*np.log10(p90 / max(p10, 1e-9))
    r['frame_p50_p10_db'] = 20*np.log10(p50 / max(p10, 1e-9))
    # Spectral centroid (brightness) in 100-3000 Hz.
    m2 = (f >= 100) & (f < 3000)
    r['centroid'] = np.sum(f[m2]*p[m2]) / np.sum(p[m2])
    return r, f, p

sr1, a = load(sys.argv[1]); sr2, b = load(sys.argv[2])
ra, fa, pa = analyze('REF', sr1, a); rb, fb, pb = analyze('CAND', sr2, b)
print(f"durations: ref {len(a)/sr1:.1f}s, cand {len(b)/sr2:.1f}s")
rows = [
    ('rms', '{:.3f}'), ('peak', '{:.3f}'), ('crest_db', '{:.1f}'), ('rail_pct', '{:.2f}'),
    ('dc', '{:.4f}'), ('inband_oob_db', '{:.1f}'), ('flatness', '{:.3f}'),
    ('frame_p90_p10_db', '{:.1f}'), ('frame_p50_p10_db', '{:.1f}'), ('centroid', '{:.0f}'),
]
print(f"{'metric':22s} {'SDR++':>10s} {'ShellSDR':>10s}")
for k, fmt in rows:
    print(f"{k:22s} {fmt.format(ra[k]):>10s} {fmt.format(rb[k]):>10s}")

# Normalized PSD shape difference (each normalized to its own 300-2400 Hz mean).
def shape(f, p):
    return db(p / np.mean(p[(f >= 300) & (f < 2400)]))
sa, sb = shape(fa, pa), shape(fb, pb)
edges = [0, 100, 200, 300, 500, 800, 1200, 1600, 2000, 2300, 2500, 2700, 2900, 3500, 5000, 8000, 12000, 24000]
print(f"\n{'band Hz':>13s} {'SDR++':>7s} {'Shell':>7s} {'diff':>6s}   (dB re own 300-2400 mean)")
for lo, hi in zip(edges[:-1], edges[1:]):
    m = (fa >= lo) & (fa < hi)
    va = db(np.mean(10**(sa[m]/10))); vb = db(np.mean(10**(sb[m]/10)))
    print(f"{lo:5d}-{hi:6d} {va:7.1f} {vb:7.1f} {vb-va:6.1f}")

# Passband edges: where the shape falls 6 dB below the in-band mean.
def edges_of(f, s):
    m = (f > 50) & (f < 4000)
    ok = f[m][s[m] > -6]
    return (ok.min(), ok.max()) if len(ok) else (0, 0)
print("\n-6 dB passband: SDR++ {:.0f}-{:.0f} Hz, ShellSDR {:.0f}-{:.0f} Hz".format(*edges_of(fa, sa), *edges_of(fb, sb)))
