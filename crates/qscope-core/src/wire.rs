use crate::data::U64;
use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_fields: usize,
    pub max_depth: usize,
    pub max_candidate_bytes: usize,
    pub max_candidate_fields: usize,
    pub max_text_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_fields: 100_000,
            max_depth: 32,
            max_candidate_bytes: 16 * 1024,
            max_candidate_fields: 10_000,
            max_text_bytes: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InputLimit,
    FieldLimit,
    DepthLimit,
    TruncatedVarint,
    VarintOverflow,
    InvalidFieldNumber,
    InvalidWireType,
    TruncatedFixed32,
    TruncatedFixed64,
    LengthOutOfBounds,
    UnexpectedEndGroup,
    MismatchedEndGroup,
    UnclosedGroup,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecodeError {
    pub code: ErrorCode,
    pub offset: usize,
    pub field_start: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub range: ByteRange,
    pub fields: Vec<Field>,
    pub complete: bool,
    pub error: Option<DecodeError>,
    /// Unparsed bytes always remain accessible in the unchanged input.
    pub undecoded: Option<ByteRange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub number: u32,
    pub wire_type: u8,
    pub raw: ByteRange,
    pub key: ByteRange,
    pub canonical_key: bool,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Value {
    Varint {
        range: ByteRange,
        unsigned: U64,
        canonical: bool,
    },
    Fixed64 {
        range: ByteRange,
        little_endian_bits: U64,
    },
    LengthDelimited {
        length_prefix: ByteRange,
        bytes: ByteRange,
        canonical_length: bool,
        candidates: Candidates,
    },
    Group {
        message: Box<Message>,
        end_key: ByteRange,
    },
    Fixed32 {
        range: ByteRange,
        little_endian_bits: u32,
    },
}

/// These are simultaneous hypotheses, never recovered schema declarations.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Candidates {
    pub utf8: Option<String>,
    pub utf8_preview_truncated: bool,
    pub message: Option<Box<Message>>,
    pub message_analysis_limited: bool,
}

struct Parser<'a> {
    input: &'a [u8],
    limits: Limits,
    fields_left: usize,
    candidates_left: usize,
}

struct ParsedMessage {
    message: Message,
    end_key: Option<ByteRange>,
}

pub fn decode(input: &[u8], limits: Limits) -> Message {
    // Even a misconfigured caller cannot turn an untrusted group chain into
    // unbounded native stack recursion.
    let limits = Limits {
        max_depth: limits.max_depth.min(64),
        ..limits
    };
    if input.len() > limits.max_input_bytes {
        return Message {
            range: ByteRange {
                start: 0,
                end: input.len(),
            },
            fields: vec![],
            complete: false,
            error: Some(DecodeError {
                code: ErrorCode::InputLimit,
                offset: 0,
                field_start: 0,
            }),
            undecoded: Some(ByteRange {
                start: 0,
                end: input.len(),
            }),
        };
    }
    Parser {
        input,
        limits,
        fields_left: limits.max_fields,
        candidates_left: limits.max_candidate_fields,
    }
    .message(0, input.len(), 0, None, false)
    .message
}

fn varint(input: &[u8], cursor: &mut usize, end: usize) -> Result<(u64, bool), ErrorCode> {
    let start = *cursor;
    let mut value = 0_u64;
    for index in 0..10 {
        if *cursor >= end {
            return Err(ErrorCode::TruncatedVarint);
        }
        let byte = input[*cursor];
        *cursor += 1;
        if index == 9 && byte > 1 {
            return Err(ErrorCode::VarintOverflow);
        }
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok((value, *cursor - start == 1 || byte != 0));
        }
    }
    Err(ErrorCode::VarintOverflow)
}

