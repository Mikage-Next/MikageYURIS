//! Shift-JIS tunnelling, as translation patches made with VNTextPatch do it.
//!
//! Characters CP932 cannot encode are listed in `sjis_ext.bin` (UTF-16LE)
//! and written into the scripts as two-byte codes that CP932 leaves unused:
//! a lead byte from 0x81 on and a trail byte below 0x40. Entry `i` is
//! `lead = 0x81 + i / 58` (0xE0 follows 0x9F) and `trail = TRAILS[i % 58]`,
//! where the trails are 0x01..=0x3F without tab, line feed, carriage return,
//! space and comma. The patch's proxy DLL decodes these codes when the game
//! draws or measures text.

/// The trail bytes of tunnelled codes, in order.
const TRAILS: [u8; 58] = {
    let mut t = [0u8; 58];
    let (mut b, mut i) = (1u8, 0usize);
    while b < 0x40 {
        if !matches!(b, 0x09 | 0x0a | 0x0d | 0x20 | 0x2c) {
            t[i] = b;
            i += 1;
        }
        b += 1;
    }
    t
};

/// Reads a `sjis_ext.bin` table (UTF-16LE characters).
pub fn parse_table(bytes: &[u8]) -> Vec<char> {
    let units = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]));
    char::decode_utf16(units).filter_map(|c| c.ok()).collect()
}

/// The table index of a tunnelled code, if `lead`/`trail` is one.
pub fn index(lead: u8, trail: u8) -> Option<usize> {
    let row = match lead {
        0x81..=0x9f => lead - 0x81,
        0xe0..=0xfc => lead - 0xe0 + 0x1f,
        _ => return None,
    } as usize;
    let col = TRAILS.iter().position(|&t| t == trail)?;
    Some(row * TRAILS.len() + col)
}

/// Decodes `bytes` with `encoding`, replacing tunnelled codes by their
/// table characters.
pub fn decode(bytes: &[u8], table: &[char], encoding: &'static encoding_rs::Encoding) -> String {
    let mut out = String::new();
    let (mut start, mut i) = (0usize, 0usize);
    while i < bytes.len() {
        let b = bytes[i];
        if !matches!(b, 0x81..=0x9f | 0xe0..=0xfc) {
            i += 1;
            continue;
        }
        let ch = bytes.get(i + 1).and_then(|&t| index(b, t)).and_then(|k| table.get(k));
        if let Some(&c) = ch {
            out.push_str(&encoding.decode(&bytes[start..i]).0);
            out.push(c);
            start = i + 2;
        }
        i += 2;
    }
    out.push_str(&encoding.decode(&bytes[start..]).0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnelled_codes_decode_between_shift_jis() {
        let table: Vec<char> = "们说这".chars().collect();
        assert_eq!(index(0x81, 0x01), Some(0));
        assert_eq!(index(0x81, 0x09), None);
        assert_eq!(index(0x82, 0x01), Some(58));
        // 我 (89 E4) + entry 0 + ア (83 41) + entry 2
        let s = decode(&[0x89, 0xe4, 0x81, 0x01, 0x83, 0x41, 0x81, 0x03], &table, encoding_rs::SHIFT_JIS);
        assert_eq!(s, "我们ア这");
    }
}
