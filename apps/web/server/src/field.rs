//! 人类可读投影以原始抓包范围和线类型为依据。
use crate::contract::{ApiError, FieldNode, FieldText, PacketDetail};
use serde::Deserialize;

#[derive(Default, Deserialize)]
pub struct TextQuery {
    pub format: Option<String>,
    pub offset: Option<usize>,
    pub length: Option<usize>,
    pub node_id: Option<String>,
    pub mode: Option<String>,
}

pub fn children(node: &FieldNode) -> &[FieldNode] {
    if node.presentation.kind == "string" {
        return &[];
    }
    if node.children.len() == 1 && node.children[0].candidate {
        &node.children[0].children
    } else {
        &node.children
    }
}
pub fn present(nodes: &mut [FieldNode], payload: &[u8]) {
    for node in nodes {
        present(&mut node.children, payload);
        let p = &mut node.presentation;
        if p.key.is_empty() {
            p.key = node.label.clone();
        }
        if p.type_name.is_empty() {
            p.type_name = if node.candidate { "message?" } else { "value" }.into();
        }
        p.value_length = p.value_range.end.saturating_sub(p.value_range.start);
        let bytes = payload
            .get(p.value_range.start..p.value_range.end)
            .unwrap_or(&[]);
        if p.kind == "string"
            && p.type_name == "bytes"
            && std::str::from_utf8(bytes).is_ok_and(|text| {
                text.chars()
                    .any(|c| c.is_control() && !['\n', '\r', '\t'].contains(&c))
            })
        {
            p.kind = if node.children.is_empty() {
                "bytes"
            } else {
                "object"
            }
            .into();
        }
        let value = match p.kind.as_str() {
            "string" => String::from_utf8_lossy(bytes).into_owned(),
            "bytes" => qscope_core::data::to_hex(&bytes[..bytes.len().min(64)]),
            "object" => String::new(),
            _ => node
                .value
                .split(" · ")
                .next()
                .unwrap_or(&node.value)
                .to_string(),
        };
        p.preview_limited = (p.kind == "bytes" && bytes.len() > 64) || value.chars().count() > 256;
        p.value = value.chars().take(256).collect();
    }
}
fn find<'a>(nodes: &'a [FieldNode], id: &str) -> Option<&'a FieldNode> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = find(&node.children, id) {
            return Some(found);
        }
    }
    None
}
fn full_value(node: &FieldNode, payload: &[u8]) -> Result<String, ApiError> {
    let p = &node.presentation;
    let bytes = payload
        .get(p.value_range.start..p.value_range.end)
        .ok_or_else(|| ApiError::new("range", "字段字节范围不可用"))?;
    Ok(match p.kind.as_str() {
        "string" => std::str::from_utf8(bytes)
            .map_err(|_| ApiError::new("format", "当前字段不再是有效 UTF-8"))?
            .into(),
        "bytes" => qscope_core::data::to_hex(bytes),
        "object" => json_object(children(node), payload, 0)?,
        _ => node
            .value
            .split(" · ")
            .next()
            .unwrap_or(&node.value)
            .to_string(),
    })
}
fn json_value(node: &FieldNode, payload: &[u8], depth: usize) -> Result<String, ApiError> {
    if node.presentation.kind == "object" {
        json_object(children(node), payload, depth)
    } else {
        serde_json::to_string(&full_value(node, payload)?)
            .map_err(|e| ApiError::new("worker", e.to_string()))
    }
}
fn json_object(nodes: &[FieldNode], payload: &[u8], depth: usize) -> Result<String, ApiError> {
    if depth > 64 {
        return Err(ApiError::new("range", "结构深度超过上限"));
    }
    // 保留首次出现顺序及全部重复项，重复键分别记录。
    let mut groups = Vec::<(&str, Vec<&FieldNode>)>::new();
    for node in nodes {
        let key = node.presentation.key.as_str();
        if let Some((_, values)) = groups.iter_mut().find(|(k, _)| *k == key) {
            values.push(node);
        } else {
            groups.push((key, vec![node]));
        }
    }
    let mut fields = Vec::new();
    for (key, values) in groups {
        let key = serde_json::to_string(key).map_err(|e| ApiError::new("worker", e.to_string()))?;
        let values: Vec<_> = values
            .iter()
            .map(|node| json_value(node, payload, depth + 1))
            .collect::<Result<_, _>>()?;
        let value = if values.len() == 1 {
            values.into_iter().next().unwrap_or_default()
        } else {
            format!("[{}]", values.join(", "))
        };
        fields.push(format!("{}{}: {}", "  ".repeat(depth + 1), key, value));
    }
    Ok(if fields.is_empty() {
        "{}".into()
    } else {
        format!("{{\n{}\n{}}}", fields.join(",\n"), "  ".repeat(depth))
    })
}
pub fn text(detail: &PacketDetail, query: &TextQuery) -> Result<FieldText, ApiError> {
    let bytes = qscope_core::data::from_hex(&detail.payload_hex, qscope_core::native::MAX_PAYLOAD)
        .map_err(|e| ApiError::new("worker", e.to_string()))?;
    let mode = query.mode.as_deref().unwrap_or("structure");
    let text = if mode == "structure" {
        json_object(&detail.nodes, &bytes, 0)?
    } else {
        let node = find(
            &detail.nodes,
            query
                .node_id
                .as_deref()
                .ok_or_else(|| ApiError::new("query", "请选择一个字段"))?,
        )
        .ok_or_else(|| ApiError::new("not_found", "字段不存在"))?;
        match mode {
            "value" => full_value(node, &bytes)?,
            "subtree" => json_object(std::slice::from_ref(node), &bytes, 0)?,
            _ => return Err(ApiError::new("query", "未知复制方式")),
        }
    };
    if text.len() > 8 * 1024 * 1024 {
        return Err(ApiError::new(
            "file_limit",
            "结构文本超过 8 MiB 上限，仍可导出原始包体",
        ));
    }
    Ok(FieldText {
        text,
        complete: detail.error.is_none() && !detail.view_limited,
        content_type: if mode == "value" {
            "text/plain"
        } else {
            "application/json"
        }
        .into(),
    })
}
