#!/usr/bin/env python3
"""(1) Estimate a constant audio-frequency shift between two SSB recordings by
cross-correlating their fine PSD shapes (a tuning offset shifts the whole
audio spectrum). (2) Show how much the dynamics metric varies within each file
(content/fading variance), so differences can be judged against it."""
import sys, wave
import numpy as np
from scipy.signal import welch, butter, sosfilt

def load(p):
    with wave.open(p, 'rb') as w:
        raw = w.readframes(w.getnframes()); sr = w.getframerate(); ch = w.getnchannels()
    x = np.frombuffer(raw, '<i2').astype(np.float64) / 32768.0
    return sr, (x.reshape(-1, ch).mean(axis=1) if ch > 1 else x)

def shape(sr, x):
    f, p = welch(x, fs=sr, nperseg=48000)  # 1 Hz bins
    m = (f >= 150) & (f <= 2400)
    s = np.log10(p[m] + 1e-20); s -= s.mean()
    return f[m], s

def dyn(sr, x):
    bp = sosfilt(butter(4, [300, 2400], btype='band', fs=sr, output='sos'), x)
    n = int(sr * 0.02); k = len(bp) // n
    fr = np.sqrt(np.mean(bp[:k*n].reshape(k, n)**2, axis=1))
    p10, p50, p90 = np.percentile(fr, [10, 50, 90])
    return 20*np.log10(p90/p10), 20*np.log10(p50/p10)

sr, a = load(sys.argv[1]); _, b = load(sys.argv[2])
fa, sa = shape(sr, a); fb, sb = shape(sr, b)
best = max(range(-150, 151), key=lambda d: np.dot(sa[150:-150], np.roll(sb, -d)[150:-150]))
c0 = np.corrcoef(sa[150:-150], sb[150:-150])[0, 1]
cb = np.corrcoef(sa[150:-150], np.roll(sb, -best)[150:-150])[0, 1]
print(f"PSD-shape correlation: unshifted {c0:.3f}, best {cb:.3f} at ShellSDR shift {best:+d} Hz")

for name, x in (('SDR++', a), ('ShellSDR', b)):
    thirds = np.array_split(x, 3)
    vals = [dyn(sr, t) for t in thirds]
    print(f"{name:9s} p90/p10 per third: " + ", ".join(f"{v[0]:.1f}" for v in vals)
          + "   p50/p10: " + ", ".join(f"{v[1]:.1f}" for v in vals))
