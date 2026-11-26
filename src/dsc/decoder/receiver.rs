//! The receiver: soft bits to one symbol sequence per call (seed
//! spec/04-algorithms/02-receiver.md).

use super::parser;
use crate::dsc::domain::codeword;
use crate::dsc::message::Symbol;

/// One call as the receiver outputs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// First format specifier through the last ECC; 255 = garbled.
    pub symbols: Vec<Symbol>,
    /// The sample number at which the phasing was found.
    pub start_sample: u64,
}

const WINDOW: usize = 130;
const DOT_CODE: u16 = 682;
const MAX_CHARS: u32 = 70;
const GARBLED: Symbol = 255;

/// After a call is done: waiting for a possible expansion sequence.
#[derive(Debug, Clone, Copy)]
struct LookAhead {
    call_end: usize,
    left: u8,
    started: bool,
}

/// What reading one character asks the receiver to do next.
enum Next {
    Go(i32),
    Output(Vec<Symbol>),
}

pub struct Receiver {
    window: [u8; WINDOW],
    reading: bool,
    /// Index in the window where the next character begins.
    cursor: i32,
    count: u32,
    errors: i32,
    symbols: Vec<Symbol>,
    start_sample: u64,
    look: Option<LookAhead>,
    dx_code: u16,
    rx_codes: [u16; 8],
}

impl Default for Receiver {
    fn default() -> Self {
        Receiver::new()
    }
}

impl Receiver {
    pub fn new() -> Receiver {
        let mut rx_codes = [0; 8];
        for (k, c) in rx_codes.iter_mut().enumerate() {
            *c = codeword::encode(104 + k as u8);
        }
        Receiver {
            window: [0; WINDOW],
            reading: false,
            cursor: 0,
            count: 0,
            errors: 0,
            symbols: Vec::new(),
            start_sample: 0,
            look: None,
            dx_code: codeword::encode(125),
            rx_codes,
        }
    }

    /// Whether a call is being read (the DECODING state of a channel).
    pub fn is_reading(&self) -> bool {
        self.reading
    }

    /// One soft bit (> 0 is bit 1) decided at `sample`.
    pub fn push(&mut self, soft: i64, sample: u64) -> Option<Call> {
        self.window.copy_within(1.., 0);
        self.window[WINDOW - 1] = u8::from(soft > 0);
        if !self.reading {
            if let Some(c) = self.find_phasing() {
                self.start(c, sample);
            }
            return None;
        }
        self.cursor -= 1;
        // Step 6: a new phasing while reading restarts (F-26).
        if self.look.is_none()
            && self.count >= 14
            && let Some(c) = self.find_phasing()
        {
            self.start(c, sample);
            return None;
        }
        while self.reading && self.cursor + 10 <= WINDOW as i32 {
            let result = match self.read_character() {
                Next::Go(r) => r,
                Next::Output(symbols) => {
                    let call = Call {
                        symbols: output(symbols),
                        start_sample: self.start_sample,
                    };
                    self.reset();
                    return Some(call);
                }
            };
            self.cursor += 10;
            self.count += 1;
            self.errors = (self.errors - result).max(0);
            if self.errors > 20 {
                // Signal lost: a call waiting for its expansion is output.
                let done = self.look.map(|l| self.symbols[..l.call_end].to_vec());
                self.reset();
                return done.map(|s| Call {
                    symbols: output(s),
                    start_sample: self.start_sample,
                });
            }
        }
        None
    }

    /// End of input: a call waiting for its expansion is output.
    pub fn flush(&mut self) -> Option<Call> {
        let done = self.look.map(|l| self.symbols[..l.call_end].to_vec());
        let start_sample = self.start_sample;
        self.reset();
        done.map(|s| Call {
            symbols: output(s),
            start_sample,
        })
    }

    fn start(&mut self, cursor: i32, sample: u64) {
        self.reading = true;
        self.cursor = cursor;
        self.count = 0;
        self.errors = 0;
        self.symbols.clear();
        self.start_sample = sample;
        self.look = None;
    }

    fn reset(&mut self) {
        self.reading = false;
        self.symbols.clear();
        self.look = None;
    }

    fn code_at(&self, i: i32) -> u16 {
        let i = i as usize;
        (0..10).fold(0, |c, k| c | u16::from(self.window[i + k]) << k)
    }

