//! Account view: launches of one observed account concatenated in start order.
//! Every row keeps its original (session_id, event_id); nothing is copied,
//! renumbered or interleaved, and each launch stays a separate raw file.
use crate::{
    capture::ListQuery,
    catalog::{Library, SharedCapture},
    contract::{ApiError, EventSummary, MergedEvent, MergedPage, Session},
};
use serde::Deserialize;
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

// Earlier launches must be fully indexed before rows are concatenated, or a
// cursor in a later launch would skip rows indexed afterwards.
const INDEX_BUDGET: Duration = Duration::from_millis(300);

#[derive(Debug, Default, Deserialize)]
pub struct Cursor {
    pub after_cursor: Option<String>,
    pub before_cursor: Option<String>,
}

fn valid_account(account: &str) -> bool {
    !account.is_empty()
        && account.len() <= qscope_core::native::MAX_ACCOUNT
        && account
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn position(text: &str, members: &[(Session, SharedCapture)]) -> Result<(usize, u64), ApiError> {
    let invalid = || ApiError::new("query", "游标格式无效");
    let (session, event) = text.split_once(':').ok_or_else(invalid)?;
    let event = event.parse().map_err(|_| invalid())?;
    let index = members
        .iter()
        .position(|(s, _)| s.id == session)
        .ok_or_else(|| ApiError::new("query", "游标对应的启动记录已不属于此账号"))?;
    Ok((index, event))
}

pub fn page(
    library: &Mutex<Library>,
    excluded: &Path,
    account: &str,
    query: &ListQuery,
    cursor: &Cursor,
) -> Result<MergedPage, ApiError> {
    if !valid_account(account) {
        return Err(ApiError::new("query", "账号格式无效"));
    }
    if query.after.is_some() || query.before.is_some() {
        return Err(ApiError::new(
            "query",
            "账号视图使用 after_cursor 或 before_cursor",
        ));
    }
    if cursor.after_cursor.is_some() && cursor.before_cursor.is_some() {
        return Err(ApiError::new(
            "query",
            "after_cursor 和 before_cursor 不能同时使用",
        ));
    }
    let limit = query.limit.unwrap_or(500).clamp(1, 1000);
    let members = library
        .lock()
        .map_err(|_| crate::lock_error())?
        .members(account, excluded);
    if members.is_empty() {
        return Err(ApiError::new("not_found", "没有属于此账号的启动记录"));
    }
    let count = members.len();
    let deadline = Instant::now() + INDEX_BUDGET;
    let mut segments = Vec::with_capacity(count);
    let mut indexing = false;
    for (index, (_, capture)) in members.iter().enumerate() {
        let mut capture = capture.lock().map_err(|_| crate::lock_error())?;
        loop {
            capture.refresh();
            if !capture.summary().has_more || index + 1 == count || Instant::now() > deadline {
                break;
            }
        }
        let summary = capture.summary();
        indexing |= summary.has_more && index + 1 < count;
        segments.push(summary);
    }
    let select = |index: usize,
                  after: Option<u64>,
                  before: Option<u64>,
                  limit: usize,
                  newest: bool|
     -> Result<(Vec<EventSummary>, bool), ApiError> {
        members[index]
            .1
            .lock()
            .map_err(|_| crate::lock_error())?
            .select(query, after, before, limit, newest)
    };
    // Validates the filter even when no rows are returned.
    select(count - 1, None, None, 1, true)?;
    let end = || -> Result<String, ApiError> {
        let last = members[count - 1]
            .1
            .lock()
            .map_err(|_| crate::lock_error())?
            .last_id();
        Ok(format!("{}:{last}", members[count - 1].0.id))
    };
    if indexing {
        return Ok(MergedPage {
            account: account.into(),
            segments,
            events: vec![],
            cursor: String::new(),
            has_earlier: false,
            has_more: false,
            indexing,
        });
    }
    let tag = |index: usize, rows: Vec<EventSummary>| -> Vec<MergedEvent> {
        rows.into_iter()
            .map(|event| MergedEvent {
                session_id: members[index].0.id.clone(),
                event,
            })
            .collect()
    };
    let mut events = vec![];
    let (mut has_earlier, mut has_more) = (false, false);
    if let Some(text) = &cursor.after_cursor {
        // After pages walk forward; they never report has_earlier.
        let (start, after) = position(text, &members)?;
        for index in start..count {
            let after = (index == start).then_some(after);
            let remaining = limit - events.len();
            if remaining == 0 {
                if !select(index, after, None, 1, false)?.0.is_empty() {
                    has_more = true;
                    break;
                }
                continue;
            }
            let (rows, beyond) = select(index, after, None, remaining, false)?;
            events.extend(tag(index, rows));
            if beyond {
                has_more = true;
                break;
            }
        }
    } else {
        let (start, before) = match &cursor.before_cursor {
            Some(text) => {
                let (index, event) = position(text, &members)?;
                (index, Some(event))
            }
            None => (count - 1, None),
        };
        let mut chunks = vec![];
        let mut taken = 0;
        for index in (0..=start).rev() {
            let before = if index == start { before } else { None };
            if taken == limit {
                if !select(index, None, before, 1, false)?.0.is_empty() {
                    has_earlier = true;
                    break;
                }
                continue;
            }
            let (rows, beyond) = select(index, None, before, limit - taken, true)?;
            taken += rows.len();
            chunks.push((index, rows));
            if beyond {
                has_earlier = true;
                break;
            }
        }
        for (index, rows) in chunks.into_iter().rev() {
            events.extend(tag(index, rows));
        }
    }
    let cursor = match (has_more, events.last()) {
        (true, Some(last)) => format!("{}:{}", last.session_id, last.event.event_id),
        _ => end()?,
    };
    Ok(MergedPage {
        account: account.into(),
        segments,
        events,
        cursor,
        has_earlier,
        has_more,
        indexing,
    })
}
