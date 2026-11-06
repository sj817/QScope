//! Explicit TLV16-BE and JCE/Tars views of unchanged bytes. No format guessing.
//! JCE wire types: https://tarscloud.github.io/TarsDocs/base/tars-protocol.html
use crate::{data::to_hex, wire::ByteRange};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_INPUT: usize = 1024 * 1024;
const MAX_FIELDS: usize = 100_000;
const MAX_DEPTH: usize = 32;

#[derive(Debug, Serialize)]
pub struct Field {
    pub tag: u32,
    pub wire_type: u8,
    pub raw: ByteRange,
    pub value: Value,
}
#[derive(Debug, Serialize)]
pub struct DecodeError {
    pub offset: usize,
    pub code: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub format: &'static str,
    pub fields: Vec<Field>,
    pub complete: bool,
    pub error: Option<DecodeError>,
    pub undecoded: Option<ByteRange>,
}
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    fields: usize,
}
type Result<T> = std::result::Result<T, DecodeError>;
impl<'a> Cursor<'a> {
    fn fail(&self, code: &'static str) -> DecodeError {
        DecodeError {
            offset: self.pos,
            code,
        }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| self.fail("length_overflow"))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| self.fail("truncated"))?;
        self.pos = end;
        Ok(slice)
    }
    fn head(&mut self) -> Result<(u32, u8)> {
        if self.fields >= MAX_FIELDS {
            return Err(self.fail("field_limit"));
        }
        self.fields += 1;
        let b = self.take(1)?[0];
        let tag = if b >> 4 == 15 {
            self.take(1)?[0] as u32
        } else {
            (b >> 4) as u32
        };
        Ok((tag, b & 15))
    }
    fn integer(&mut self, kind: u8) -> Result<i64> {
        Ok(match kind {
            0 => i8::from_be_bytes(self.take(1)?.try_into().expect("one byte")) as i64,
            1 => i16::from_be_bytes(self.take(2)?.try_into().expect("two bytes")) as i64,
            2 => i32::from_be_bytes(self.take(4)?.try_into().expect("four bytes")) as i64,
            3 => i64::from_be_bytes(self.take(8)?.try_into().expect("eight bytes")),
            12 => 0,
            _ => return Err(self.fail("expected_integer")),
        })
    }
    fn count(&mut self) -> Result<usize> {
        let (tag, kind) = self.head()?;
        if tag != 0 {
            return Err(self.fail("invalid_size_tag"));
        }
        let n = self.integer(kind)?;
        if n < 0 {
            return Err(self.fail("negative_size"));
        }
        usize::try_from(n).map_err(|_| self.fail("length_overflow"))
    }
    fn bytes_value(&mut self, n: usize) -> Result<Value> {
        let start = self.pos;
        let raw = self.take(n)?;
        let (preview, limited) = match std::str::from_utf8(raw) {
            Ok(text) => {
                let mut chars = text.chars();
                let preview = chars.by_ref().take(256).collect::<String>();
                (Some(preview), chars.next().is_some())
            }
            Err(_) => (None, false),
        };
        Ok(
            json!({"bytes":ByteRange{start,end:self.pos},"utf8":preview,"utf8_preview_limited":limited}),
        )
    }
    fn field(&mut self, depth: usize) -> Result<Field> {
        if depth >= MAX_DEPTH {
            return Err(self.fail("depth_limit"));
        }
        let start = self.pos;
        let (tag, kind) = self.head()?;
        let value = match kind {
            0..=3 | 12 => json!({"signed_decimal":self.integer(kind)?.to_string()}),
            4 => json!({"big_endian_float_bits":to_hex(self.take(4)?)}),
            5 => json!({"big_endian_float_bits":to_hex(self.take(8)?)}),
            6 => {
                let n = self.take(1)?[0] as usize;
                self.bytes_value(n)?
            }
            7 => {
                let n = u32::from_be_bytes(self.take(4)?.try_into().expect("four bytes")) as usize;
                self.bytes_value(n)?
            }
            8 | 9 => {
                let n = self.count()?;
                let fields_per_item = if kind == 8 { 2 } else { 1 };
                if n > MAX_FIELDS.saturating_sub(self.fields) / fields_per_item
                    || n > (self.bytes.len() - self.pos) / fields_per_item
                {
                    return Err(self.fail("container_size_limit"));
                }
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    let key = self.field(depth + 1)?;
                    if key.tag != 0 {
                        return Err(self.fail("invalid_element_tag"));
                    }
                    if kind == 8 {
                        let value = self.field(depth + 1)?;
                        if value.tag != 1 {
                            return Err(self.fail("invalid_map_value_tag"));
                        }
                        items.push(json!({"key":key,"value":value}));
                    } else {
                        items.push(json!(key));
                    }
                }
                json!({"items":items})
            }
            10 => {
                let mut fields = Vec::new();
                loop {
                    if self.pos == self.bytes.len() {
                        return Err(self.fail("unclosed_struct"));
                    }
                    if self.bytes[self.pos] & 15 == 11 {
                        let end = self.pos;
                        let (tag, _) = self.head()?;
                        if tag != 0 {
                            return Err(self.fail("invalid_struct_end_tag"));
                        }
                        break json!({"fields":fields,"end":ByteRange{start:end,end:self.pos}});
                    }
                    fields.push(self.field(depth + 1)?);
                }
            }
            11 => return Err(self.fail("unexpected_struct_end")),
            13 => {
                let (inner_tag, inner_kind) = self.head()?;
                if inner_tag != 0 || inner_kind != 0 {
                    return Err(self.fail("invalid_simple_list_type"));
                }
                let n = self.count()?;
                self.bytes_value(n)?
            }
            _ => return Err(self.fail("invalid_wire_type")),
        };
        Ok(Field {
            tag,
            wire_type: kind,
            raw: ByteRange {
                start,
                end: self.pos,
            },
            value,
        })
    }
}