impl Parser<'_> {
    fn message(
        &mut self,
        start: usize,
        end: usize,
        depth: usize,
        group: Option<u32>,
        candidate: bool,
    ) -> ParsedMessage {
        let mut message = Message {
            range: ByteRange { start, end },
            fields: vec![],
            complete: true,
            error: None,
            undecoded: None,
        };
        let mut cursor = start;
        let mut failure = None;
        while cursor < end {
            let field_start = cursor;
            let result = self.field(&mut cursor, end, depth, group, candidate);
            match result {
                Ok(FieldResult::Field(field)) => message.fields.push(field),
                Ok(FieldResult::EndGroup(end_key)) => {
                    message.range.end = end_key.start;
                    return ParsedMessage {
                        message,
                        end_key: Some(end_key),
                    };
                }
                Err(code) => {
                    failure = Some(DecodeError {
                        code,
                        offset: cursor,
                        field_start,
                    });
                    break;
                }
            }
        }
        if failure.is_none() && group.is_some() {
            failure = Some(DecodeError {
                code: ErrorCode::UnclosedGroup,
                offset: cursor,
                field_start: start,
            });
        }
        if let Some(error) = failure {
            message.complete = false;
            message.undecoded = Some(ByteRange {
                start: error.field_start,
                end,
            });
            message.error = Some(error);
        }
        ParsedMessage {
            message,
            end_key: None,
        }
    }

    fn field(
        &mut self,
        cursor: &mut usize,
        end: usize,
        depth: usize,
        group: Option<u32>,
        candidate: bool,
    ) -> Result<FieldResult, ErrorCode> {
        let start = *cursor;
        let (tag, canonical_key) = varint(self.input, cursor, end)?;
        let number = tag >> 3;
        if number == 0 || number > 0x1fff_ffff {
            return Err(ErrorCode::InvalidFieldNumber);
        }
        let number = number as u32;
        let wire_type = (tag & 7) as u8;
        let key = ByteRange {
            start,
            end: *cursor,
        };
        if wire_type == 4 {
            return match group {
                None => Err(ErrorCode::UnexpectedEndGroup),
                Some(expected) if expected != number => Err(ErrorCode::MismatchedEndGroup),
                Some(_) => Ok(FieldResult::EndGroup(key)),
            };
        }
        let budget = if candidate {
            &mut self.candidates_left
        } else {
            &mut self.fields_left
        };
        if *budget == 0 {
            return Err(ErrorCode::FieldLimit);
        }
        *budget -= 1;
        let value_start = *cursor;
        let value = match wire_type {
            0 => {
                let (unsigned, canonical) = varint(self.input, cursor, end)?;
                Value::Varint {
                    range: ByteRange {
                        start: value_start,
                        end: *cursor,
                    },
                    unsigned: U64(unsigned),
                    canonical,
                }
            }
            1 => {
                let bytes = self
                    .input
                    .get(*cursor..end)
                    .and_then(|bytes| bytes.get(..8))
                    .ok_or(ErrorCode::TruncatedFixed64)?;
                let little_endian_bits = U64(u64::from_le_bytes(
                    bytes.try_into().expect("eight checked bytes"),
                ));
                *cursor += 8;
                Value::Fixed64 {
                    range: ByteRange {
                        start: value_start,
                        end: *cursor,
                    },
                    little_endian_bits,
                }
            }
            2 => {
                let (length, canonical_length) = varint(self.input, cursor, end)?;
                let length_prefix = ByteRange {
                    start: value_start,
                    end: *cursor,
                };
                let available = end - *cursor;
                if length > available as u64 {
                    return Err(ErrorCode::LengthOutOfBounds);
                }
                let bytes = ByteRange {
                    start: *cursor,
                    end: *cursor + length as usize,
                };
                *cursor = bytes.end;
                let candidates = self.candidates(bytes, depth);
                Value::LengthDelimited {
                    length_prefix,
                    bytes,
                    canonical_length,
                    candidates,
                }
            }
            3 => {
                if depth >= self.limits.max_depth {
                    return Err(ErrorCode::DepthLimit);
                }
                let parsed = self.message(*cursor, end, depth + 1, Some(number), candidate);
                if let Some(error) = parsed.message.error {
                    *cursor = error.offset;
                    return Err(error.code);
                }
                let end_key = parsed.end_key.ok_or(ErrorCode::UnclosedGroup)?;
                *cursor = end_key.end;
                Value::Group {
                    message: Box::new(parsed.message),
                    end_key,
                }
            }
            5 => {
                let bytes = self
                    .input
                    .get(*cursor..end)
                    .and_then(|bytes| bytes.get(..4))
                    .ok_or(ErrorCode::TruncatedFixed32)?;
                let little_endian_bits =
                    u32::from_le_bytes(bytes.try_into().expect("four checked bytes"));
                *cursor += 4;
                Value::Fixed32 {
                    range: ByteRange {
                        start: value_start,
                        end: *cursor,
                    },
                    little_endian_bits,
                }
            }
            _ => return Err(ErrorCode::InvalidWireType),
        };
        Ok(FieldResult::Field(Field {
            number,
            wire_type,
            raw: ByteRange {
                start,
                end: *cursor,
            },
            key,
            canonical_key,
            value,
        }))
    }

    fn candidates(&mut self, bytes: ByteRange, depth: usize) -> Candidates {
        let mut result = Candidates::default();
        if let Ok(text) = std::str::from_utf8(&self.input[bytes.start..bytes.end]) {
            let mut length = text.len().min(self.limits.max_text_bytes);
            while !text.is_char_boundary(length) {
                length -= 1;
            }
            result.utf8 = Some(text[..length].to_owned());
            result.utf8_preview_truncated = length < text.len();
        }
        if bytes.start == bytes.end {
            return result;
        }
        if depth >= self.limits.max_depth
            || bytes.end - bytes.start > self.limits.max_candidate_bytes
            || self.candidates_left == 0
        {
            result.message_analysis_limited = true;
            return result;
        }
        let parsed = self
            .message(bytes.start, bytes.end, depth + 1, None, true)
            .message;
        if parsed.complete && !parsed.fields.is_empty() {
            result.message = Some(Box::new(parsed));
        } else if parsed.error.as_ref().is_some_and(|error| {
            matches!(error.code, ErrorCode::DepthLimit | ErrorCode::FieldLimit)
        }) {
            result.message_analysis_limited = true;
        }
        result
    }
}

