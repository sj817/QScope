use crate::{Error, client, display_path};
use qscope_core::data::sha256;
use serde::{Deserialize, Serialize};
use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
};
use ts_rs::TS;

const TEMPLATE: &str = include_str!("../skills/qscope/SKILL.md");
const MARKER: &str = "\n<!-- qscope-managed ";
const MAX_SKILL_BYTES: u64 = 256 * 1024;

#[derive(Deserialize)]
struct SkillMetadata {
    version: String,
}

fn skill_version() -> Result<String, Error> {
    Ok(serde_json::from_str::<SkillMetadata>(include_str!("../skills/qscope/skill.json"))?.version)
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(rename = "SkillAgent")]
pub enum Agent {
    Codex,
    ClaudeCode,
    Cursor,
}

impl Agent {
    pub const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Cursor];
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
        }
    }
    fn directory(self) -> &'static str {
        match self {
            Self::Codex => ".agents",
            Self::ClaudeCode => ".claude",
            Self::Cursor => ".cursor",
        }
    }
}

impl FromStr for Agent {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude-code" => Ok(Self::ClaudeCode),
            "cursor" => Ok(Self::Cursor),
            _ => Err("Choose codex, claude-code or cursor".into()),
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
        })
    }
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename = "SkillScope")]
pub enum Scope {
    #[default]
    Project,
    User,
}

impl FromStr for Scope {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "project" => Ok(Self::Project),
            "user" => Ok(Self::User),
            _ => Err("Choose project or user".into()),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Project => "project",
            Self::User => "user",
        })
    }
}

