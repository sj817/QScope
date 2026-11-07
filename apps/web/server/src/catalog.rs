use crate::{
    capture::Capture,
    contract::{API_VERSION, ApiError, Catalog, Session},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub type SharedCapture = Arc<Mutex<Capture>>;
pub struct Library {
    pub roots: Vec<PathBuf>,
    captures: BTreeMap<String, SharedCapture>,
    paths: BTreeMap<PathBuf, String>,
    summaries: BTreeMap<String, Session>,
    discovering: bool,
    limited: bool,
    errors: Vec<String>,
    index: Option<crate::index::Store>,
}

impl Library {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            captures: BTreeMap::new(),
            paths: BTreeMap::new(),
            summaries: BTreeMap::new(),
            discovering: true,
            limited: false,
            errors: vec![],
            index: None,
        }
    }

    pub fn indexed(roots: Vec<PathBuf>, index: crate::index::Store) -> Self {
        Self {
            index: Some(index),
            ..Self::new(roots)
        }
    }

    pub fn insert(&mut self, mut capture: Capture) -> Result<SharedCapture, ApiError> {
        capture.attach_index(self.index.clone());
        let summary = capture.summary();
        if let Some(existing) = self.captures.get(&summary.id) {
            return Ok(existing.clone());
        }
        if self.captures.len() >= 512 {
            return Err(ApiError::new("file_limit", "工作区最多打开 512 份抓包"));
        }
        let id = summary.id.clone();
        self.paths.insert(PathBuf::from(&summary.path), id.clone());
        self.summaries.insert(id.clone(), summary);
        let shared = Arc::new(Mutex::new(capture));
        self.captures.insert(id, shared.clone());
        Ok(shared)
    }

    pub fn check_capacity(&self, files: &[Capture]) -> Result<(), ApiError> {
        let additions: BTreeSet<_> = files
            .iter()
            .map(|capture| capture.summary().id)
            .filter(|id| !self.captures.contains_key(id))
            .collect();
        if self.captures.len() + additions.len() > 512 {
            return Err(ApiError::new("file_limit", "工作区最多打开 512 份抓包"));
        }
        Ok(())
    }

    fn scanner(&self) -> Scanner {
        Scanner {
            roots: self.roots.clone(),
            known: self.paths.keys().cloned().collect(),
            remaining: 10_000,
            limited: false,
            errors: vec![],
        }
    }

    fn finish_discovery(&mut self, scanner: Scanner) {
        self.limited = scanner.limited;
        self.errors = scanner.errors;
        self.discovering = false;
    }

    #[cfg(test)]
    pub fn discover(&mut self) {
        let mut scanner = self.scanner();
        scanner
            .run(&mut |capture| self.insert(capture).map(|_| ()))
            .unwrap();
        self.finish_discovery(scanner);
    }

    pub fn get(&self, id: &str) -> Result<SharedCapture, ApiError> {
        self.captures
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::new("not_found", "采集会话不存在"))
    }

    pub fn snapshot(&mut self) -> Catalog {
        // 繁忙抓包沿用最近元数据，目录查询按需复用索引并避开抓包 I/O 锁。
        for (id, capture) in &self.captures {
            if let Ok(capture) = capture.try_lock() {
                self.summaries.insert(id.clone(), capture.summary());
            }
        }
        let mut sessions: Vec<_> = self.summaries.values().cloned().collect();
        sessions.sort_by(|a, b| {
            b.started_filetime
                .parse::<u64>()
                .expect("native timestamp")
                .cmp(&a.started_filetime.parse::<u64>().expect("native timestamp"))
                .then_with(|| b.id.cmp(&a.id))
        });
        Catalog {
            api_version: API_VERSION,
            sessions,
            discovering: self.discovering,
            discovery_limited: self.limited,
            discovery_errors: self.errors.clone(),
        }
    }
}

