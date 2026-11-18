//! YCG images (magic `YCG\0`), the engine's own picture format.
//!
//! Erewhon's Chinese patch stores its translated UI pictures as YCG under
//! `.png` names, so pictures are recognised by their bytes, not by name.
//!
//! ```text
//! u8  magic[4] = "YCG\0"
//! u32 width, height, bpp      // bpp 32 (BGRA) or 24 (BGR)
//! u32 method                  // 1 = zlib (the only one seen)
//! u32 strips
//! strip × strips: u32 first_row, last_row, unpacked_len, packed_len
//! packed strip data, in order: rows top to bottom
//! ```

use std::io::Read;

/// Whether `bytes` start like a YCG picture.
#[must_use]
pub fn is_ycg(bytes: &[u8]) -> bool {
    bytes.starts_with(b"YCG\0")
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Width and height from the header.
#[must_use]
pub fn size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !is_ycg(bytes) {
        return None;
    }
    Some((u32_at(bytes, 4)?, u32_at(bytes, 8)?))
}

/// Decodes to `(width, height, RGBA8 pixels)`; `None` when malformed.
#[must_use]
pub fn decode_rgba(b: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if !is_ycg(b) {
        return None;
    }
    let (w, h, bpp, method, strips) =
        (u32_at(b, 4)?, u32_at(b, 8)?, u32_at(b, 12)?, u32_at(b, 16)?, u32_at(b, 20)?);
    if method != 1 || !(bpp == 32 || bpp == 24) || w == 0 || h == 0 || w > 16384 || h > 16384 {
        return None;
    }
    let src_px = (bpp / 8) as usize;
    let (wu, hu) = (w as usize, h as usize);
    let mut data = vec![0u8; wu * hu * 4];
    let mut pos = 24usize.checked_add((strips as usize).checked_mul(16)?)?;
    for i in 0..strips as usize {
        let t = 24 + i * 16;
        let (first, last, unpacked, packed) =
            (u32_at(b, t)?, u32_at(b, t + 4)?, u32_at(b, t + 8)?, u32_at(b, t + 12)?);
        let packed = b.get(pos..pos.checked_add(packed as usize)?)?;
        pos += packed.len();
        let mut out = Vec::with_capacity((unpacked as usize).min(wu * hu * 4));
        flate2::read::ZlibDecoder::new(packed).read_to_end(&mut out).ok()?;
        let row_len = wu * src_px;
        for (k, row) in out.chunks(row_len).enumerate() {
            let y = first as usize + k;
            if y > last as usize || y >= hu || row.len() < row_len {
                break;
            }
            for (x, px) in row.chunks(src_px).enumerate() {
                let o = (y * wu + x) * 4;
                let a = if src_px == 4 { px[3] } else { 255 };
                data[o..o + 4].copy_from_slice(&[px[2], px[1], px[0], a]);
            }
        }
    }
    Some((w, h, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn decodes_a_bgra_strip() {
        let rows: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8]; // 2x1 BGRA
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&rows).unwrap();
        let packed = z.finish().unwrap();
        let mut b = b"YCG\0".to_vec();
        for v in [2u32, 1, 32, 1, 1, 0, 0, 8, packed.len() as u32] {
            b.extend(v.to_le_bytes());
        }
        b.extend(&packed);
        assert_eq!(size(&b), Some((2, 1)));
        let (w, h, px) = decode_rgba(&b).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, [3, 2, 1, 4, 7, 6, 5, 8]);
    }
}