#[derive(Debug, Deserialize, TS)]
pub struct SkillRequest {
    pub agent: Agent,
    #[serde(default)]
    pub scope: Scope,
    pub project: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct SkillStatus {
    pub agent: Agent,
    pub name: String,
    pub scope: Scope,
    pub path: String,
    pub state: String,
    pub detail: String,
    pub permissions_path: Option<String>,
    pub permissions_ready: bool,
    pub version: String,
    pub installed_version: Option<String>,
    pub template_sha256: String,
    pub installed_template_sha256: Option<String>,
    pub content_sha256: Option<String>,
    pub installed_content_sha256: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct IntegrationSettings {
    pub workspace: String,
    pub target_directory: String,
    pub scope: Scope,
    pub home_directory: String,
    pub cli_path: String,
    pub cli_available: bool,
    pub server_url: String,
    pub targets: Vec<SkillStatus>,
    pub install_prompt: String,
    pub devtools_endpoint: String,
    pub research_launcher: Option<String>,
}

#[derive(Clone)]
pub struct Environment {
    pub workspace: PathBuf,
    pub home: PathBuf,
    pub cli: PathBuf,
    pub server_url: String,
}

#[derive(Deserialize, Serialize)]
struct Ownership {
    schema: u32,
    body_sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    permissions_allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_permissions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    template_sha256: Option<String>,
}

impl Ownership {
    fn checksum(&self, body: &str) -> Result<String, Error> {
        Ok(sha256(&serde_json::to_vec(&(
            body,
            &self.permissions_allow,
            self.legacy_permissions.unwrap_or(false),
            &self.version,
            &self.template_sha256,
        ))?))
    }
}

pub fn user_home() -> Result<PathBuf, Error> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key).map(PathBuf::from).ok_or_else(|| {
        Error::new(
            "home_directory",
            "Cannot locate the current user's home directory",
        )
    })
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn checked_root(path: &Path) -> Result<PathBuf, Error> {
    if path.to_string_lossy().contains(['\n', '\r', '\0']) {
        return Err(Error::new("install_path", "Invalid project path"));
    }
    let root = path.canonicalize()?;
    if !root.is_dir() || root.parent().is_none() {
        return Err(Error::new(
            "install_path",
            "Select an existing project or home directory",
        ));
    }
    Ok(root)
}

fn skill_path(root: &Path, agent: Agent, create: bool) -> Result<PathBuf, Error> {
    let mut path = root.to_path_buf();
    for part in [agent.directory(), "skills", "qscope"] {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if is_link(&metadata) || !metadata.is_dir() => {
                return Err(Error::new(
                    "install_path",
                    format!("Expected a regular directory at {}", path.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if create {
                    match fs::create_dir(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            let metadata = fs::symlink_metadata(&path)?;
                            if is_link(&metadata) || !metadata.is_dir() {
                                return Err(Error::new(
                                    "install_path",
                                    "Installation directory changed",
                                ));
                            }
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    path.push("SKILL.md");
    Ok(path)
}

fn read_file(path: &Path) -> Result<Option<String>, Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if is_link(&metadata) || !metadata.is_file() || metadata.len() > MAX_SKILL_BYTES {
        return Err(Error::new(
            "skill_conflict",
            "Existing integration file must be a regular bounded file",
        ));
    }
    let mut text = String::new();
    fs::File::open(path)?
        .take(MAX_SKILL_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_SKILL_BYTES {
        return Err(Error::new(
            "skill_conflict",
            "Existing integration file is too large",
        ));
    }
    Ok(Some(text))
}

fn publish_file(path: &Path, previous: Option<&str>, content: &str) -> Result<(), Error> {
    if content.len() as u64 > MAX_SKILL_BYTES {
        return Err(Error::new(
            "integration_limit",
            "Generated integration file exceeds its size limit",
        ));
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().expect("integration parent"))?;
    temporary.write_all(content.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    if read_file(path)?.as_deref() != previous {
        return Err(Error::new(
            "skill_changed",
            "Integration file changed during installation. Reload its status and retry",
        ));
    }
    if previous.is_none() {
        temporary
            .persist_noclobber(path)
            .map_err(|error| Error::from(error.error))?;
    } else {
        temporary
            .persist(path)
            .map_err(|error| Error::from(error.error))?;
    }
    Ok(())
}

struct Permissions {
    path: PathBuf,
    previous: Option<String>,
    content: String,
    ready: bool,
    added: Vec<String>,
}

impl Permissions {
    fn publish(&self) -> Result<(), Error> {
        if !self.ready {
            publish_file(&self.path, self.previous.as_deref(), &self.content)?;
        }
        Ok(())
    }

    fn rollback(&self) -> Result<(), Error> {
        if self.ready {
            return Ok(());
        }
        if let Some(previous) = &self.previous {
            publish_file(&self.path, Some(&self.content), previous)
        } else if read_file(&self.path)?.as_deref() == Some(&self.content) {
            fs::remove_file(&self.path).map_err(Into::into)
        } else {
            Err(Error::new(
                "install_partial",
                "Claude settings changed before installation rollback",
            ))
        }
    }
}

fn ownership(text: &str) -> Option<Ownership> {
    let (body, marker) = text.rsplit_once(MARKER)?;
    let json = marker.strip_suffix(" -->\n")?;
    let ownership: Ownership = serde_json::from_str(json).ok()?;
    if ownership.body_sha256 != sha256(body.as_bytes()) || ownership.permissions_allow.len() > 512 {
        return None;
    }
    match ownership.schema {
        1 if ownership.permissions_allow.is_empty()
            && ownership.metadata_sha256.is_none()
            && ownership.legacy_permissions.is_none()
            && ownership.version.is_none()
            && ownership.template_sha256.is_none() =>
        {
            Some(ownership)
        }
        2 if ownership.version.is_none() && ownership.template_sha256.is_none() => {
            let metadata = serde_json::to_vec(&(
                body,
                &ownership.permissions_allow,
                ownership.legacy_permissions.unwrap_or(false),
            ))
            .ok()?;
            (ownership.metadata_sha256.as_deref() == Some(sha256(&metadata).as_str()))
                .then_some(ownership)
        }
        3 if ownership
            .version
            .as_ref()
            .is_some_and(|version| !version.is_empty())
            && ownership.template_sha256.as_ref().is_some_and(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) =>
        {
            (ownership.metadata_sha256.as_deref() == Some(ownership.checksum(body).ok()?.as_str()))
                .then_some(ownership)
        }
        _ => None,
    }
}

fn installation_lock(root: &Path, agent: Agent) -> Result<fs::File, Error> {
    let path = root.join(agent.directory()).join(".qscope-install.lock");
    read_file(&path)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock().map_err(|error| {
        Error::new(
            "install_busy",
            format!("{} integration is busy: {error}", agent.label()),
        )
    })?;
    Ok(file)
}

impl Environment {
    fn permissions(
        &self,
        root: &Path,
        scope: Scope,
        removal: Option<&[String]>,
    ) -> Result<Permissions, Error> {
        let path = root.join(".claude").join(match scope {
            Scope::Project => "settings.local.json",
            Scope::User => "settings.json",
        });
        let previous = read_file(&path)?;
        if removal.is_some() && previous.is_none() {
            return Ok(Permissions {
                path,
                previous,
                content: String::new(),
                ready: true,
                added: Vec::new(),
            });
        }
        let mut settings: serde_json::Value = match &previous {
            Some(text) => {
                serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|error| {
                    Error::new(
                        "permissions_config",
                        format!(
                            "Invalid Claude settings at {}: {error}",
                            display_path(&path)
                        ),
                    )
                })?
            }
            None => serde_json::json!({}),
        };
        let config_error = || {
            Error::new(
                "permissions_config",
                format!(
                    "Claude settings at {} must contain an object and a permissions.allow string array",
                    display_path(&path)
                ),
            )
        };
        let object = settings.as_object_mut().ok_or_else(config_error)?;
        let permissions = object
            .entry("permissions")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(config_error)?;
        let allow = permissions
            .entry("allow")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(config_error)?;
        if allow.iter().any(|value| !value.is_string()) {
            return Err(config_error());
        }
        if let Some(removal) = removal {
            let before = allow.len();
            allow.retain(|rule| !removal.iter().any(|owned| rule.as_str() == Some(owned)));
            let ready = before == allow.len();
            let content = if ready {
                previous.clone().expect("existing settings")
            } else {
                format!("{}\n", serde_json::to_string_pretty(&settings)?)
            };
            return Ok(Permissions {
                path,
                previous,
                content,
                ready,
                added: Vec::new(),
            });
        }
        let native = display_path(&self.cli.canonicalize()?);
        if native.contains(['*', '?', '"', '\n', '\r']) {
            return Err(Error::new(
                "permissions_path",
                "CLI path contains permission rule metacharacters",
            ));
        }
        let mut paths = vec![native.replace('\\', "/"), native];
        #[cfg(windows)]
        if let Some(path) = paths[0].get(1..).filter(|suffix| suffix.starts_with(":/")) {
            paths.push(format!("/{}{}", paths[0][..1].to_lowercase(), &path[1..]));
        }
        paths.sort();
        paths.dedup();
        let mut rules = Vec::new();
        for path in paths {
            for quoted in [
                format!("\"{path}\""),
                format!("'{}'", path.replace('\'', "''")),
            ] {
                rules.push(format!("Bash({quoted} *)"));
                rules.push(format!("PowerShell({quoted} *)"));
                rules.push(format!("PowerShell(& {quoted} *)"));
            }
            if !path.contains([' ', '\t', '\'']) {
                rules.push(format!("Bash({path} *)"));
                rules.push(format!("PowerShell({path} *)"));
                rules.push(format!("PowerShell(& {path} *)"));
            }
        }
        let ready = rules
            .iter()
            .all(|rule| allow.iter().any(|value| value.as_str() == Some(rule)));
        let mut added = Vec::new();
        for rule in rules {
            if !allow.iter().any(|value| value.as_str() == Some(&rule)) {
                allow.push(serde_json::json!(&rule));
                added.push(rule);
            }
        }
        let content = if ready {
            previous.clone().expect("existing permissions")
        } else {
            format!("{}\n", serde_json::to_string_pretty(&settings)?)
        };
        Ok(Permissions {
            path,
            previous,
            content,
            ready,
            added,
        })
    }

    fn root(&self, scope: Scope, project: Option<&str>) -> Result<PathBuf, Error> {
        let path = match scope {
            Scope::Project => project.map(Path::new).unwrap_or(&self.workspace),
            Scope::User => &self.home,
        };
        checked_root(path)
    }

    fn rendered(&self) -> Result<String, Error> {
        let server =
            if self.server_url == client::DEFAULT_SERVER || self.server_url.starts_with('@') {
                self.server_url.clone()
            } else {
                client::loopback_url(&self.server_url)?
                    .as_str()
                    .trim_end_matches('/')
                    .into()
            };
        let cli = self.cli.canonicalize()?;
        let body = TEMPLATE
            .replace("{{SKILL_VERSION}}", &skill_version()?)
            .replace("{{CLI_PATH}}", &serde_json::to_string(&display_path(&cli))?)
            .replace(
                "{{POWERSHELL_CLI}}",
                &format!("& '{}'", display_path(&cli).replace('\'', "''")),
            )
            .replace(
                "{{BASH_CLI}}",
                &format!(
                    "'{}'",
                    display_path(&cli).replace('\\', "/").replace('\'', "'\\''")
                ),
            )
            .replace("{{SERVER_URL}}", &server)
            .replace(
                "{{POWERSHELL_SERVER}}",
                &format!("'{}'", server.replace('\'', "''")),
            )
            .replace(
                "{{BASH_SERVER}}",
                &format!("'{}'", server.replace('\\', "/").replace('\'', "'\\''")),
            )
            .replace("{{DEVTOOLS_URL}}", client::DEFAULT_DEVTOOLS);
        let mut ownership = Ownership {
            schema: 3,
            body_sha256: sha256(body.as_bytes()),
            permissions_allow: Vec::new(),
            metadata_sha256: None,
            legacy_permissions: None,
            version: Some(skill_version()?),
            template_sha256: Some(sha256(TEMPLATE.as_bytes())),
        };
        ownership.metadata_sha256 = Some(ownership.checksum(&body)?);
        Ok(format!(
            "{body}{MARKER}{} -->\n",
            serde_json::to_string(&ownership)?
        ))
    }

    pub fn inspect(
        &self,
        agent: Agent,
        scope: Scope,
        project: Option<&str>,
    ) -> Result<SkillStatus, Error> {
        let root = self.root(scope, project)?;
        let path = skill_path(&root, agent, false)?;
        let previous = read_file(&path)?;
        let owner = previous.as_deref().and_then(ownership);
        let version = skill_version()?;
        let template_sha256 = sha256(TEMPLATE.as_bytes());
        let expected = self.cli.is_file().then(|| self.rendered()).transpose()?;
        let content_sha256 = expected
            .as_deref()
            .and_then(ownership)
            .map(|owner| owner.body_sha256);
        let installed_content_sha256 = previous.as_deref().map(|text| {
            sha256(
                text.rsplit_once(MARKER)
                    .map_or(text, |value| value.0)
                    .as_bytes(),
            )
        });
        let (mut state, mut detail) = match previous {
            None => ("missing", "尚未安装"),
            Some(ref text) if ownership(text).is_none() => {
                ("conflict", "检测到已有或手动修改的技能，请保留后自行处理")
            }
            Some(ref text)
                if owner.as_ref().is_some_and(|owner| {
                    owner.version.as_deref() == Some(version.as_str())
                        && owner.template_sha256.as_deref() == Some(template_sha256.as_str())
                }) && expected.as_ref().is_some_and(|expected| {
                    text.rsplit_once(MARKER).map(|value| value.0)
                        == expected.rsplit_once(MARKER).map(|value| value.0)
                }) =>
            {
                ("installed", "已安装当前版本")
            }
            Some(_) => ("update_available", "可更新 QScope 生成的技能"),
        };
        let mut permissions_path = None;
        let mut permissions_ready = true;
        if matches!(agent, Agent::ClaudeCode) {
            match self.permissions(&root, scope, None) {
                Ok(permissions) => {
                    permissions_path = Some(display_path(&permissions.path));
                    permissions_ready = permissions.ready;
                    if state == "installed" && !permissions.ready {
                        state = "update_available";
                        detail = "需要补齐 Claude Code CLI 权限";
                    }
                }
                Err(error) => {
                    return Ok(SkillStatus {
                        agent,
                        name: agent.label().into(),
                        scope,
                        path: display_path(&path),
                        state: "conflict".into(),
                        detail: error.message,
                        permissions_path: None,
                        permissions_ready: false,
                        version,
                        installed_version: owner.as_ref().and_then(|owner| owner.version.clone()),
                        template_sha256,
                        installed_template_sha256: owner
                            .as_ref()
                            .and_then(|owner| owner.template_sha256.clone()),
                        content_sha256,
                        installed_content_sha256,
                    });
                }
            }
        }
        Ok(SkillStatus {
            agent,
            name: agent.label().into(),
            scope,
            path: display_path(&path),
            state: state.into(),
            detail: detail.into(),
            permissions_path,
            permissions_ready,
            version,
            installed_version: owner.as_ref().and_then(|owner| owner.version.clone()),
            template_sha256,
            installed_template_sha256: owner
                .as_ref()
                .and_then(|owner| owner.template_sha256.clone()),
            content_sha256,
            installed_content_sha256,
        })
    }

    pub fn install(&self, request: &SkillRequest) -> Result<SkillStatus, Error> {
        if !self.cli.is_file() {
            return Err(Error::new(
                "cli_missing",
                "Build qscope-cli before installing its skill",
            ));
        }
        let root = self.root(request.scope, request.project.as_deref())?;
        let target = skill_path(&root, request.agent, false)?;
        let previous = read_file(&target)?;
        if previous
            .as_deref()
            .is_some_and(|text| ownership(text).is_none())
        {
            return Err(Error::new(
                "skill_conflict",
                "Existing skill has custom content. Preserve it before replacing it",
            ));
        }
        let mut content = self.rendered()?;
        let permissions = matches!(request.agent, Agent::ClaudeCode)
            .then(|| self.permissions(&root, request.scope, None))
            .transpose()?;
        if let Some(permissions) = &permissions {
            let previous_ownership = previous.as_deref().and_then(ownership);
            let legacy = previous_ownership
                .as_ref()
                .is_some_and(|owner| owner.schema == 1 || owner.legacy_permissions == Some(true));
            let mut rules = previous_ownership
                .map(|owner| owner.permissions_allow)
                .unwrap_or_default();
            rules.extend(permissions.added.iter().cloned());
            rules.sort();
            rules.dedup();
            if rules.len() > 512 {
                return Err(Error::new(
                    "permissions_limit",
                    "Managed CLI permission history exceeds its limit",
                ));
            }
            let body = content.rsplit_once(MARKER).expect("generated ownership").0;
            let mut owner = Ownership {
                schema: 3,
                body_sha256: sha256(body.as_bytes()),
                permissions_allow: rules.clone(),
                metadata_sha256: None,
                legacy_permissions: legacy.then_some(true),
                version: Some(skill_version()?),
                template_sha256: Some(sha256(TEMPLATE.as_bytes())),
            };
            owner.metadata_sha256 = Some(owner.checksum(body)?);
            content = format!("{body}{MARKER}{} -->\n", serde_json::to_string(&owner)?);
        }
        let target = skill_path(&root, request.agent, true)?;
        let _lock = installation_lock(&root, request.agent)?;
        if let Some(permissions) = &permissions {
            permissions.publish()?;
        }
        if previous.as_deref() != Some(content.as_str())
            && let Err(error) = publish_file(&target, previous.as_deref(), &content)
        {
            if let Some(permissions) = &permissions {
                permissions.rollback()?;
            }
            return Err(error);
        }
        self.inspect(request.agent, request.scope, request.project.as_deref())
    }

    pub fn uninstall(&self, request: &SkillRequest) -> Result<SkillStatus, Error> {
        let root = self.root(request.scope, request.project.as_deref())?;
        let target = skill_path(&root, request.agent, false)?;
        let Some(previous) = read_file(&target)? else {
            let mut status =
                self.inspect(request.agent, request.scope, request.project.as_deref())?;
            status.state = "missing".into();
            return Ok(status);
        };
        let owner = ownership(&previous).ok_or_else(|| {
            Error::new(
                "skill_conflict",
                "Existing skill has custom content and requires manual removal",
            )
        })?;
        let _lock = installation_lock(&root, request.agent)?;
        let permissions = matches!(request.agent, Agent::ClaudeCode)
            .then(|| self.permissions(&root, request.scope, Some(&owner.permissions_allow)))
            .transpose()?;
        if let Some(permissions) = &permissions {
            permissions.publish()?;
        }
        let removal = (|| {
            if read_file(&target)?.as_deref() != Some(&previous) {
                return Err(Error::new(
                    "skill_changed",
                    "Skill changed during removal. Reload its status and retry",
                ));
            }
            fs::remove_file(&target).map_err(Error::from)
        })();
        if let Err(error) = removal {
            if let Some(permissions) = &permissions {
                permissions.rollback()?;
            }
            return Err(error);
        }
        let _ = fs::remove_dir(target.parent().expect("skill directory"));
        let mut status = self.inspect(request.agent, request.scope, request.project.as_deref())?;
        status.state = "missing".into();
        status.detail = if matches!(request.agent, Agent::ClaudeCode)
            && (owner.schema == 1 || owner.legacy_permissions == Some(true))
        {
            "技能已卸载。旧版安装缺少权限归属记录，旧 CLI 白名单需手动移除"
        } else {
            "QScope 集成已卸载"
        }
        .into();
        Ok(status)
    }

    pub fn prompt(&self, scope: Scope, project: Option<&str>) -> Result<String, Error> {
        let root = self.root(scope, project)?;
        let cli = serde_json::to_string(&display_path(&self.cli))?;
        let project = serde_json::to_string(&display_path(&root))?;
        Ok(format!(
            "请为你当前使用的 AI 编程工具安装本机 QScope skill。QScope CLI 的绝对路径是 {cli}，工作台地址是 {}，安装范围是 {scope}，目标目录是 {project}。先用适合当前 shell 的绝对路径调用方式运行该 CLI 的 --help。识别你是 Claude Code、Codex 还是 Cursor，再执行 agent install --agent 对应值 --scope {scope} --project {project} --server {}，对应值分别为 claude-code、codex、cursor。完成后运行 agent status --scope {scope} --project {project} --server {}，核对返回的 SKILL.md 路径并读取它，遵守其中的金融相关任务禁令。其他兼容 Agent Skills 的工具可通过 agent skill --server {} 获取生成内容，安装到你自己的标准技能目录。最后告诉我安装路径、验证结果及是否需要重新加载当前 AI 会话。",
            self.server_url, self.server_url, self.server_url, self.server_url
        ))
    }

    pub fn settings(
        &self,
        scope: Scope,
        project: Option<&str>,
    ) -> Result<IntegrationSettings, Error> {
        Ok(IntegrationSettings {
            workspace: display_path(&self.workspace),
            target_directory: display_path(&self.root(scope, project)?),
            scope,
            home_directory: display_path(&self.home),
            cli_path: display_path(&self.cli),
            cli_available: self.cli.is_file(),
            server_url: self.server_url.clone(),
            targets: Agent::ALL
                .into_iter()
                .map(|agent| self.inspect(agent, scope, project))
                .collect::<Result<_, _>>()?,
            install_prompt: self.prompt(scope, project)?,
            devtools_endpoint: client::DEFAULT_DEVTOOLS.into(),
            research_launcher: crate::runtime::Layout::development(&self.workspace)
                .host()
                .ok()
                .map(|path| display_path(&path)),
        })
    }

    pub fn skill(&self) -> Result<String, Error> {
        self.rendered()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(root: &Path) -> Environment {
        let project = root.join("project with spaces");
        let home = root.join("user home");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&home).unwrap();
        Environment {
            workspace: project,
            home,
            cli: std::env::current_exe().unwrap(),
            server_url: client::DEFAULT_SERVER.into(),
        }
    }

    #[test]
    fn agent_installations_are_scoped_atomic_idempotent_and_preserve_custom_edits() {
        let directory = tempfile::tempdir().unwrap();
        let mut environment = environment(directory.path());
        for agent in Agent::ALL {
            let request = SkillRequest {
                agent,
                scope: Scope::Project,
                project: None,
            };
            let installed = environment.install(&request).unwrap();
            assert_eq!(installed.state, "installed");
            let original = fs::read(&installed.path).unwrap();
            let again = environment.install(&request).unwrap();
            assert_eq!(fs::read(&again.path).unwrap(), original);
            assert!(
                Path::new(&installed.path)
                    .canonicalize()
                    .unwrap()
                    .starts_with(environment.workspace.canonicalize().unwrap())
            );
            assert!(!environment.home.join(agent.directory()).exists());
        }
        environment.server_url = "http://127.0.0.1:19842".into();
        assert_eq!(
            environment
                .inspect(Agent::Codex, Scope::Project, None)
                .unwrap()
                .state,
            "update_available"
        );
        let request = SkillRequest {
            agent: Agent::Codex,
            scope: Scope::Project,
            project: None,
        };
        let installed = environment.install(&request).unwrap();
        let content = fs::read_to_string(&installed.path).unwrap();
        assert!(content.contains("http://127.0.0.1:19842"));
        let custom = content.replace("采集与定位", "用户维护的步骤");
        fs::write(&installed.path, &custom).unwrap();
        assert_eq!(
            environment
                .inspect(Agent::Codex, Scope::Project, None)
                .unwrap()
                .state,
            "conflict"
        );
        assert_eq!(
            environment.install(&request).unwrap_err().code,
            "skill_conflict"
        );
        assert_eq!(
            environment.uninstall(&request).unwrap_err().code,
            "skill_conflict"
        );
        assert_eq!(fs::read_to_string(&installed.path).unwrap(), custom);
        let files: Vec<_> = fs::read_dir(Path::new(&installed.path).parent().unwrap())
            .unwrap()
            .collect();
        assert_eq!(
            files.len(),
            1,
            "atomic installation leaves only its complete SKILL.md"
        );
    }

    #[test]
    fn user_scope_uses_its_own_home_and_rejects_non_directory_targets() {
        let directory = tempfile::tempdir().unwrap();
        let environment = environment(directory.path());
        let request = SkillRequest {
            agent: Agent::Cursor,
            scope: Scope::User,
            project: Some("ignored in user scope".into()),
        };
        let installed = environment.install(&request).unwrap();
        assert!(
            Path::new(&installed.path)
                .canonicalize()
                .unwrap()
                .starts_with(environment.home.canonicalize().unwrap())
        );
        assert!(!environment.workspace.join(".cursor").exists());
        let obstruction = environment.workspace.join(".claude");
        fs::write(&obstruction, "existing user file").unwrap();
        assert_eq!(
            environment
                .install(&SkillRequest {
                    agent: Agent::ClaudeCode,
                    scope: Scope::Project,
                    project: None
                })
                .unwrap_err()
                .code,
            "install_path"
        );
        assert_eq!(
            fs::read_to_string(obstruction).unwrap(),
            "existing user file"
        );
    }

    #[test]
    fn claude_permissions_merge_repair_and_keep_existing_settings() {
        let directory = tempfile::tempdir().unwrap();
        let mut environment = environment(directory.path());
        let cli_directory = directory.path().join("Program Files (x64)");
        fs::create_dir(&cli_directory).unwrap();
        environment.cli = cli_directory.join("qscope.exe");
        fs::write(&environment.cli, "synthetic CLI path").unwrap();
        let config_path = environment.workspace.join(".claude/settings.local.json");
        fs::create_dir(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, r#"{"env":{"KEEP":"test"},"permissions":{"deny":["Bash(rm *)"],"allow":["Bash(git status *)"]},"hooks":{"SessionStart":[]}}"#).unwrap();
        let request = SkillRequest {
            agent: Agent::ClaudeCode,
            scope: Scope::Project,
            project: None,
        };
        let installed = environment.install(&request).unwrap();
        assert!(installed.permissions_ready);
        assert_eq!(
            installed.permissions_path.as_deref(),
            Some(display_path(&config_path.canonicalize().unwrap()).as_str())
        );
        let content = fs::read_to_string(&config_path).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(config["env"]["KEEP"], "test");
        assert_eq!(config["permissions"]["deny"][0], "Bash(rm *)");
        assert!(config["hooks"]["SessionStart"].is_array());
        let rules = config["permissions"]["allow"].as_array().unwrap();
        assert!(
            rules
                .iter()
                .any(|rule| rule.as_str().unwrap().starts_with("PowerShell(& "))
        );
        assert!(rules.iter().skip(1).all(|rule| {
            rule.as_str()
                .unwrap()
                .contains(environment.cli.file_name().unwrap().to_str().unwrap())
        }));
        environment.install(&request).unwrap();
        assert_eq!(fs::read_to_string(&config_path).unwrap(), content);
        fs::write(&config_path, "{\"permissions\":{\"allow\":[]}}").unwrap();
        assert_eq!(
            environment
                .inspect(Agent::ClaudeCode, Scope::Project, None)
                .unwrap()
                .state,
            "update_available"
        );
        assert!(environment.install(&request).unwrap().permissions_ready);
        let skill = fs::read_to_string(installed.path).unwrap();
        assert!(skill.contains("任何金融相关任务直接拒绝"));
        assert!(
            environment
                .prompt(Scope::Project, None)
                .unwrap()
                .contains("金融相关任务禁令")
        );
    }

    #[test]
    fn claude_user_config_and_invalid_json_are_handled_before_skill_publication() {
        let directory = tempfile::tempdir().unwrap();
        let environment = environment(directory.path());
        let request = SkillRequest {
            agent: Agent::ClaudeCode,
            scope: Scope::User,
            project: None,
        };
        let installed = environment.install(&request).unwrap();
        assert_eq!(
            installed.permissions_path.unwrap(),
            display_path(
                &environment
                    .home
                    .join(".claude/settings.json")
                    .canonicalize()
                    .unwrap()
            )
        );
        assert!(!environment.workspace.join(".claude").exists());
        let config_path = environment.workspace.join(".claude/settings.local.json");
        fs::create_dir(config_path.parent().unwrap()).unwrap();
        for bad in [
            "{broken",
            "{\"permissions\":false}",
            "{\"permissions\":{\"allow\":[42]}}",
        ] {
            fs::write(&config_path, bad).unwrap();
            assert_eq!(
                environment
                    .install(&SkillRequest {
                        agent: Agent::ClaudeCode,
                        scope: Scope::Project,
                        project: None
                    })
                    .unwrap_err()
                    .code,
                "permissions_config"
            );
            assert_eq!(fs::read_to_string(&config_path).unwrap(), bad);
            assert!(
                !environment
                    .workspace
                    .join(".claude/skills/qscope/SKILL.md")
                    .exists()
            );
        }
    }

    #[test]
    fn uninstall_tracks_added_permissions_across_updates_and_preserves_existing_rules() {
        let directory = tempfile::tempdir().unwrap();
        let mut environment = environment(directory.path());
        let root = environment.workspace.clone();
        let existing_rule = environment
            .permissions(&root, Scope::Project, None)
            .unwrap()
            .added[0]
            .clone();
        let settings_path = root.join(".claude/settings.local.json");
        fs::create_dir(settings_path.parent().unwrap()).unwrap();
        let original = serde_json::json!({"env":{"KEEP":"yes"},"permissions":{"allow":[existing_rule,"Bash(git status *)"],"deny":["Bash(rm *)"]},"hooks":{"SessionStart":[]}});
        fs::write(&settings_path, serde_json::to_vec(&original).unwrap()).unwrap();
        let request = SkillRequest {
            agent: Agent::ClaudeCode,
            scope: Scope::Project,
            project: None,
        };
        let installed = environment.install(&request).unwrap();
        let before = fs::read(&installed.path).unwrap();
        environment.install(&request).unwrap();
        assert_eq!(fs::read(&installed.path).unwrap(), before);
        environment.cli = directory.path().join("updated qscope.exe");
        fs::write(&environment.cli, "synthetic CLI identity").unwrap();
        environment.install(&request).unwrap();
        let skill = fs::read_to_string(&installed.path).unwrap();
        let settings = fs::read_to_string(&settings_path).unwrap();
        fs::write(&settings_path, "{broken").unwrap();
        assert_eq!(
            environment.uninstall(&request).unwrap_err().code,
            "permissions_config"
        );
        assert_eq!(fs::read_to_string(&installed.path).unwrap(), skill);
        fs::write(&settings_path, settings).unwrap();
        fs::remove_file(&environment.cli).unwrap();
        assert_eq!(environment.uninstall(&request).unwrap().state, "missing");
        assert!(!Path::new(&installed.path).exists());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&settings_path).unwrap())
                .unwrap(),
            original
        );
        assert_eq!(environment.uninstall(&request).unwrap().state, "missing");
    }

    #[test]
    fn uninstall_handles_user_scope_and_legacy_permission_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let environment = environment(directory.path());
        for agent in Agent::ALL {
            let request = SkillRequest {
                agent,
                scope: Scope::User,
                project: None,
            };
            let installed = environment.install(&request).unwrap();
            assert_eq!(environment.uninstall(&request).unwrap().state, "missing");
            assert!(!Path::new(&installed.path).exists());
            assert!(!environment.workspace.join(agent.directory()).exists());
        }
        let request = SkillRequest {
            agent: Agent::ClaudeCode,
            scope: Scope::Project,
            project: None,
        };
        for update in [false, true] {
            let installed = environment.install(&request).unwrap();
            let rendered = environment.rendered().unwrap();
            let (body, _) = rendered.rsplit_once(MARKER).unwrap();
            let legacy = serde_json::json!({"schema":1,"body_sha256":sha256(body.as_bytes())});
            fs::write(&installed.path, format!("{body}{MARKER}{legacy} -->\n")).unwrap();
            if update {
                environment.install(&request).unwrap();
            }
            let config = installed.permissions_path.unwrap();
            let before = fs::read(&config).unwrap();
            let removed = environment.uninstall(&request).unwrap();
            assert!(removed.detail.contains("旧版安装缺少权限归属记录"));
            assert_eq!(fs::read(config).unwrap(), before);
        }
    }

    #[test]
    fn integration_size_limits_reject_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let oversized = "x".repeat(MAX_SKILL_BYTES as usize + 1);
        assert_eq!(
            publish_file(&path, None, &oversized).unwrap_err().code,
            "integration_limit"
        );
        assert!(!path.exists());
    }

    #[test]
    fn skill_version_and_hashes_detect_updates_and_preserve_custom_content() {
        let directory = tempfile::tempdir().unwrap();
        let environment = environment(directory.path());
        let request = SkillRequest {
            agent: Agent::Codex,
            scope: Scope::Project,
            project: None,
        };
        let installed = environment.install(&request).unwrap();
        assert_eq!(
            installed.installed_version.as_deref(),
            Some(installed.version.as_str())
        );
        assert_eq!(
            installed.template_sha256,
            installed.installed_template_sha256.as_deref().unwrap()
        );
        assert_eq!(installed.content_sha256, installed.installed_content_sha256);
        let rendered = fs::read_to_string(&installed.path).unwrap();
        let (body, _) = rendered.rsplit_once(MARKER).unwrap();
        let mut owner = ownership(&rendered).unwrap();
        owner.version = Some("0.0.1".into());
        owner.template_sha256 = Some("0".repeat(64));
        owner.metadata_sha256 = Some(owner.checksum(body).unwrap());
        fs::write(
            &installed.path,
            format!(
                "{body}{MARKER}{} -->\n",
                serde_json::to_string(&owner).unwrap()
            ),
        )
        .unwrap();
        let status = environment
            .inspect(Agent::Codex, Scope::Project, None)
            .unwrap();
        assert_eq!(status.state, "update_available");
        assert_eq!(status.installed_version.as_deref(), Some("0.0.1"));
        assert_eq!(environment.install(&request).unwrap().state, "installed");
        owner.version = Some(installed.version.clone());
        owner.metadata_sha256 = Some(owner.checksum(body).unwrap());
        fs::write(
            &installed.path,
            format!(
                "{body}{MARKER}{} -->\n",
                serde_json::to_string(&owner).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(
            environment
                .inspect(Agent::Codex, Scope::Project, None)
                .unwrap()
                .state,
            "update_available"
        );
        assert_eq!(environment.install(&request).unwrap().state, "installed");
        fs::write(&installed.path, format!("自定义内容{rendered}")).unwrap();
        assert_eq!(
            environment
                .inspect(Agent::Codex, Scope::Project, None)
                .unwrap()
                .state,
            "conflict"
        );
        assert_eq!(
            environment.install(&request).unwrap_err().code,
            "skill_conflict"
        );
    }

    #[test]
    fn schema_two_skills_upgrade_with_owned_permissions_intact() {
        let directory = tempfile::tempdir().unwrap();
        let environment = environment(directory.path());
        let request = SkillRequest {
            agent: Agent::ClaudeCode,
            scope: Scope::Project,
            project: None,
        };
        let installed = environment.install(&request).unwrap();
        let text = fs::read_to_string(&installed.path).unwrap();
        let (body, _) = text.rsplit_once(MARKER).unwrap();
        let mut owner = ownership(&text).unwrap();
        let rules = owner.permissions_allow.clone();
        owner.schema = 2;
        owner.version = None;
        owner.template_sha256 = None;
        owner.metadata_sha256 = Some(sha256(&serde_json::to_vec(&(body, &rules, false)).unwrap()));
        fs::write(
            &installed.path,
            format!(
                "{body}{MARKER}{} -->\n",
                serde_json::to_string(&owner).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(
            environment
                .inspect(Agent::ClaudeCode, Scope::Project, None)
                .unwrap()
                .state,
            "update_available"
        );
        let upgraded = environment.install(&request).unwrap();
        assert_eq!(upgraded.state, "installed");
        assert_eq!(
            ownership(&fs::read_to_string(&installed.path).unwrap())
                .unwrap()
                .permissions_allow,
            rules
        );
        environment.uninstall(&request).unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(installed.permissions_path.unwrap()).unwrap())
                .unwrap();
        assert_eq!(settings["permissions"]["allow"], serde_json::json!([]));
    }
}
