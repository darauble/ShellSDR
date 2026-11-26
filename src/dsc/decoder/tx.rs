//! A small DSC transmitter (M.493-16 §1–§3), for tests and fake devices:
//! the on-air bits of a call and their FSK audio. It is independent of the
//! receiver: it only uses the codeword table.

use crate::dsc::domain::codeword;
use crate::dsc::message::Symbol;

/// The characters of one call on air, in transmission order: phasing, the
/// call (format specifier … ECC) in DX and RX five characters apart, and
/// the closing EOS repetitions (§3.2, §9, Fig. A1-1). No dot pattern.
pub fn characters(call: &[Symbol]) -> Vec<Symbol> {
    let eos = call.len().checked_sub(2).map_or(127, |i| call[i]);
    let mut dx: Vec<Symbol> = vec![125; 6];
    dx.extend_from_slice(call);
    dx.extend_from_slice(&[eos, eos]);
    let mut rx: Vec<Symbol> = (104..=111).rev().collect();
    rx.extend_from_slice(call);
    dx.iter().zip(&rx).flat_map(|(&d, &r)| [d, r]).collect()
}

/// An expansion sequence (M.821) sent right after a call: the same DX/RX
/// structure without dot pattern or phasing. Its DX copies start at once;
/// the RX copies follow five characters later.
pub fn expansion_characters(seq: &[Symbol]) -> Vec<Symbol> {
    let eos = seq.len().checked_sub(2).map_or(127, |i| seq[i]);
    let mut dx: Vec<Symbol> = seq.to_vec();
    dx.extend_from_slice(&[eos, eos]);
    let mut rx: Vec<Symbol> = vec![126, 126];
    rx.extend_from_slice(seq);
    dx.iter().zip(&rx).flat_map(|(&d, &r)| [d, r]).collect()
}

/// The bits of characters (bit 1 = Y = the lower tone), first sent first.
pub fn bits(chars: &[Symbol]) -> Vec<u8> {
    chars
        .iter()
        .flat_map(|&c| {
            let code = codeword::encode(c);
            (0..10).map(move |i| (code >> i & 1) as u8)
        })
        .collect()
}

/// A dot pattern of `n` bits, as received: 0, 1, 0, 1, …
pub fn dots(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 2) as u8).collect()
}

/// The whole transmission of a call: dot pattern of `dot_bits`, phasing,
/// call, closing.
pub fn call_bits(call: &[Symbol], dot_bits: usize) -> Vec<u8> {
    let mut b = dots(dot_bits);
    b.extend(bits(&characters(call)));
    b
}

/// FSK audio of bits at 100 Bd: bit 1 at centre − 85 Hz, bit 0 at
/// centre + 85 Hz, phase-continuous, amplitude `amp`.
pub fn fsk(bits: &[u8], rate: f64, centre: f64, amp: f64) -> Vec<i16> {
    let per_bit = rate / 100.0;
    let total = (bits.len() as f64 * per_bit).round() as usize;
    let mut out = Vec::with_capacity(total);
    let mut phase = 0.0f64;
    for n in 0..total {
        let b = bits[((n as f64 / per_bit) as usize).min(bits.len() - 1)];
        let f = if b == 1 { centre - 85.0 } else { centre + 85.0 };
        out.push((amp * phase.sin()).round() as i16);
        phase = (phase + 2.0 * std::f64::consts::PI * f / rate) % (2.0 * std::f64::consts::PI);
    }
    out
}

/// Complex baseband FSK of `bits` at `rate`, centred `offset` Hz from DC
/// (ShellSDR: a DSC signal in a VFO's IQ): bit 1 (Y) is the lower tone,
/// offset - 85 Hz; bit 0 (B) the higher, offset + 85 Hz. Phase-continuous.
pub fn fsk_iq(bits: &[u8], rate: f64, offset: f64, amp: f32) -> Vec<num_complex::Complex<f32>> {
    let per_bit = rate / 100.0;
    let mut out = Vec::with_capacity((bits.len() as f64 * per_bit) as usize + 1);
    let mut phase = 0.0f64;
    let mut emitted = 0usize;
    for (k, &b) in bits.iter().enumerate() {
        let f = if b == 1 { offset - 85.0 } else { offset + 85.0 };
        let end = ((k + 1) as f64 * per_bit).round() as usize;
        while emitted < end {
            out.push(num_complex::Complex::from_polar(amp, phase as f32));
            phase = (phase + std::f64::consts::TAU * f / rate) % std::f64::consts::TAU;
            emitted += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rx_copy_follows_its_dx_copy_five_characters_later() {
        let c = characters(&[120, 120, 1, 2, 117, 5]);
        // DX RX7 DX RX6 … DX RX2 | A RX1 B RX0 C A D B …
        assert_eq!(
            &c[..12],
            &[125, 111, 125, 110, 125, 109, 125, 108, 125, 107, 125, 106]
        );
        assert_eq!(&c[12..20], &[120, 105, 120, 104, 1, 120, 2, 120]);
        // closing: DX EOS ECC EOS EOS, RX … EOS ECC
        assert_eq!(&c[c.len() - 8..], &[117, 1, 5, 2, 117, 117, 117, 5]);
    }

    #[test]
    fn bits_send_the_codeword_first_bit_first() {
        assert_eq!(bits(&[125]), vec![1, 0, 1, 1, 1, 1, 1, 0, 0, 1]); // 637
    }
}
