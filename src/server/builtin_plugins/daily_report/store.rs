//! Local state for the daily-report plugin: the login session, the writing
//! guide and the draft under review. The directory belongs to this plugin
//! alone, so no workspace, tool result or browser response reads it directly.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Owner-only directory: the session file next to it holds a bearer token.
const DIR_MODE: u32 = 0o700;
/// Owner-only file, set after every write so a looser umask cannot widen it.
const FILE_MODE: u32 = 0o600;
const SESSION_FILE: &str = "session.json";
const DRAFT_FILE: &str = "draft.json";
const GUIDE_FILE: &str = "guide.md";

/// A logged-in company account. The access token never leaves the host.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct Session {
    pub base_url: String,
    pub client_id: String,
    pub tenant_id: String,
    pub access_token: String,
    pub user_id: String,
    pub user_name: String,
    pub nick_name: String,
    pub updated_at: String,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            client_id: String::new(),
            tenant_id: "000000".to_owned(),
            access_token: String::new(),
            user_id: String::new(),
            user_name: String::new(),
            nick_name: String::new(),
            updated_at: String::new(),
        }
    }
}

impl Session {
    /// What the page and the AI tools may see: everything except the token.
    pub(super) fn masked(&self) -> Value {
        json!({
            "logged_in": !self.access_token.is_empty(),
            "base_url": self.base_url,
            "user_id": self.user_id,
            "user_name": self.user_name,
            "nick_name": self.nick_name,
            "updated_at": self.updated_at,
        })
    }
}

/// One project's part of a daily report.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct DraftProject {
    pub project_id: Option<i64>,
    pub project_name: String,
    pub work_content: String,
}

/// The report waiting for review. It is a plugin file, not a company record.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct Draft {
    pub report_date: String,
    pub projects: Vec<DraftProject>,
    pub problems: String,
    pub remark: String,
    pub overtime_hours: f64,
    pub leave_hours: f64,
    pub reimbursement: f64,
    pub reimbursement_reason: String,
    /// `create` or `update`; an update carries the server row id.
    pub action: String,
    pub report_id: Option<String>,
    /// `ai` when a conversation produced it, `manual` when the page did.
    pub origin: String,
    pub updated_at: String,
}

impl Draft {
    /// The `WrDailyBo` body the company system expects, plus the row id when
    /// this draft updates an existing report.
    pub(super) fn payload(&self, session: &Session) -> Value {
        let (year, month) = match self.report_date.split_once('-') {
            Some((year, rest)) => (year.to_owned(), rest.split('-').next().unwrap_or_default().to_owned()),
            None => (String::new(), String::new()),
        };
        let projects = self
            .projects
            .iter()
            .map(|project| {
                json!({
                    "projectId": project.project_id,
                    "projectName": project.project_name,
                    "workContent": project.work_content,
                })
            })
            .collect::<Vec<_>>();
        let mut body = json!({
            "reportDate": self.report_date,
            "reportYear": year,
            "reportMonth": month,
            "userName": session.nick_name.is_empty().then(|| session.user_name.clone()).unwrap_or_else(|| session.nick_name.clone()),
            "userId": session.user_id,
            "problems": self.problems,
            "remark": self.remark,
            "overtimeHours": self.overtime_hours,
            "leaveHours": self.leave_hours,
            "reimbursement": self.reimbursement,
            "reimbursementReason": self.reimbursement_reason,
            "projects": projects,
        });
        if let Some(id) = self.report_id.as_ref().filter(|id| !id.is_empty()) {
            if let Some(object) = body.as_object_mut() {
                object.insert("id".into(), json!(id));
            }
        }
        body
    }
}

fn write_private(path: &Path, body: &str) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(FILE_MODE)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(FILE_MODE))?;
    }
    Ok(())
}

fn read_or_none(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(body) => Ok(Some(body)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn parse<T: for<'a> Deserialize<'a>>(path: &Path, body: Option<String>) -> Result<Option<T>> {
    match body.map(|body| body.trim().to_owned()) {
        None => Ok(None),
        Some(body) if body.is_empty() => Ok(None),
        Some(body) => serde_json::from_str(&body)
            .map(Some)
            .with_context(|| format!("{} is not valid JSON", path.display())),
    }
}

/// The plugin's own directory, created on first use.
pub(super) struct Store {
    root: PathBuf,
}

impl Store {
    /// `DAILY_REPORT_DATA_DIR` wins, so tests and a portable install stay out
    /// of the user's real configuration.
    pub(super) fn data_dir() -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os("DAILY_REPORT_DATA_DIR").filter(|dir| !dir.is_empty()) {
            return Ok(PathBuf::from(dir));
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("APPDATA").filter(|dir| !dir.is_empty()).map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|dir| !dir.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
            .context("no configuration directory for the daily-report session")?;
        Ok(base.join("grow-daily-report"))
    }

    pub(super) fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { root: root.into() };
        std::fs::create_dir_all(&store.root)
            .with_context(|| format!("creating {}", store.root.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&store.root, std::fs::Permissions::from_mode(DIR_MODE))?;
        }
        // The guide exists from the first visit, so the markdown plugin and the
        // AI both have a real file to read.
        if !store.guide_path().exists() {
            write_private(&store.guide_path(), "")?;
        }
        Ok(store)
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn guide_path(&self) -> PathBuf {
        self.root.join(GUIDE_FILE)
    }

    fn session_path(&self) -> PathBuf {
        self.root.join(SESSION_FILE)
    }

    fn draft_path(&self) -> PathBuf {
        self.root.join(DRAFT_FILE)
    }

    pub(super) fn session(&self) -> Result<Option<Session>> {
        let path = self.session_path();
        parse(&path, read_or_none(&path)?)
    }

    pub(super) fn save_session(&self, session: &Session) -> Result<()> {
        write_private(&self.session_path(), &serde_json::to_string_pretty(session)?)
    }

    pub(super) fn logout(&self) -> Result<()> {
        match std::fs::remove_file(self.session_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn guide(&self) -> Result<String> {
        Ok(read_or_none(&self.guide_path())?.unwrap_or_default())
    }

    pub(super) fn write_guide(&self, body: &str) -> Result<()> {
        write_private(&self.guide_path(), body)
    }

    pub(super) fn draft(&self) -> Result<Option<Draft>> {
        let path = self.draft_path();
        parse(&path, read_or_none(&path)?)
    }

    pub(super) fn save_draft(&self, draft: &Draft) -> Result<()> {
        write_private(&self.draft_path(), &serde_json::to_string_pretty(draft)?)
    }

    pub(super) fn clear_draft(&self) -> Result<()> {
        match std::fs::remove_file(self.draft_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// The page's opening state, with every secret already removed.
    pub(super) fn status(&self) -> Result<Value> {
        let session = self.session()?.unwrap_or_default();
        Ok(json!({
            "session": session.masked(),
            "guide_dir": self.root.display().to_string(),
            "guide_exists": self.guide_path().is_file(),
            "draft": self.draft()?,
        }))
    }
}