fn decode(
    bytes: &[u8],
    format: &'static str,
    mut parse: impl FnMut(&mut Cursor<'_>) -> Result<Field>,
) -> Report {
    let mut report = Report {
        format,
        fields: Vec::new(),
        complete: true,
        error: None,
        undecoded: None,
    };
    if bytes.len() > MAX_INPUT {
        report.complete = false;
        report.error = Some(DecodeError {
            offset: 0,
            code: "input_limit",
        });
        report.undecoded = Some(ByteRange {
            start: 0,
            end: bytes.len(),
        });
        return report;
    }
    let mut cursor = Cursor {
        bytes,
        pos: 0,
        fields: 0,
    };
    while cursor.pos < bytes.len() {
        let start = cursor.pos;
        match parse(&mut cursor) {
            Ok(field) => report.fields.push(field),
            Err(error) => {
                report.complete = false;
                report.error = Some(error);
                report.undecoded = Some(ByteRange {
                    start,
                    end: bytes.len(),
                });
                break;
            }
        }
    }
    report
}
pub fn jce(bytes: &[u8]) -> Report {
    decode(bytes, "jce", |cursor| cursor.field(0))
}
/// A stream of 16-bit tag + 16-bit value length, both big-endian. The caller
/// selects the TLV stream boundary; WTLogin outer headers are not stripped.
pub fn tlv16be(bytes: &[u8]) -> Report {
    decode(bytes, "tlv16be", |cursor| {
        if cursor.fields >= MAX_FIELDS {
            return Err(cursor.fail("field_limit"));
        }
        cursor.fields += 1;
        let start = cursor.pos;
        let header = cursor.take(4)?;
        let tag = u16::from_be_bytes([header[0], header[1]]) as u32;
        let n = u16::from_be_bytes([header[2], header[3]]) as usize;
        let value = cursor.bytes_value(n)?;
        Ok(Field {
            tag,
            wire_type: 0,
            raw: ByteRange {
                start,
                end: cursor.pos,
            },
            value,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tlv_keeps_duplicates_zero_lengths_and_incomplete_tail() {
        let r = tlv16be(&[1, 6, 0, 2, 8, 1, 1, 6, 0, 0, 0, 1, 0, 3, 1]);
        assert_eq!(r.fields.len(), 2);
        assert_eq!(r.fields[0].tag, 0x106);
        assert_eq!(r.fields[1].tag, 0x106);
        assert_eq!(r.fields[1].raw, ByteRange { start: 6, end: 10 });
        assert!(!r.complete);
        assert_eq!(r.undecoded, Some(ByteRange { start: 10, end: 15 }));
    }
    #[test]
    fn jce_handles_extended_tags_bytes_and_signed_64_bit_values() {
        let r = jce(&[
            0xf0, 128, 0xfe, 0x13, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x2d, 0, 0, 3,
            0x80, 0xff, 0,
        ]);
        assert!(r.complete);
        assert_eq!(r.fields[0].tag, 128);
        assert_eq!(r.fields[0].value["signed_decimal"], "-2");
        assert_eq!(r.fields[1].value["signed_decimal"], "9223372036854775807");
        assert_eq!(r.fields[2].value["bytes"], json!({"start":16,"end":19}));
    }
    #[test]
    fn jce_keeps_maps_lists_and_structs_as_ordered_fields() {
        let r = jce(&[
            0x08, 0x00, 0x01, 0x06, 0x01, b'x', 0x1c, 0x19, 0x00, 0x02, 0x00, 1, 0x00, 2, 0x2a,
            0x00, 7, 0x0b,
        ]);
        assert!(r.complete);
        assert_eq!(r.fields.len(), 3);
        assert_eq!(r.fields[0].value["items"][0]["value"]["tag"], 1);
        assert_eq!(r.fields[1].value["items"].as_array().unwrap().len(), 2);
        assert_eq!(
            r.fields[2].value["fields"][0]["value"]["signed_decimal"],
            "7"
        );
    }
    #[test]
    fn jce_rejects_bad_counts_types_unclosed_structs_and_deep_input() {
        for input in [
            &[0x09, 0, 0xff][..],
            &[0x0d, 1, 0, 1, 1],
            &[0x0a, 0x0c],
            &[0x0e],
        ] {
            assert!(!jce(input).complete);
        }
        let input = vec![0x0a; 100];
        assert_eq!(jce(&input).error.unwrap().code, "depth_limit");
    }
}
