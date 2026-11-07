use crate::{
    capture::summarize,
    contract::{ApiError, ByteRange, FieldNode, FieldPresentation, PacketDetail},
};
use qscope_core::{data::sha256, formats, native, wire};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
pub struct DecodeQuery {
    pub format: Option<String>,
    pub offset: Option<usize>,
    pub length: Option<usize>,
    /// 按前序遍历计数的已接收字段。
    pub node_offset: Option<usize>,
}

// 字段视图按前序节点分页，每个包使用固定总量上限。
const VIEW_PAGE: usize = 2000;
const VIEW_NODES: usize = 20_000;

struct Budget {
    left: usize,
    limited: bool,
}
impl Budget {
    fn take(&mut self) -> bool {
        if self.left == 0 {
            self.limited = true;
            return false;
        }
        self.left -= 1;
        true
    }
}
// 跳过前序遍历的前 skip 个节点，保留后续节点的祖先外壳，便于客户端按 ID 拼接子节点。
fn after(nodes: Vec<FieldNode>, skip: usize, seen: &mut usize) -> Vec<FieldNode> {
    nodes
        .into_iter()
        .filter_map(|mut node| {
            let index = *seen;
            *seen += 1;
            node.children = after(std::mem::take(&mut node.children), skip, seen);
            (index >= skip || !node.children.is_empty()).then_some(node)
        })
        .collect()
}
fn page(result: &mut PacketDetail, budget: &Budget, skip: usize) {
    let end = (skip + VIEW_PAGE).min(VIEW_NODES);
    result.view_limited = budget.limited;
    result.next_node_offset = (budget.limited && end < VIEW_NODES).then_some(end);
    if skip > 0 {
        result.nodes = after(std::mem::take(&mut result.nodes), skip, &mut 0);
    }
}

fn shifted(range: wire::ByteRange, base: usize) -> ByteRange {
    ByteRange {
        start: range.start + base,
        end: range.end + base,
    }
}
fn pb_fields(
    message: &wire::Message,
    base: usize,
    prefix: &str,
    budget: &mut Budget,
) -> Vec<FieldNode> {
    let mut nodes = Vec::new();
    for (i, field) in message.fields.iter().enumerate() {
        if !budget.take() {
            break;
        }
        let id = format!("{prefix}.{i}");
        let mut children = Vec::new();
        let (kind, value) = match &field.value {
            wire::Value::Varint {
                unsigned,
                canonical,
                ..
            } => (
                "varint",
                format!(
                    "{}{}",
                    unsigned.0,
                    if *canonical {
                        ""
                    } else {
                        " · 非最短编码"
                    }
                ),
            ),
            wire::Value::Fixed64 {
                little_endian_bits, ..
            } => (
                "fixed64",
                format!("0x{:016x} · 原始位模式", little_endian_bits.0),
            ),
            wire::Value::Fixed32 {
                little_endian_bits, ..
            } => (
                "fixed32",
                format!("0x{little_endian_bits:08x} · 原始位模式"),
            ),
            wire::Value::Group { message, .. } => {
                children = pb_fields(message, base, &id, budget);
                ("group", format!("{} 个字段", message.fields.len()))
            }
            wire::Value::LengthDelimited {
                bytes, candidates, ..
            } => {
                if let Some(message) = &candidates.message
                    && budget.take()
                {
                    children.push(FieldNode {
                        id: format!("{id}.candidate"),
                        label: "嵌套消息候选".into(),
                        value: format!("{} 个字段 · 未确认", message.fields.len()),
                        range: shifted(*bytes, base),
                        candidate: true,
                        children: pb_fields(message, base, &format!("{id}.candidate"), budget),
                        presentation: FieldPresentation {
                            kind: "object".into(),
                            value_range: shifted(*bytes, base),
                            ..Default::default()
                        },
                    });
                }
                let text = candidates
                    .utf8
                    .as_ref()
                    .map(|s| {
                        format!(
                            " · UTF-8 候选 {s:?}{}",
                            if candidates.utf8_preview_truncated {
                                "…"
                            } else {
                                ""
                            }
                        )
                    })
                    .unwrap_or_default();
                (
                    "bytes",
                    format!(
                        "{} B{text}{}",
                        bytes.end - bytes.start,
                        if candidates.message_analysis_limited {
                            " · 候选分析受限"
                        } else {
                            ""
                        }
                    ),
                )
            }
        };
        let value_range = match &field.value {
            wire::Value::LengthDelimited { bytes, .. } => shifted(*bytes, base),
            wire::Value::Varint { range, .. }
            | wire::Value::Fixed32 { range, .. }
            | wire::Value::Fixed64 { range, .. } => shifted(*range, base),
            wire::Value::Group { message, .. } => shifted(message.range, base),
        };
        let value_kind = match &field.value {
            wire::Value::Varint { .. } => "number",
            wire::Value::Fixed32 { .. } | wire::Value::Fixed64 { .. } => "literal",
            wire::Value::Group { .. } => "object",
            wire::Value::LengthDelimited { candidates, .. } => {
                if candidates.utf8.is_some() {
                    "string"
                } else if !children.is_empty() {
                    "object"
                } else {
                    "bytes"
                }
            }
        };
        nodes.push(FieldNode {
            id,
            label: format!("{} · {kind}", field.number),
            value,
            range: shifted(field.raw, base),
            candidate: false,
            children,
            presentation: FieldPresentation {
                key: field.number.to_string(),
                kind: value_kind.into(),
                type_name: kind.into(),
                value_range,
                ..Default::default()
            },
        });
    }
    nodes
}