    /// Step 3: the cursor of the first format specifier, if phasing ends
    /// near the newest end of the window.
    fn find_phasing(&self) -> Option<i32> {
        for offset in 100..=119 {
            let mut run = 0;
            let mut i = offset;
            while i < 120 {
                if self.code_at(i) == DOT_CODE {
                    return None;
                }
                for ph in 0..6 {
                    for (pos, expected) in [
                        (i - 20 * ph, self.rx_codes[ph as usize + 2]),
                        (i - 20 * ph - 10, self.dx_code),
                    ] {
                        if pos >= 0 && self.code_at(pos) == expected {
                            run += 1;
                        } else if run < 3 {
                            run = 0;
                        }
                    }
                }
                i += 10;
            }
            if run >= 3 {
                return Some(i);
            }
        }
        None
    }

    /// Step 4: the character at the cursor.
    fn read_character(&mut self) -> Next {
        let code = self.code_at(self.cursor);
        let ok = |b: bool| Next::Go(if b { 1 } else { -1 });
        match self.count {
            1 => return ok(codeword::symbol_of(code) == 105),
            3 => return ok(codeword::symbol_of(code) == 104),
            c if c % 2 == 0 => return Next::Go(0),
            _ => {}
        }
        let symbol = choose(code, self.code_at(self.cursor - 50));
        let result = if symbol == GARBLED { -1 } else { 1 };
        match self.look {
            None => {
                self.symbols.push(symbol);
                if self.eos_found() {
                    self.look = Some(LookAhead {
                        call_end: self.symbols.len(),
                        left: 3,
                        started: false,
                    });
                } else if self.count > MAX_CHARS {
                    // No end found: give up, output the cut call (F-27).
                    return Next::Output(std::mem::take(&mut self.symbols));
                }
            }
            Some(mut l) => {
                // Step 7: the expansion look-ahead.
                let call_only = self.symbols[..l.call_end].to_vec();
                if !l.started {
                    if (100..=106).contains(&symbol) {
                        l.started = true;
                        self.symbols.push(symbol);
                    } else if [126, 117, 122, 127].contains(&symbol) {
                        // Fillers stay between the call and its expansion.
                        self.symbols.push(symbol);
                        l.left -= 1;
                        if l.left == 0 {
                            return Next::Output(call_only);
                        }
                    } else {
                        return Next::Output(call_only);
                    }
                } else {
                    self.symbols.push(symbol);
                    if self.eos_found() {
                        return Next::Output(std::mem::take(&mut self.symbols));
                    }
                    if self.symbols.len() > l.call_end + 40 {
                        return Next::Output(call_only);
                    }
                }
                self.look = Some(l);
            }
        }
        Next::Go(result)
    }

    /// Step 5: three of the four closing EOS copies behind the cursor.
    fn eos_found(&self) -> bool {
        self.count >= 39
            && [1, 2, 3, 7]
                .iter()
                .filter(|&&back| {
                    [117, 122, 127]
                        .contains(&codeword::symbol_of(self.code_at(self.cursor - 10 * back)))
                })
                .count()
                >= 3
    }
}

/// The normative choice between the RX and the DX copy (F-28).
fn choose(rx: u16, dx: u16) -> Symbol {
    if codeword::is_valid(rx) {
        codeword::symbol_of(rx)
    } else if codeword::is_valid(dx) {
        codeword::symbol_of(dx)
    } else {
        GARBLED
    }
}