enum FieldResult {
    Field(Field),
    EndGroup(ByteRange),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(bytes: &[u8]) -> ErrorCode {
        decode(bytes, Limits::default()).error.unwrap().code
    }

    #[test]
    fn order_duplicates_and_original_ranges_survive() {
        let input = [8, 150, 1, 18, 2, b'h', b'i', 8, 1];
        let message = decode(&input, Limits::default());
        assert!(message.complete);
        assert_eq!(
            message
                .fields
                .iter()
                .map(|field| field.number)
                .collect::<Vec<_>>(),
            [1, 2, 1]
        );
        assert_eq!(message.fields[0].raw, ByteRange { start: 0, end: 3 });
        assert_eq!(message.fields[1].raw, ByteRange { start: 3, end: 7 });
        assert!(matches!(
            message.fields[0].value,
            Value::Varint {
                unsigned: U64(150),
                ..
            }
        ));
    }

    #[test]
    fn every_varint_boundary_and_noncanonical_values() {
        for value in [0, 1, 127, 128, 16383, 16384, u32::MAX as u64, u64::MAX] {
            let mut input = vec![8];
            let mut rest = value;
            while rest >= 128 {
                input.push(rest as u8 | 128);
                rest >>= 7;
            }
            input.push(rest as u8);
            let message = decode(&input, Limits::default());
            assert!(message.complete, "{value}");
            assert!(
                matches!(message.fields[0].value, Value::Varint { unsigned: U64(v), canonical: true, .. } if v == value)
            );
        }
        let message = decode(&[0x88, 0, 0x81, 0], Limits::default());
        assert!(message.complete);
        assert!(!message.fields[0].canonical_key);
        assert!(matches!(
            message.fields[0].value,
            Value::Varint {
                unsigned: U64(1),
                canonical: false,
                ..
            }
        ));
    }

