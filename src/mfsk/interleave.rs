//! The MFSK (de)interleaver (fldigi `mfsk/interleave.cxx`): `depth` square
//! diagonal interleavers of `size` x `size` in a row.
//!
//! Ported from fldigi 4.1.23 (GPL v3 or later; Dave Freese W1HKJ, from gMFSK
//! by Tomi Manninen OH2BNS).

/// A soft symbol that carries no information.
pub const PUNCTURE: u8 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Transmit.
    Forward,
    /// Receive.
    Reverse,
}

#[derive(Debug, Clone)]
pub struct Interleave {
    size: usize,
    depth: usize,
    dir: Direction,
    table: Vec<u8>,
}

impl Interleave {
    pub fn new(size: usize, depth: usize, dir: Direction) -> Self {
        let fill = if dir == Direction::Reverse {
            PUNCTURE
        } else {
            0
        };
        Self {
            size,
            depth,
            dir,
            table: vec![fill; size * size * depth],
        }
    }

    fn at(&self, k: usize, i: usize, j: usize) -> usize {
        self.size * self.size * k + self.size * i + j
    }

    /// One symbol's `size` soft bits through all stages, in place.
    pub fn symbols(&mut self, syms: &mut [u8]) {
        let n = self.size;
        for k in 0..self.depth {
            for (i, &s) in syms.iter().enumerate().take(n) {
                let row = self.at(k, i, 0);
                self.table.copy_within(row + 1..row + n, row);
                self.table[row + n - 1] = s;
            }
            for (i, s) in syms.iter_mut().enumerate().take(n) {
                let j = match self.dir {
                    Direction::Forward => n - i - 1,
                    Direction::Reverse => i,
                };
                *s = self.table[self.at(k, i, j)];
            }
        }
    }

    /// The bits of `word` (MSB first, `size` of them) through the stages.
    pub fn bits(&mut self, word: u32) -> u32 {
        let n = self.size;
        let mut syms: Vec<u8> = (0..n).map(|i| ((word >> (n - i - 1)) & 1) as u8).collect();
        self.symbols(&mut syms);
        syms.iter().fold(0, |w, &b| (w << 1) | u32::from(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reverse_undoes_the_forward_after_its_delay() {
        for (size, depth) in [(4, 10), (5, 5), (3, 10), (4, 20)] {
            let mut tx = Interleave::new(size, depth, Direction::Forward);
            let mut rx = Interleave::new(size, depth, Direction::Reverse);
            let words: Vec<u32> = (0..300u32)
                .map(|i| (i.wrapping_mul(2654435761u32) >> 7) & ((1 << size) - 1))
                .collect();
            let mut got = Vec::new();
            for &w in &words {
                let t = tx.bits(w);
                let mut syms: Vec<u8> = (0..size)
                    .map(|i| {
                        if (t >> (size - i - 1)) & 1 == 1 {
                            255
                        } else {
                            0
                        }
                    })
                    .collect();
                rx.symbols(&mut syms);
                got.push(syms);
            }
            // a bit comes out size x (size - 1) x depth symbols... find the lag
            let lag = (0..200)
                .find(|&lag| {
                    (0..words.len() - lag).all(|n| {
                        (0..size).all(|i| {
                            let b = (words[n] >> (size - i - 1)) & 1 == 1;
                            got[n + lag][i] == if b { 255 } else { 0 }
                        })
                    })
                })
                .expect("some lag restores every bit");
            assert_eq!(lag, (size - 1) * depth, "size {size} depth {depth}");
        }
    }
}
