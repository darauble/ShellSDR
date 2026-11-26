#!/usr/bin/env python3
"""Print normalized PSD (dB re in-band mean) of two WAVs side by side."""
import sys, wave
import numpy as np
from scipy.signal import welch

def load(p):
    with wave.open(p, 'rb') as w:
        sr, n, ch, sw = w.getframerate(), w.getnframes(), w.getnchannels(), w.getsampwidth()
        raw = w.readframes(n)
    x = np.frombuffer(raw, dtype='<i2').astype(np.float64) / 32768.0 if sw == 2 else np.frombuffer(raw, dtype='<f4').astype(np.float64)
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    return sr, x

def psd_db(sr, x):
    f, p = welch(x, fs=sr, nperseg=4096)
    inband = p[(f >= 500) & (f <= 2000)].mean()
    return f, 10 * np.log10(p / inband + 1e-20)

sr1, a = load(sys.argv[1]); sr2, b = load(sys.argv[2])
f, pa = psd_db(sr1, a); _, pb = psd_db(sr2, b)
edges = [0, 50, 100, 150, 200, 300, 400, 600, 1000, 1500, 2000, 2400, 2600, 2700, 2800, 2900, 3000, 3200, 3500, 4000, 5000, 6000, 8000, 10000, 15000, 20000, 24000]
print(f"{'band Hz':>14s} {'SDR++ dB':>9s} {'Shell dB':>9s} {'diff':>6s}")
for lo, hi in zip(edges[:-1], edges[1:]):
    m = (f >= lo) & (f < hi)
    if not m.any():
        continue
    ra = 10*np.log10(np.mean(10**(pa[m]/10))); rb = 10*np.log10(np.mean(10**(pb[m]/10)))
    print(f"{lo:6d}-{hi:6d} {ra:9.1f} {rb:9.1f} {rb-ra:6.1f}")