/// Step 7 output: through the last ECC, or all of a cut call.
fn output(symbols: Vec<Symbol>) -> Vec<Symbol> {
    let n = parser::parse(&symbols).length;
    if n == 0 {
        symbols
    } else {
        symbols[..n.min(symbols.len())].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsc::decoder::tx;

    /// The real recording's call (conformance audio, `real-recording`).
    const INDIVIDUAL: [u8; 23] = [
        120, 120, 0, 27, 11, 0, 10, 108, 35, 20, 3, 67, 50, 118, 126, 126, 126, 126, 126, 126, 126,
        117, 54,
    ];
    /// A distress alert (format 112 twice … EOS 127, ECC).
    fn distress() -> Vec<u8> {
        let mut c = vec![
            112, 112, 21, 13, 5, 11, 0, 101, 10, 0, 60, 2, 0, 12, 34, 110, 127,
        ];
        let ecc = c[1..].iter().fold(0, |a, &s| a ^ s);
        c.push(ecc);
        c
    }

    /// Every call the receiver outputs for these bits, then flushes.
    fn receive(bits: &[u8]) -> Vec<Call> {
        let mut r = Receiver::new();
        let mut out: Vec<Call> = bits
            .iter()
            .enumerate()
            .filter_map(|(i, &b)| r.push(if b == 1 { 5 } else { -5 }, i as u64 * 10))
            .collect();
        out.extend(r.flush());
        out
    }

    fn symbols(calls: &[Call]) -> Vec<Vec<u8>> {
        calls.iter().map(|c| c.symbols.clone()).collect()
    }

    /// xorshift64: deterministic noise bits.
    fn noise(n: usize, mut seed: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed & 1) as u8
            })
            .collect()
    }

    #[test]
    fn a_clean_call_gives_its_symbols_through_the_ecc() {
        for dots in [200, 20] {
            let mut b = noise(37, 1);
            b.extend(tx::call_bits(&INDIVIDUAL, dots));
            b.extend(noise(300, 2));
            assert_eq!(
                symbols(&receive(&b)),
                vec![INDIVIDUAL.to_vec()],
                "dots {dots}"
            );
        }
    }

    #[test]
    fn the_start_sample_is_where_the_phasing_was_found() {
        let mut b = tx::dots(200);
        let phasing_end = b.len() + 120; // 12 phasing characters
        b.extend(tx::bits(&tx::characters(&INDIVIDUAL))[..].iter());
        b.extend(noise(300, 3));
        let c = receive(&b);
        let found_bit = c[0].start_sample / 10;
        assert!(
            (phasing_end as u64..phasing_end as u64 + 10).contains(&found_bit),
            "{found_bit} vs {phasing_end}"
        );
    }

    #[test]
    fn a_garbled_copy_is_replaced_by_the_other_and_two_give_255() {
        let chars = tx::characters(&INDIVIDUAL);
        // Symbol 3 (11): DX copy at character 12 + 2*3, RX copy 5 later.
        let dx3 = 12 + 2 * 3;
        let rx3 = dx3 + 5;
        let rx5 = 12 + 2 * 5 + 5;
        let dx5 = 12 + 2 * 5;
        let mut b = tx::dots(20);
        let mut bits = tx::bits(&chars);
        bits[dx3 * 10] ^= 1;
        bits[rx5 * 10 + 3] ^= 1;
        bits[dx5 * 10 + 4] ^= 1;
        b.extend(bits);
        b.extend(noise(300, 4));
        let mut want = INDIVIDUAL.to_vec();
        want[5] = 255;
        assert_eq!(symbols(&receive(&b)), vec![want]);
        let _ = rx3;
    }

    #[test]
    fn two_calls_back_to_back_give_two() {
        let mut b = tx::call_bits(&distress(), 200);
        b.extend(tx::call_bits(&distress(), 20));
        b.extend(noise(300, 5));
        assert_eq!(symbols(&receive(&b)), vec![distress(), distress()]);
    }

    #[test]
    fn a_call_cut_off_by_a_new_phasing_is_dropped() {
        let first = tx::call_bits(&INDIVIDUAL, 200);
        let mut b = first[..200 + 120 + 12 * 10].to_vec(); // six slots (DX + RX) after phasing
        b.extend(tx::call_bits(&distress(), 200));
        b.extend(noise(300, 6));
        assert_eq!(symbols(&receive(&b)), vec![distress()]);
    }

    #[test]
    fn noise_gives_nothing() {
        assert_eq!(receive(&noise(100_000, 7)), vec![]);
    }

    #[test]
    fn end_of_input_right_after_the_call_still_outputs_it() {
        assert_eq!(
            symbols(&receive(&tx::call_bits(&INDIVIDUAL, 20))),
            vec![INDIVIDUAL.to_vec()]
        );
    }

    #[test]
    fn an_expansion_sequence_is_joined_to_its_call() {
        // M.821 enhanced position: 100, 4 digit pairs, EOS 127, ECC.
        let mut exp = vec![100, 12, 34, 56, 78, 127];
        let ecc = exp.iter().fold(0, |a, &s| a ^ s);
        exp.push(ecc);
        let mut b = tx::call_bits(&distress(), 200);
        b.extend(tx::bits(&tx::expansion_characters(&exp)));
        b.extend(noise(300, 8));
        let mut want = distress();
        want.extend([126, 126]); // the RX fillers stay (audio synth-expansion-fillers)
        want.extend(&exp);
        assert_eq!(symbols(&receive(&b)), vec![want]);
    }
}
