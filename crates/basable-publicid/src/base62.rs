//! Base62 over big-endian bytes, byte-identical to the Go original: leading
//! zero BYTES are preserved as leading `0` characters so the encoding of a
//! fixed-width input is reversible; decoding is length-lenient and
//! left-pads to the requested width.

use crate::DecodeError;

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Encodes big-endian bytes as base62.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();

    // Base-256 → base-62 by repeated division over the big-endian digits.
    let mut digits = bytes.to_vec();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() * 2);
    while digits.iter().any(|&d| d != 0) {
        let mut rem: u32 = 0;
        for d in digits.iter_mut() {
            let acc = rem * 256 + u32::from(*d);
            *d = (acc / 62) as u8;
            rem = acc % 62;
        }
        out.push(ALPHABET[rem as usize]);
    }
    out.extend(std::iter::repeat_n(ALPHABET[0], zeros));
    out.reverse();
    if out.is_empty() {
        out.push(ALPHABET[0]);
    }
    String::from_utf8(out).expect("the alphabet is ASCII")
}

/// Decodes base62 into exactly `width` big-endian bytes, left-padded with
/// zeros. Extra leading `0` characters contribute nothing and alias to the
/// same value, so ids are not canonical on decode.
pub(crate) fn decode(s: &str, width: usize) -> Result<Vec<u8>, DecodeError> {
    if s.is_empty() {
        return Err(DecodeError::Malformed(String::new()));
    }
    // Little-endian base-256 accumulator; multiply by 62 and add per digit.
    let mut acc: Vec<u8> = Vec::with_capacity(width + 1);
    for c in s.chars() {
        let idx = ALPHABET
            .iter()
            .position(|&a| char::from(a) == c)
            .ok_or_else(|| DecodeError::InvalidCharacter {
                c,
                payload: s.to_owned(),
            })?;
        let mut carry: u32 = idx as u32;
        for b in acc.iter_mut() {
            let v = u32::from(*b) * 62 + carry;
            *b = (v & 0xff) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            acc.push((carry & 0xff) as u8);
            carry >>= 8;
        }
        if acc.len() > width {
            return Err(DecodeError::Overflow(s.to_owned()));
        }
    }
    let mut out = vec![0u8; width];
    for (i, b) in acc.iter().enumerate() {
        out[width - 1 - i] = *b;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_zero_bytes_become_leading_zero_characters() {
        assert_eq!(encode(&[0u8; 16]), "0000000000000000");
        assert_eq!(encode(&[0, 0, 0, 1]), "0001");
        assert_eq!(encode(&[]), "0");
        assert_eq!(encode(&[61]), "z");
        assert_eq!(encode(&[62]), "10");
    }

    #[test]
    fn decode_inverts_encode_for_every_width() {
        for bytes in [
            vec![0u8; 16],
            vec![255u8; 16],
            vec![0, 0, 0, 1],
            (0u8..16).collect(),
        ] {
            let s = encode(&bytes);
            assert_eq!(decode(&s, bytes.len()).unwrap(), bytes, "{s}");
        }
    }

    #[test]
    fn decode_is_lenient_about_length_and_strict_about_alphabet() {
        assert_eq!(decode("1", 2).unwrap(), vec![0, 1]);
        assert_eq!(decode("01", 2).unwrap(), vec![0, 1]);
        assert!(matches!(
            decode("1-", 2),
            Err(DecodeError::InvalidCharacter { c: '-', .. })
        ));
        assert!(matches!(decode("zzz", 1), Err(DecodeError::Overflow(_))));
        assert_eq!(decode("", 1), Err(DecodeError::Malformed(String::new())));
    }
}