fn json_range(value: &Value, base: usize) -> ByteRange {
    ByteRange {
        start: value["start"].as_u64().unwrap_or(0) as usize + base,
        end: value["end"].as_u64().unwrap_or(0) as usize + base,
    }
}
fn other_field(
    field: &Value,
    format: &str,
    base: usize,
    id: String,
    prefix: &str,
    budget: &mut Budget,
) -> Option<FieldNode> {
    if !budget.take() {
        return None;
    }
    let tag = field["tag"].as_u64().unwrap_or(0);
    let wire_type = field["wire_type"].as_u64().unwrap_or(0);
    let value = &field["value"];
    let mut children = Vec::new();
    if let Some(fields) = value["fields"].as_array() {
        for (i, field) in fields.iter().enumerate() {
            if let Some(node) = other_field(field, format, base, format!("{id}.f{i}"), "", budget) {
                children.push(node);
            } else {
                break;
            }
        }
    }
    if let Some(items) = value["items"].as_array() {
        for (i, item) in items.iter().enumerate() {
            if item.get("key").is_some() {
                for name in ["key", "value"] {
                    if let Some(node) = other_field(
                        &item[name],
                        format,
                        base,
                        format!("{id}.{i}.{name}"),
                        &format!("[{i}] {name} · "),
                        budget,
                    ) {
                        children.push(node);
                    }
                }
            } else if let Some(node) = other_field(
                item,
                format,
                base,
                format!("{id}.{i}"),
                &format!("[{i}] · "),
                budget,
            ) {
                children.push(node);
            }
            if budget.left == 0 {
                budget.limited |= i + 1 < items.len();
                break;
            }
        }
    }
    let description = if let Some(n) = value["signed_decimal"].as_str() {
        n.into()
    } else if let Some(bits) = value["big_endian_float_bits"].as_str() {
        format!("0x{bits} · 浮点原始位模式")
    } else if value.get("bytes").is_some() {
        let range = json_range(&value["bytes"], base);
        format!(
            "{} B{}",
            range.end.saturating_sub(range.start),
            value["utf8"]
                .as_str()
                .map(|s| format!(
                    " · UTF-8 {s:?}{}",
                    if value["utf8_preview_limited"].as_bool().unwrap_or(false) {
                        "…"
                    } else {
                        ""
                    }
                ))
                .unwrap_or_default()
        )
    } else if let Some(items) = value["items"].as_array() {
        format!("{} 项", items.len())
    } else {
        format!("{} 个子字段", children.len())
    };
    Some(FieldNode {
        id,
        label: if format == "tlv16be" {
            format!("tag 0x{tag:04x} · TLV")
        } else {
            format!("{prefix}tag {tag} · type {wire_type}")
        },
        value: description,
        range: json_range(&field["raw"], base),
        candidate: false,
        children,
        presentation: FieldPresentation {
            key: if format == "tlv16be" {
                format!("0x{tag:04x}")
            } else {
                format!("{prefix}{tag}")
            },
            kind: if value.get("bytes").is_some() {
                if value["utf8"].is_string() {
                    "string"
                } else {
                    "bytes"
                }
            } else if value.get("fields").is_some() || value.get("items").is_some() {
                "object"
            } else {
                "literal"
            }
            .into(),
            type_name: if format == "tlv16be" {
                "TLV16-BE".into()
            } else {
                format!("JCE {wire_type}")
            },
            value_range: if value.get("bytes").is_some() {
                json_range(&value["bytes"], base)
            } else {
                json_range(&field["raw"], base)
            },
            ..Default::default()
        },
    })
}

