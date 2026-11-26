//! The 10-bit error-detecting codeword (M.493-16 §1.1, Table A1-1).
//!
//! Bit 0 is the first bit transmitted. Bits 0..6 are the symbol, least
//! significant bit first; bits 7..9 the number of 0 bits among them, most
//! significant bit first. Bit value 1 is the Y state (the lower tone).

/// The codeword of a symbol 0..127.
pub fn encode(symbol: u8) -> u16 {
    let value = u16::from(symbol & 0x7f);
    let zeros = 7 - value.count_ones() as u16;
    // The count goes out most significant bit first: its bit 2 is bit 7.
    value | (zeros >> 2 & 1) << 7 | (zeros >> 1 & 1) << 8 | (zeros & 1) << 9
}

/// The symbol value of a codeword (bits 0..6), valid or not.
pub fn symbol_of(code: u16) -> u8 {
    (code & 0x7f) as u8
}

/// Whether a received 10-bit code is a valid codeword.
pub fn is_valid(code: u16) -> bool {
    code < 1024 && encode(symbol_of(code)) == code
}

#[cfg(test)]
mod tests {
    use super::*;

    // conformance/codec/codewords.json "special"; 02-receiver.md step 1.
    #[test]
    fn dx_phasing_125_is_637_and_valid() {
        assert_eq!(encode(125), 637);
        assert!(is_valid(637));
        assert_eq!(symbol_of(637), 125);
    }

    #[test]
    fn rx_phasing_codes_are_valid_symbols_104_to_111() {
        let rx = [232, 873, 874, 363, 876, 365, 366, 623];
        for (i, code) in rx.into_iter().enumerate() {
            assert!(is_valid(code), "{code}");
            assert_eq!(symbol_of(code), 104 + i as u8);
        }
    }

    #[test]
    fn dot_pattern_682_is_not_a_valid_codeword() {
        assert!(!is_valid(682));
    }

    #[test]
    fn exactly_128_of_1024_codes_are_valid() {
        let valid: Vec<u16> = (0..1024).filter(|&c| is_valid(c)).collect();
        assert_eq!(valid.len(), 128);
        assert!(valid.iter().all(|&c| encode(symbol_of(c)) == c));
    }

    #[test]
    fn one_bit_error_is_detected() {
        for s in 0..128u8 {
            for bit in 0..10 {
                assert!(!is_valid(encode(s) ^ 1 << bit), "symbol {s} bit {bit}");
            }
        }
    }
}