    #[test]
    fn fixed_values_are_bits_not_assumed_float_types() {
        let message = decode(
            &[13, 0x78, 0x56, 0x34, 0x12, 17, 8, 7, 6, 5, 4, 3, 2, 1],
            Limits::default(),
        );
        assert!(message.complete);
        assert!(matches!(
            message.fields[0].value,
            Value::Fixed32 {
                little_endian_bits: 0x12345678,
                ..
            }
        ));
        assert!(matches!(
            message.fields[1].value,
            Value::Fixed64 {
                little_endian_bits: U64(0x0102030405060708),
                ..
            }
        ));
    }

    #[test]
    fn length_delimited_can_have_multiple_candidates() {
        let message = decode(&[10, 2, 8, 1], Limits::default());
        let Value::LengthDelimited {
            candidates, bytes, ..
        } = &message.fields[0].value
        else {
            panic!()
        };
        assert_eq!(*bytes, ByteRange { start: 2, end: 4 });
        assert!(candidates.utf8.is_some());
        assert_eq!(candidates.message.as_ref().unwrap().fields[0].number, 1);
        assert!(decode(&[10, 3, 0xff, 0xff, 0xff], Limits::default()).complete);
    }

    #[test]
    fn groups_are_matched_and_nesting_is_bounded() {
        let message = decode(&[11, 16, 1, 12], Limits::default());
        assert!(message.complete);
        assert!(
            matches!(message.fields[0].value, Value::Group { ref message, end_key: ByteRange { start: 3, end: 4 } } if message.fields[0].number == 2)
        );
        assert_eq!(error(&[12]), ErrorCode::UnexpectedEndGroup);
        assert_eq!(error(&[11, 20]), ErrorCode::MismatchedEndGroup);
        assert_eq!(error(&[11, 8, 1]), ErrorCode::UnclosedGroup);
        let limits = Limits {
            max_depth: 1,
            ..Limits::default()
        };
        assert_eq!(
            decode(&[11, 11, 12, 12], limits).error.unwrap().code,
            ErrorCode::DepthLimit
        );
    }

    #[test]
    fn truncation_and_overflow_never_read_past_input() {
        for (input, code) in [
            (vec![8, 128], ErrorCode::TruncatedVarint),
            (
                vec![8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 2],
                ErrorCode::VarintOverflow,
            ),
            (vec![9, 0], ErrorCode::TruncatedFixed64),
            (vec![13, 0], ErrorCode::TruncatedFixed32),
            (
                vec![10, 255, 255, 255, 255, 15],
                ErrorCode::LengthOutOfBounds,
            ),
            (vec![0], ErrorCode::InvalidFieldNumber),
            (vec![14], ErrorCode::InvalidWireType),
        ] {
            assert_eq!(error(&input), code);
        }
        let message = decode(&[8, 1, 18, 3, 0], Limits::default());
        assert_eq!(message.fields.len(), 1);
        assert_eq!(message.undecoded, Some(ByteRange { start: 2, end: 5 }));
    }

    #[test]
    fn resource_limits_apply_to_groups_and_optional_candidates() {
        let message = decode(
            &[8, 1, 16, 2],
            Limits {
                max_fields: 1,
                ..Limits::default()
            },
        );
        assert_eq!(message.error.unwrap().code, ErrorCode::FieldLimit);
        let message = decode(
            &[10, 2, 8, 1],
            Limits {
                max_candidate_fields: 0,
                ..Limits::default()
            },
        );
        assert!(message.complete);
        assert!(
            matches!(message.fields[0].value, Value::LengthDelimited { ref candidates, .. } if candidates.message_analysis_limited)
        );
        assert_eq!(
            decode(
                &[8, 1],
                Limits {
                    max_input_bytes: 1,
                    ..Limits::default()
                }
            )
            .error
            .unwrap()
            .code,
            ErrorCode::InputLimit
        );
    }

    #[test]
    fn callers_cannot_disable_the_recursion_ceiling() {
        let mut input = vec![11; 1000];
        input.extend(vec![12; 1000]);
        let message = decode(
            &input,
            Limits {
                max_depth: usize::MAX,
                ..Limits::default()
            },
        );
        assert_eq!(message.error.unwrap().code, ErrorCode::DepthLimit);
    }
}
