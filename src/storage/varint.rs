pub fn decode(input: &[u8]) -> Result<(u32, usize), DecodeError> {
    let mut value = 0;

    for (index, &byte) in input.iter().take(5).enumerate() {
        let data = (byte & 0x7F) as u32;

        if index == 4 && data > 0x0F {
            return Err(DecodeError::Overflow);
        }

        value |= data << (index * 7);

        if byte & 0x80 == 0 {
            if index > 0 && data == 0 {
                return Err(DecodeError::NonCanonical);
            }
            return Ok((value, index + 1));
        }
    }

    if input.len() < 5 {
        Err(DecodeError::Truncated)
    } else {
        Err(DecodeError::Overflow)
    }
}

pub fn encode(mut value: u32, output: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;

        if value != 0 {
            byte |= 0x80;
        }

        output.push(byte);

        if value == 0 {
            return;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    Overflow,
    NonCanonical,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_known_values() {
        let cases = [
            (0, vec![0x00]),
            (1, vec![0x01]),
            (127, vec![0x7F]),
            (128, vec![0x80, 0x01]),
            (300, vec![0xAC, 0x02]),
            (16_383, vec![0xFF, 0x7F]),
            (16_384, vec![0x80, 0x80, 0x01]),
            (u32::MAX, vec![0xFF, 0xFF, 0xFF, 0xFF, 0x0F]),
        ];

        for (value, expected) in cases {
            let mut output = Vec::new();
            encode(value, &mut output);
            assert_eq!(output, expected, "incorrect encoding for {value}");
        }
    }

    #[test]
    fn encode_appends_to_existing_output() {
        let mut output = vec![0xAA];

        encode(300, &mut output);

        assert_eq!(output, [0xAA, 0xAC, 0x02]);
    }

    #[test]
    fn decode_known_values() {
        let cases: &[(&[u8], u32, usize)] = &[
            (&[0x00], 0, 1),
            (&[0x01], 1, 1),
            (&[0x7F], 127, 1),
            (&[0x80, 0x01], 128, 2),
            (&[0xAC, 0x02], 300, 2),
            (&[0xFF, 0x7F], 16_383, 2),
            (&[0x80, 0x80, 0x01], 16_384, 3),
            (&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F], u32::MAX, 5),
        ];

        for (input, expected_value, expected_bytes) in cases {
            let result = decode(input);
            assert_eq!(result, Ok((*expected_value, *expected_bytes)));
        }
    }

    #[test]
    fn decode_stops_after_one_varint() {
        let input = [0xAC, 0x02, 0x01];

        assert_eq!(decode(&input), Ok((300, 2)));
    }

    #[test]
    fn decode_rejects_truncated_values() {
        assert_eq!(decode(&[]), Err(DecodeError::Truncated));
        assert_eq!(decode(&[0x80]), Err(DecodeError::Truncated));
        assert_eq!(
            decode(&[0xFF, 0xFF, 0xFF, 0xFF]),
            Err(DecodeError::Truncated)
        );
    }

    #[test]
    fn decode_rejects_overflow() {
        assert_eq!(
            decode(&[0xFF, 0xFF, 0xFF, 0xFF, 0x10]),
            Err(DecodeError::Overflow)
        );
        assert_eq!(
            decode(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00]),
            Err(DecodeError::Overflow)
        );
    }

    #[test]
    fn decode_rejects_non_canonical_values() {
        assert_eq!(decode(&[0x80, 0x00]), Err(DecodeError::NonCanonical));
        assert_eq!(decode(&[0x81, 0x00]), Err(DecodeError::NonCanonical));
    }
}
