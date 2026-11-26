#!/usr/bin/env python3
"""Phase-invariant A/B of two SSB demods of the SAME baseband.

Two correct SSB demodulators whose LOs start at different phases give audio
that differs by a constant phase rotation (sounds identical, plain waveform
correlation can be ~0). So compare analytic signals: |<A, B>| / (|A| |B|) is
invariant to that rotation. Alignment uses full cross-correlation of 10 ms
envelopes (either may start first), then a fine search on the analytic signal.
Also reports the residual frequency offset (slope of the phase difference),
per-window similarity, level/clip stats, PSD shape and AGC level tracking."""
import sys, wave
import numpy as np
from scipy.signal import welch, correlate, butter, sosfilt, hilbert

def load(p):
    with wave.open(p, 'rb') as w:
        raw = w.readframes(w.getnframes()); sr = w.getframerate(); ch = w.getnchannels()
    x = np.frombuffer(raw, '<i2').astype(np.float64) / 32768.0
    return sr, (x.reshape(-1, ch).mean(axis=1) if ch > 1 else x)

def db(v): return 10*np.log10(np.maximum(v, 1e-20))
sr, ref = load(sys.argv[1]); _, cand = load(sys.argv[2])
bp = butter(6, [250, 2500], btype='band', fs=sr, output='sos')
rb, cb = sosfilt(bp, ref), sosfilt(bp, cand)
step = sr // 100
def env(x):
    k = len(x) // step; return np.abs(x[:k*step]).reshape(k, step).mean(axis=1)
er, ec = env(rb), env(cb)
er = (er - er.mean())/er.std(); ec = (ec - ec.mean())/ec.std()
cc = correlate(ec, er, mode='full'); lags = np.arange(-len(er)+1, len(ec))
L = lags[np.argmax(cc)] * step
print(f"coarse lag {L/sr:+.3f} s (ShellSDR index = SDR++ index + lag), env corr {cc.max()/min(len(er),len(ec)):.3f}")

A, B = hilbert(rb), hilbert(cb)
def overlap(lag):
    i0 = max(0, -lag); j0 = max(0, lag); n = min(len(A)-i0, len(B)-j0)
    return A[i0:i0+n], B[j0:j0+n]
best, bv = L, -1
for d in range(-step*2, step*2):
    a, b = overlap(L + d)
    a, b = a[sr:sr*6], b[sr:sr*6]
    v = abs(np.vdot(b, a)) / (np.linalg.norm(a)*np.linalg.norm(b))
    if v > bv: bv, best = v, L + d
a, b = overlap(best)
print(f"fine lag {best} samples ({best/sr:+.4f} s); phase-invariant similarity on 5 s: {bv:.3f}")
n = len(a)
# Residual frequency offset: slope of unwrapped phase of a * conj(b), over windows.
w = sr // 4
ph = [np.angle(np.vdot(b[s:s+w], a[s:s+w])) for s in range(0, n-w, w)]
slope = np.polyfit(np.arange(len(ph))*0.25, np.unwrap(ph), 1)[0]
print(f"residual frequency offset SDR++ vs ShellSDR: {slope/(2*np.pi):+.3f} Hz")
sim = [abs(np.vdot(b[s:s+w], a[s:s+w]))/(np.linalg.norm(a[s:s+w])*np.linalg.norm(b[s:s+w])+1e-12) for s in range(0, n-w, w)]
q = np.percentile(sim, [10, 50, 90])
print(f"0.25 s similarity percentiles 10/50/90: {q[0]:.3f} {q[1]:.3f} {q[2]:.3f}  (1.000 = identical up to phase/gain)")

# Level/clip stats on the full-band aligned audio.
i0 = max(0, -best); j0 = max(0, best); m = min(len(ref)-i0, len(cand)-j0)
r, c = ref[i0:i0+m], cand[j0:j0+m]
def stats(name, x):
    ab = np.abs(x); pk = ab.max()
    print(f"{name:9s} rms {np.sqrt(np.mean(x**2)):.3f} peak {pk:.3f} rail {100*np.mean(ab>=0.999*pk):.2f}% crest {20*np.log10(pk/np.sqrt(np.mean(x**2))):.1f} dB")
stats("SDR++", r); stats("ShellSDR", c)
g = np.sqrt(np.mean(r**2)/np.mean(c**2))
f, pr = welch(r, fs=sr, nperseg=4096); _, pc = welch(c*g, fs=sr, nperseg=4096)
edges = [0, 100, 200, 300, 500, 1000, 1500, 2000, 2400, 2600, 2800, 3000, 3500, 4000, 6000, 12000, 24000]
print(f"\n{'band Hz':>12s} {'SDR++':>7s} {'Shell':>7s} {'diff':>6s}   (dB, ShellSDR rms-matched)")
for lo, hi in zip(edges[:-1], edges[1:]):
    mm = (f >= lo) & (f < hi)
    print(f"{lo:5d}-{hi:5d} {db(pr[mm].mean()):7.1f} {db(pc[mm].mean()):7.1f} {db(pc[mm].mean())-db(pr[mm].mean()):6.1f}")
# AGC tracking: 20 ms level ratio (in-band), percentiles.
fw = sr // 50
def fr(x):
    k = len(x)//fw; return np.sqrt(np.mean(x[:k*fw].reshape(k, fw)**2, axis=1))
rat = 20*np.log10((fr(np.real(b))*g + 1e-9)/(fr(np.real(a)) + 1e-9))
p = np.percentile(rat, [5, 25, 50, 75, 95])
print("\n20 ms in-band level ratio Shell/SDR++ (dB) 5/25/50/75/95: " + " ".join(f"{v:+.1f}" for v in p))