impl Library {
    /// 按启动时间列出最近观察到同一账号的会话，excluded 下的导入、录制和代理输出独立展示。
    pub fn members(&mut self, account: &str, excluded: &Path) -> Vec<(Session, SharedCapture)> {
        let mut members: Vec<_> = self
            .snapshot()
            .sessions
            .into_iter()
            .filter(|s| {
                s.account.as_deref() == Some(account) && !Path::new(&s.path).starts_with(excluded)
            })
            .filter_map(|s| self.captures.get(&s.id).cloned().map(|c| (s, c)))
            .collect();
        members.reverse();
        let skip = members.len().saturating_sub(MAX_MEMBERS);
        members.drain(..skip);
        members
    }
}

const MAX_MEMBERS: usize = 64;

/// 在库锁外对未索引文件执行有界账号前瞻，繁忙抓包留待下轮处理。
pub fn scan_accounts_shared(library: &Mutex<Library>, budget: usize) -> Result<(), ApiError> {
    let captures: Vec<_> = library
        .lock()
        .map_err(|_| crate::lock_error())?
        .captures
        .iter()
        .map(|(id, capture)| (id.clone(), capture.clone()))
        .collect();
    let mut remaining = budget;
    for (id, capture) in captures {
        if remaining == 0 {
            break;
        }
        let Ok(mut capture) = capture.try_lock() else {
            continue;
        };
        if capture.scan_account() {
            remaining -= 1;
            let summary = capture.summary();
            drop(capture);
            library
                .lock()
                .map_err(|_| crate::lock_error())?
                .summaries
                .insert(id, summary);
        }
    }
    Ok(())
}

/// 在库锁外遍历并及时发布有效头部，已打开记录可独立于其他配置和缓存目录访问。
pub fn discover_shared(library: &Mutex<Library>) -> Result<(), ApiError> {
    let mut scanner = {
        let mut library = library.lock().map_err(|_| crate::lock_error())?;
        library.discovering = true;
        library.scanner()
    };
    scanner.run(&mut |capture| {
        library
            .lock()
            .map_err(|_| crate::lock_error())?
            .insert(capture)
            .map(|_| ())
    })?;
    library
        .lock()
        .map_err(|_| crate::lock_error())?
        .finish_discovery(scanner);
    Ok(())
}

struct Scanner {
    roots: Vec<PathBuf>,
    known: BTreeSet<PathBuf>,
    remaining: usize,
    limited: bool,
    errors: Vec<String>,
}

impl Scanner {
    fn run(
        &mut self,
        publish: &mut impl FnMut(Capture) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        for root in self.roots.clone() {
            self.walk(&root, &root, 0, publish)?;
        }
        Ok(())
    }

    fn walk(
        &mut self,
        root: &Path,
        path: &Path,
        depth: usize,
        publish: &mut impl FnMut(Capture) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        if self.remaining == 0 || depth > 12 || self.known.len() >= 512 {
            self.limited = true;
            return Ok(());
        }
        self.remaining -= 1;
        let Ok(meta) = fs::symlink_metadata(path) else {
            return Ok(());
        };
        if meta.is_symlink() {
            return Ok(());
        }
        let Ok(canonical) = path.canonicalize() else {
            return Ok(());
        };
        if !canonical.starts_with(root) {
            return Ok(());
        }
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&canonical) {
                let mut entries: Vec<_> = entries.flatten().take(self.remaining + 1).collect();
                // 优先遍历研究抓包目录，再遍历 Electron 缓存。
                entries.sort_by_key(|entry| (entry.file_name() != ".qscope", entry.file_name()));
                for entry in entries {
                    self.walk(root, &entry.path(), depth + 1, publish)?;
                    if self.remaining == 0 {
                        self.limited = true;
                        break;
                    }
                }
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("qscap"))
            && !self.known.contains(&canonical)
        {
            match Capture::open(canonical.clone()) {
                Ok(capture) => {
                    publish(capture)?;
                    self.known.insert(canonical);
                }
                Err(error) if self.errors.len() < 16 => self.errors.push(format!(
                    "{}: {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    error.message
                )),
                Err(_) => {}
            }
        }
        Ok(())
    }
}
