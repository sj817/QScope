use crate::{catalog::Library, contract::*, decode};
use std::collections::{BTreeMap, HashMap};

// 保留字段身份和重复项，对齐依据独立于字节偏移及字段顺序。
fn flatten(nodes: &[FieldNode], prefix: &str, hex: &str, out: &mut Vec<(String, String, String)>) {
    let mut occurrences = HashMap::<&str, usize>::new();
    for node in nodes {
        let occurrence = occurrences.entry(&node.label).or_default();
        *occurrence += 1;
        let path = format!("{prefix}/{}[{}]", node.label, occurrence);
        let raw = hex
            .get(node.range.start * 2..node.range.end * 2)
            .unwrap_or("");
        out.push((
            path.clone(),
            node.value.clone(),
            qscope_core::data::sha256(raw.as_bytes()),
        ));
        flatten(&node.children, &path, hex, out);
    }
}

pub fn run(library: &Library, request: CompareRequest) -> Result<Comparison, ApiError> {
    if !(2..=8).contains(&request.items.len()) {
        return Err(ApiError::new("query", "请选择 2–8 个协议包进行对比"));
    }
    let format = request.format.as_deref().unwrap_or("pb");
    let mut columns = Vec::new();
    let mut rows = Vec::<CompareRow>::new();
    let mut paths = BTreeMap::<String, usize>::new();
    let mut limited = false;
    for (column, source) in request.items.iter().enumerate() {
        let capture = library.get(&source.session_id)?;
        let event = capture
            .lock()
            .map_err(|_| ApiError::new("worker", "会话锁不可用"))?
            .event(
                source
                    .event_id
                    .parse()
                    .map_err(|_| ApiError::new("query", "无效事件 ID"))?,
            )?;
        if event.kind != "packet" {
            return Err(ApiError::new(
                "query",
                "采集控制事件没有协议包体，不能参与对比",
            ));
        }
        let detail = decode::detail(
            event,
            &decode::DecodeQuery {
                format: Some(format.into()),
                ..Default::default()
            },
        )?;
        limited |= detail.view_limited;
        let mut fields = Vec::new();
        flatten(&detail.nodes, "", &detail.payload_hex, &mut fields);
        for (path, value, fingerprint) in fields {
            let index = if let Some(index) = paths.get(&path) {
                *index
            } else {
                if rows.len() >= 4000 {
                    limited = true;
                    continue;
                }
                let index = rows.len();
                paths.insert(path.clone(), index);
                rows.push(CompareRow {
                    path,
                    values: vec![None; request.items.len()],
                    fingerprints: vec![None; request.items.len()],
                    changed: false,
                });
                index
            };
            rows[index].values[column] = Some(value);
            rows[index].fingerprints[column] = Some(fingerprint);
        }
        columns.push(CompareColumn {
            source: source.clone(),
            event: detail.event,
            payload_hex: detail.payload_hex,
            payload_sha256: detail.payload_sha256,
            error: detail.error,
        });
    }
    for row in &mut rows {
        row.changed = row.values.windows(2).any(|pair| pair[0] != pair[1])
            || row.fingerprints.windows(2).any(|pair| pair[0] != pair[1]);
    }
    Ok(Comparison {
        columns,
        rows,
        view_limited: limited,
    })
}
