use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use std::fmt;

/// JSON represents every arbitrary 64-bit integer as a decimal string.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct U64(pub u64);

impl Serialize for U64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for U64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = U64;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a canonical unsigned 64-bit decimal string")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<U64, E> {
                if value.is_empty()
                    || value.len() > 20
                    || (value.len() > 1 && value.starts_with('0'))
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(E::custom("invalid unsigned decimal string"));
                }
                value.parse().map(U64).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    to_hex(&Sha256::digest(bytes))
}

pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexError {
    pub offset: usize,
    pub message: &'static str,
}

impl fmt::Display for HexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at text byte {}", self.message, self.offset)
    }
}

impl std::error::Error for HexError {}

pub fn from_hex(text: &str, max_bytes: usize) -> Result<Vec<u8>, HexError> {
    let text = text.trim();
    let (text, prefix) = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .map_or((text, 0), |text| (text, 2));
    let mut output = Vec::with_capacity((text.len() / 2).min(max_bytes));
    let mut high = None;
    for (offset, byte) in text.bytes().enumerate() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => {
                return Err(HexError {
                    offset: offset + prefix,
                    message: "invalid hex digit",
                });
            }
        };
        if let Some(first) = high.take() {
            if output.len() == max_bytes {
                return Err(HexError {
                    offset: offset + prefix,
                    message: "input byte limit exceeded",
                });
            }
            output.push(first << 4 | digit);
        } else {
            high = Some(digit);
        }
    }
    if high.is_some() {
        return Err(HexError {
            offset: text.len() + prefix,
            message: "unpaired hex digit",
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_never_cross_json_as_numbers() {
        let encoded = serde_json::to_string(&U64(u64::MAX)).unwrap();
        assert_eq!(encoded, "\"18446744073709551615\"");
        assert_eq!(
            serde_json::from_str::<U64>(&encoded).unwrap(),
            U64(u64::MAX)
        );
        for value in [
            "18446744073709551615",
            "\"18446744073709551616\"",
            "\"01\"",
            "\"+1\"",
            "\"\"",
        ] {
            assert!(serde_json::from_str::<U64>(value).is_err(), "{value}");
        }
    }

    #[test]
    fn hex_input_is_strict_and_bounded() {
        assert_eq!(from_hex(" 0x08 96\n01 ", 3).unwrap(), [8, 150, 1]);
        assert_eq!(
            from_hex("00ff", 1).unwrap_err().message,
            "input byte limit exceeded"
        );
        assert!(from_hex("0:1", 4).is_err());
        assert!(from_hex("123", 4).is_err());
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
