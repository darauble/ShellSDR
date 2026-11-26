//! Real pictures on terminals with the Kitty graphics protocol (spec 15.6):
//! Kitty, WezTerm and Ghostty. The picture is sent as a PNG (`f=100`),
//! base64 in chunks, and placed over the cells of the view, scaled to them
//! (`c`, `r`). It is drawn outside ratatui: after a frame, at the view's
//! position; the cells under it stay blank.

/// The image id ShellSDR uses (one picture at a time).
pub const IMAGE_ID: u32 = 4242;
/// Base64 bytes per escape sequence (the protocol's limit is 4096).
const CHUNK: usize = 4096;

/// Whether the terminal speaks the Kitty graphics protocol, judged from the
/// environment (not inside tmux/screen, which do not pass it on).
pub fn supported_by_env(get: impl Fn(&str) -> Option<String>) -> bool {
    if get("TMUX").is_some() || get("STY").is_some() {
        return false;
    }
    let term = get("TERM").unwrap_or_default();
    let program = get("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
    get("KITTY_WINDOW_ID").is_some()
        || term == "xterm-kitty"
        || term == "xterm-ghostty"
        || program == "wezterm"
        || program == "ghostty"
}

/// [`supported_by_env`] for this process.
pub fn supported() -> bool {
    supported_by_env(|k| std::env::var(k).ok())
}

/// The escapes that show `png` at cell (`col`, `row`) (0-based), scaled to
/// `cols` x `rows` cells, replacing the previous picture.
pub fn place(png: &[u8], col: u16, row: u16, cols: u16, rows: u16) -> Vec<u8> {
    let data = base64(png);
    let mut out = Vec::with_capacity(data.len() + data.len() / CHUNK * 8 + 64);
    // save the cursor, move to the cell, send, restore
    out.extend_from_slice(format!("\x1b7\x1b[{};{}H", row + 1, col + 1).as_bytes());
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(CHUNK).collect();
    let n = chunks.len().max(1);
    for i in 0..n {
        let more = u8::from(i + 1 < n);
        if i == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=T,f=100,i={IMAGE_ID},p=1,q=2,C=1,c={cols},r={rows},m={more};")
                    .as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        out.extend_from_slice(chunks.get(i).copied().unwrap_or_default());
        out.extend_from_slice(b"\x1b\\");
    }
    out.extend_from_slice(b"\x1b8");
    out
}

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= c.len() {
                out.push(T[(n >> shift & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The escape that removes the picture.
pub fn remove() -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn detection_follows_the_terminal_and_not_multiplexers() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(supported_by_env(env(&[("TERM", "xterm-kitty")])));
        assert!(supported_by_env(env(&[("KITTY_WINDOW_ID", "1")])));
        assert!(supported_by_env(env(&[("TERM_PROGRAM", "WezTerm")])));
        assert!(supported_by_env(env(&[("TERM_PROGRAM", "ghostty")])));
        assert!(!supported_by_env(env(&[("TERM", "xterm-256color")])));
        assert!(!supported_by_env(env(&[
            ("TERM", "xterm-kitty"),
            ("TMUX", "/tmp/x")
        ])));
    }

    #[test]
    fn a_large_picture_goes_in_chunks_with_the_placement_first() {
        let png = vec![7u8; 10_000];
        let s = String::from_utf8(place(&png, 2, 3, 40, 20)).unwrap();
        assert!(s.starts_with("\x1b7\x1b[4;3H\x1b_Ga=T,f=100,i=4242,p=1,q=2,C=1,c=40,r=20,m=1;"));
        assert_eq!(
            s.matches("\x1b_G").count(),
            4,
            "13336 base64 bytes in 4 chunks"
        );
        assert!(s.contains("\x1b_Gm=0;"), "the last chunk ends it");
        assert!(s.ends_with("\x1b\\\x1b8"));
        let small = String::from_utf8(place(&[1, 2, 3], 0, 0, 1, 1)).unwrap();
        assert!(small.contains(",m=0;AQID\x1b\\"));
    }
}
