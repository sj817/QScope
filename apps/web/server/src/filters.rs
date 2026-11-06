use crate::contract::{ApiError, EventSummary};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct Filter {
    pub after: Option<u64>,
    pub before: Option<u64>,
    pub q: Option<String>,
    pub direction: Option<String>,
    pub controls: Option<bool>,
    pub limit: Option<usize>,
    pub include: Option<String>,
    pub exclude: Option<String>,
    pub kinds: Option<String>,
    pub min_size: Option<usize>,
    pub max_size: Option<usize>,
    pub time_from: Option<u64>,
    pub time_to: Option<u64>,
    pub event_from: Option<u64>,
    pub event_to: Option<u64>,
    pub seq: Option<u64>,
    pub tid: Option<u32>,
    pub error_only: Option<bool>,
    pub truncated_only: Option<bool>,
    pub hide_heartbeat: Option<bool>,
    pub hide_login: Option<bool>,
}

fn rules(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

// A bounded command matcher, no regex execution: '=' marks an exact name, '*' and '?'
// make a whole-name glob, and anything else matches as a substring.
fn matches(pattern: &str, text: &str) -> bool {
    if let Some(exact) = pattern.strip_prefix('=') {
        return text == exact;
    }
    if !pattern.contains(['*', '?']) {
        return text.contains(pattern);
    }
    let (pattern, text) = (pattern.as_bytes(), text.as_bytes());
    let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = t;
        } else if let Some(at) = star {
            retry += 1;
            t = retry;
            p = at + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

impl Filter {
    pub fn validate(&self) -> Result<(), ApiError> {
        for value in [&self.include, &self.exclude].into_iter().flatten() {
            // Room for a baseline of background commands, each as an exact rule.
            if value.len() > 16 * 1024
                || rules(value).count() > 256
                || rules(value).any(|s| s.len() > 128)
            {
                return Err(ApiError::new(
                    "query",
                    "命令规则最多 256 条，每条 128 字节；用逗号或换行分隔",
                ));
            }
        }
        if self.kinds.as_ref().is_some_and(|s| {
            s.len() > 64 || rules(s).any(|v| !["packet", "status", "loss", "end"].contains(&v))
        }) {
            return Err(ApiError::new("query", "未知记录类型"));
        }
        if self.min_size.zip(self.max_size).is_some_and(|(a, b)| a > b)
            || self.time_from.zip(self.time_to).is_some_and(|(a, b)| a > b)
            || self
                .event_from
                .zip(self.event_to)
                .is_some_and(|(a, b)| a > b)
        {
            return Err(ApiError::new("query", "筛选范围的起始值不能大于结束值"));
        }
        Ok(())
    }
    pub fn accepts(&self, e: &EventSummary) -> bool {
        let command = e.command.to_lowercase();
        let any = |value: &Option<String>| {
            value
                .as_ref()
                .is_some_and(|s| rules(s).any(|p| matches(&p.to_lowercase(), &command)))
        };
        let number = |value: &str| value.parse::<u64>().unwrap_or(0);
        (!self
            .include
            .as_ref()
            .is_some_and(|s| rules(s).next().is_some())
            || any(&self.include))
            && !any(&self.exclude)
            && self
                .kinds
                .as_ref()
                .is_none_or(|s| rules(s).any(|k| k == e.kind))
            && self.min_size.is_none_or(|n| e.captured_length >= n)
            && self.max_size.is_none_or(|n| e.captured_length <= n)
            && self
                .time_from
                .is_none_or(|n| number(&e.timestamp_filetime) >= n)
            && self
                .time_to
                .is_none_or(|n| number(&e.timestamp_filetime) <= n)
            && self.event_from.is_none_or(|n| number(&e.event_id) >= n)
            && self.event_to.is_none_or(|n| number(&e.event_id) <= n)
            && self.seq.is_none_or(|n| number(&e.sequence) == n)
            && self.tid.is_none_or(|n| e.tid == n)
            && (!self.error_only.unwrap_or(false) || e.error_code != 0)
            && (!self.truncated_only.unwrap_or(false) || e.truncated)
            && (!self.hide_heartbeat.unwrap_or(false)
                || !["heartbeat", "heart_beat", "keepalive", "statSvc.simpleget"]
                    .iter()
                    .any(|s| command.contains(&s.to_lowercase())))
            && (!self.hide_login.unwrap_or(false)
                || !["wtlogin.", "login", "registerproxy"]
                    .iter()
                    .any(|s| command.contains(s)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_command_globs_and_invalid_ranges() {
        assert!(matches("oidb*0x?de*", "oidbsvctrpc.0xcde_2"));
        assert!(matches("trpc", "test.trpc.command"));
        assert!(!matches("test.*", "other.command"));
        assert!(matches("=test.command.1", "test.command.1"));
        assert!(!matches("=test.command.1", "test.command.10"));
        assert!(matches("test.command.1", "test.command.10"));
        assert!(
            Filter {
                min_size: Some(5),
                max_size: Some(4),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Filter {
                kinds: Some("packet,invalid".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