pub fn detail(event: native::Event, query: &DecodeQuery) -> Result<PacketDetail, ApiError> {
    let format = query.format.as_deref().unwrap_or("pb");
    if !["pb", "hex", "tlv16be", "jce", "http"].contains(&format) {
        return Err(ApiError::new("format", "未知解码格式"));
    }
    let start = query.offset.unwrap_or(0);
    let length = query
        .length
        .unwrap_or(event.payload.len().saturating_sub(start));
    let end = start
        .checked_add(length)
        .filter(|end| *end <= event.payload.len())
        .ok_or_else(|| ApiError::new("range", "解码区域超出包体"))?;
    let input = event
        .payload
        .get(start..end)
        .ok_or_else(|| ApiError::new("range", "解码区域超出包体"))?;
    let skip = query.node_offset.unwrap_or(0);
    if skip >= VIEW_NODES {
        return Err(ApiError::new("range", "字段分页超出视图上限"));
    }
    let mut budget = Budget {
        left: (skip + VIEW_PAGE).min(VIEW_NODES),
        limited: false,
    };
    let mut result = PacketDetail {
        event: summarize(&event),
        payload_sha256: sha256(&event.payload),
        payload_hex: event.payload_hex,
        format: format.into(),
        decode_range: ByteRange { start, end },
        nodes: vec![],
        complete: None,
        error: None,
        undecoded: None,
        view_limited: false,
        next_node_offset: None,
    };
    if event.kind != "packet" || format == "hex" {
        return Ok(result);
    }
    if format == "http" {
        let Some(split) = input
            .windows(4)
            .take(64 * 1024)
            .position(|part| part == b"\r\n\r\n")
        else {
            result.error = Some(ApiError::new(
                "http_headers",
                "选区内没有完整 HTTP 消息头，原始字节已保留",
            ));
            result.complete = Some(false);
            result.undecoded = Some(ByteRange { start, end });
            return Ok(result);
        };
        let mut at = 0;
        for (i, line) in input[..split + 2].split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            if !budget.take() {
                break;
            }
            let text = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line));
            let clean = line.strip_suffix(b"\r").unwrap_or(line);
            let mut value_at = if i == 0 {
                0
            } else {
                clean.iter().position(|b| *b == b':').map_or(0, |at| at + 1)
            };
            while value_at < clean.len() && clean[value_at].is_ascii_whitespace() {
                value_at += 1;
            }
            let (label, value) = if i == 0 {
                ("HTTP 起始行".to_string(), text.to_string())
            } else {
                let (name, value) = text.split_once(':').unwrap_or(("header", &text));
                (name.to_string(), value.trim().chars().take(512).collect())
            };
            result.nodes.push(FieldNode {
                id: format!("http.{i}"),
                label,
                value,
                range: ByteRange {
                    start: start + at,
                    end: start + at + line.len(),
                },
                candidate: false,
                children: vec![],
                presentation: FieldPresentation {
                    kind: if std::str::from_utf8(&clean[value_at..]).is_ok() {
                        "string"
                    } else {
                        "bytes"
                    }
                    .into(),
                    type_name: "HTTP".into(),
                    value_range: ByteRange {
                        start: start + at + value_at,
                        end: start + at + clean.len(),
                    },
                    ..Default::default()
                },
            });
            at += line.len() + 1;
        }
        let body_start = start + split + 4;
        result.nodes.push(FieldNode {
            id: "http.body".into(),
            label: "HTTP body · bytes".into(),
            value: format!(
                "{} B · 选中后可解析选区，再切换 PB / TLV / JCE",
                end - body_start
            ),
            range: ByteRange {
                start: body_start,
                end,
            },
            candidate: false,
            children: vec![],
            presentation: FieldPresentation {
                key: "body".into(),
                kind: "bytes".into(),
                type_name: "HTTP body".into(),
                value_range: ByteRange {
                    start: body_start,
                    end,
                },
                ..Default::default()
            },
        });
        result.complete = Some(!event.truncated);
        if event.truncated {
            result.error = Some(ApiError::new(
                "http_truncated",
                "HTTP 采集达到包长限额或大包已流式转发，已保存部分字节",
            ));
        }
        page(&mut result, &budget, skip);
        crate::field::present(&mut result.nodes, &event.payload);
        return Ok(result);
    }
    if format == "pb" {
        let message = wire::decode(
            input,
            wire::Limits {
                max_fields: 5000,
                max_candidate_fields: 1000,
                ..Default::default()
            },
        );
        result.nodes = pb_fields(&message, start, "pb", &mut budget);
        result.complete = Some(message.complete);
        result.undecoded = message.undecoded.map(|r| shifted(r, start));
        result.error = message.error.map(|e| ApiError {
            code: serde_json::to_value(e.code)
                .expect("error enum")
                .as_str()
                .expect("string enum")
                .into(),
            message: "当前区域无法完整按 PB 解释；原字节已保留".into(),
            offset: Some(start + e.offset),
        });
    } else {
        let report = if format == "jce" {
            formats::jce(input)
        } else {
            formats::tlv16be(input)
        };
        for (i, field) in report.fields.iter().enumerate() {
            if let Some(node) = other_field(
                &serde_json::to_value(field).expect("field"),
                format,
                start,
                format!("{format}.{i}"),
                "",
                &mut budget,
            ) {
                result.nodes.push(node);
            } else {
                break;
            }
        }
        result.complete = Some(report.complete);
        result.undecoded = report.undecoded.map(|r| shifted(r, start));
        result.error = report.error.map(|e| ApiError {
            code: e.code.into(),
            message: "当前区域无法完整按所选格式解释；原字节已保留".into(),
            offset: Some(start + e.offset),
        });
    }
    page(&mut result, &budget, skip);
    crate::field::present(&mut result.nodes, &event.payload);
    Ok(result)
}
