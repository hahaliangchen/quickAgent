use std::{
    fmt, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::credentials::{Credentials, Kind, Secret};

const SCHEMA: &str = include_str!("schema.sql");
const PROFILE_COLUMNS: &str = "id, name, host, port, username, auth_type, key_path, env_tier,
    require_approval, default_dir, tags_json, notes, host_key_fingerprint, last_connected_at,
    created_at, updated_at";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AuthType {
    #[default]
    Agent,
    KeyFile,
    Password,
}

impl AuthType {
    fn sql(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::KeyFile => "key_file",
            Self::Password => "password",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "agent" => Self::Agent,
            "key_file" => Self::KeyFile,
            "password" => Self::Password,
            _ => anyhow::bail!("invalid stored SSH authentication type"),
        })
    }

    fn credential_kind(self) -> Option<Kind> {
        match self {
            Self::Agent => None,
            Self::KeyFile => Some(Kind::Passphrase),
            Self::Password => Some(Kind::Password),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum EnvTier {
    #[default]
    Dev,
    Staging,
    Prod,
}

impl EnvTier {
    fn sql(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Staging => "staging",
            Self::Prod => "prod",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "dev" => Self::Dev,
            "staging" => Self::Staging,
            "prod" => Self::Prod,
            _ => anyhow::bail!("invalid stored SSH environment"),
        })
    }
}

#[derive(Deserialize)]
pub(super) struct NewServer {
    pub(super) name: String,
    pub(super) host: String,
    pub(super) port: Option<u16>,
    pub(super) username: String,
    pub(super) auth_type: AuthType,
    pub(super) key_path: Option<String>,
    pub(super) env_tier: EnvTier,
    pub(super) require_approval: bool,
    pub(super) default_dir: Option<String>,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    pub(super) notes: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ProfileUpdate {
    pub(super) name: String,
    pub(super) env_tier: EnvTier,
    #[serde(default)]
    pub(super) require_approval: bool,
    pub(super) default_dir: Option<String>,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    pub(super) notes: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Server {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) username: String,
    pub(super) auth_type: AuthType,
    #[serde(skip)]
    pub(super) key_path: Option<String>,
    pub(super) env_tier: EnvTier,
    pub(super) require_approval: bool,
    pub(super) default_dir: Option<String>,
    pub(super) tags: Vec<String>,
    pub(super) notes: Option<String>,
    pub(super) host_key_fingerprint: Option<HostKeyFingerprint>,
    pub(super) last_connected_at: Option<i64>,
    pub(super) created_at: i64,
    pub(super) updated_at: i64,
    pub(super) has_password: bool,
    pub(super) has_passphrase: bool,
}

impl Server {
    fn from_row(row: &rusqlite::Row<'_>, credentials: &Credentials) -> Result<Self> {
        let id: String = row.get(0)?;
        let auth_type = AuthType::parse(&row.get::<_, String>(5)?)?;
        Ok(Self {
            has_password: auth_type == AuthType::Password && credentials.has(&id, Kind::Password),
            has_passphrase: auth_type == AuthType::KeyFile
                && credentials.has(&id, Kind::Passphrase),
            id,
            name: row.get(1)?,
            host: row.get(2)?,
            port: row.get(3)?,
            username: row.get(4)?,
            auth_type,
            key_path: row.get(6)?,
            env_tier: EnvTier::parse(&row.get::<_, String>(7)?)?,
            require_approval: row.get(8)?,
            default_dir: row.get(9)?,
            tags: serde_json::from_str(&row.get::<_, String>(10)?)?,
            notes: row.get(11)?,
            host_key_fingerprint: row
                .get::<_, Option<String>>(12)?
                .as_deref()
                .map(HostKeyFingerprint::parse)
                .transpose()?,
            last_connected_at: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HostKeyFingerprint([u8; 32]);

impl HostKeyFingerprint {
    // Hash the SSH wire-format public key blob, not its text representation.
    pub(super) fn of_key_blob(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub(super) fn parse(value: &str) -> Result<Self> {
        let invalid = || anyhow::anyhow!("invalid stored SSH host key fingerprint");
        let hash = value.strip_prefix("SHA256:").ok_or_else(invalid)?;
        let bytes = STANDARD_NO_PAD.decode(hash).map_err(|_| invalid())?;
        Ok(Self(bytes.try_into().map_err(|_| invalid())?))
    }
}

impl fmt::Display for HostKeyFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SHA256:{}", STANDARD_NO_PAD.encode(self.0))
    }
}

impl Serialize for HostKeyFingerprint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("HOST_KEY_MISMATCH: trusted {trusted}, received {received}")]
pub(super) struct HostKeyMismatch {
    pub(super) trusted: HostKeyFingerprint,
    pub(super) received: HostKeyFingerprint,
}

#[derive(Clone, Copy)]
pub(super) enum Action {
    Terminal,
    Exec,
    Upload,
    Download,
    WriteFile,
    ReadFile,
}

impl Action {
    fn fields(self) -> (&'static str, &'static str) {
        match self {
            Self::Terminal => ("terminal", "Opened terminal"),
            Self::Exec => ("exec", "Executed remote command"),
            Self::Upload => ("upload", "Uploaded file"),
            Self::Download => ("download", "Downloaded file"),
            Self::WriteFile => ("write_file", "Wrote remote file"),
            Self::ReadFile => ("read_file", "Read remote file"),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct UsageEntry {
    pub(super) id: i64,
    pub(super) server_id: String,
    pub(super) task_id: Option<String>,
    pub(super) workspace_root: Option<String>,
    pub(super) action: String,
    pub(super) remote_path: Option<String>,
    pub(super) summary: String,
    pub(super) created_at: i64,
}

pub(super) struct Store {
    db: Connection,
    credentials_path: PathBuf,
}

impl Store {
    // config_dir is the host's application configuration directory, not a task workspace.
    pub(super) fn open(config_dir: &Path) -> Result<Self> {
        fs::create_dir_all(config_dir)?;
        let mut db = Connection::open(config_dir.join("ssh-workbench.sqlite3"))?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.pragma_update(None, "foreign_keys", true)?;
        if schema_version(&db)? != 1 {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            match schema_version(&tx)? {
                0 => tx.execute_batch(SCHEMA)?,
                1 => (),
                _ => anyhow::bail!("unsupported SSH workbench database version"),
            }
            tx.commit()?;
        }
        Ok(Self {
            db,
            credentials_path: config_dir.join("ssh-credentials.json"),
        })
    }

    pub(super) fn create(&self, server: NewServer) -> Result<String> {
        let (name, host, username) = (
            server.name.trim(),
            server.host.trim(),
            server.username.trim(),
        );
        ensure!(
            ![name, host, username]
                .iter()
                .any(|s| s.is_empty() || s.chars().any(char::is_control)),
            "SSH server name, host and username must be nonempty and contain no control characters"
        );
        let port = server.port.unwrap_or(22);
        ensure!(port != 0, "SSH port must not be zero");
        let key_path = server.key_path.as_deref();
        ensure!(
            match (server.auth_type, key_path) {
                (AuthType::KeyFile, Some(path)) =>
                    Path::new(path).is_absolute() && !path.contains('\0'),
                (AuthType::KeyFile, None) | (_, Some(_)) => false,
                (_, None) => true,
            },
            "an absolute private-key path is required and only valid for key-file authentication"
        );
        let approval = server.require_approval || server.env_tier == EnvTier::Prod;
        // Endpoint and auth are immutable at this layer; editing them must never reuse a stale key or credential.
        Ok(self.db.query_row(
            "INSERT INTO ssh_servers (name, host, port, username, auth_type, key_path, env_tier,
             require_approval, default_dir, tags_json, notes) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             RETURNING id",
            params![name, host, port, username, server.auth_type.sql(), key_path, server.env_tier.sql(),
                approval, server.default_dir, serde_json::to_string(&server.tags)?, server.notes],
            |row| row.get(0),
        )?)
    }

    pub(super) fn get(&self, id: &str) -> Result<Option<Server>> {
        let sql = format!("SELECT {PROFILE_COLUMNS} FROM ssh_servers WHERE id = ?1");
        let mut stmt = self.db.prepare(&sql)?;
        let mut rows = stmt.query([id])?;
        rows.next()?
            .map(|row| Server::from_row(row, &Credentials::read(&self.credentials_path)?))
            .transpose()
    }

    pub(super) fn list(&self) -> Result<Vec<Server>> {
        let credentials = Credentials::read(&self.credentials_path)?;
        let sql =
            format!("SELECT {PROFILE_COLUMNS} FROM ssh_servers ORDER BY name COLLATE NOCASE, id");
        let mut stmt = self.db.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            result.push(Server::from_row(row, &credentials)?);
        }
        Ok(result)
    }

    pub(super) fn update_details(&self, id: &str, input: ProfileUpdate) -> Result<()> {
        let name = input.name.trim();
        ensure!(
            !name.is_empty() && !name.chars().any(char::is_control),
            "Invalid SSH server name"
        );
        let changed = self.db.execute(
            "UPDATE ssh_servers SET name=?2, env_tier=?3, require_approval=?4, default_dir=?5,
             tags_json=?6, notes=?7, updated_at=unixepoch() WHERE id=?1",
            params![
                id,
                name,
                input.env_tier.sql(),
                input.require_approval || input.env_tier == EnvTier::Prod,
                input.default_dir,
                serde_json::to_string(&input.tags)?,
                input.notes
            ],
        )?;
        ensure!(changed == 1, "SSH server not found");
        Ok(())
    }

    pub(super) fn recent_in_workspace(&self, workspace: &str) -> Result<Vec<String>> {
        let mut stmt = self.db.prepare(
            "SELECT server_id FROM ssh_usage_logs WHERE workspace_root=?1
             GROUP BY server_id ORDER BY MAX(id) DESC",
        )?;
        Ok(stmt
            .query_map([workspace], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(super) fn set_credential(&mut self, id: &str, secret: Option<Secret>) -> Result<()> {
        // The database write lock serializes credential file changes even across processes.
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let kind = auth_type(&tx, id)?
            .credential_kind()
            .context("SSH agent profiles do not take a saved credential")?;
        let mut credentials = Credentials::read(&self.credentials_path)?;
        if let Some(secret) = secret {
            credentials.set(id, kind, secret);
            credentials.write(&self.credentials_path)?;
        } else if credentials.remove(id) {
            credentials.write(&self.credentials_path)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn auth_secret(&self, id: &str) -> Result<Option<Secret>> {
        let Some(kind) = auth_type(&self.db, id)?.credential_kind() else {
            return Ok(None);
        };
        Ok(Credentials::read(&self.credentials_path)?.get(id, kind))
    }

    pub(super) fn delete(&mut self, id: &str) -> Result<bool> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = tx.execute("DELETE FROM ssh_servers WHERE id = ?1", [id])? != 0;
        if deleted {
            let mut credentials = Credentials::read(&self.credentials_path)?;
            // Delete the secret before commit; a crash may remove a credential, but cannot reuse it.
            if credentials.remove(id) {
                credentials.write(&self.credentials_path)?;
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    // Must run before authentication. The first observer pins the key under the write lock.
    pub(super) fn observe_host_key(
        &mut self,
        id: &str,
        received: HostKeyFingerprint,
    ) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        match stored_host_key(&tx, id)? {
            Some(trusted) if trusted != received => {
                return Err(HostKeyMismatch { trusted, received }.into());
            }
            Some(_) => (),
            None => store_host_key(&tx, id, received)?,
        }
        tx.commit()?;
        Ok(())
    }

    // Caller must get explicit confirmation for this exact old/new pair.
    pub(super) fn confirm_host_key(
        &mut self,
        id: &str,
        expected: HostKeyFingerprint,
        replacement: HostKeyFingerprint,
    ) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            stored_host_key(&tx, id)? == Some(expected),
            "SSH host key confirmation is stale; verify fingerprints again"
        );
        store_host_key(&tx, id, replacement)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn mark_connected(
        &self,
        id: &str,
        authenticated_key: HostKeyFingerprint,
    ) -> Result<()> {
        let changed = self.db.execute(
            "UPDATE ssh_servers SET last_connected_at = unixepoch()
             WHERE id = ?1 AND host_key_fingerprint = ?2",
            params![id, authenticated_key.to_string()],
        )?;
        ensure!(
            changed == 1,
            "SSH profile or trusted host key changed during authentication"
        );
        Ok(())
    }

    pub(super) fn record_usage(
        &self,
        id: &str,
        action: Action,
        task_id: Option<&str>,
        workspace_root: Option<&str>,
        remote_path: Option<&str>,
    ) -> Result<()> {
        let (action, summary) = action.fields();
        self.db.execute(
            "INSERT INTO ssh_usage_logs (server_id, task_id, workspace_root, action, remote_path, summary)
             VALUES (?1,?2,?3,?4,?5,?6)", params![id, task_id, workspace_root, action, remote_path, summary],
        )?;
        Ok(())
    }

    pub(super) fn history(&self, id: &str, limit: u32) -> Result<Vec<UsageEntry>> {
        let mut stmt = self.db.prepare(
            "SELECT id, server_id, task_id, workspace_root, action, remote_path, summary, created_at
             FROM ssh_usage_logs WHERE server_id = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id, limit.min(200)], |row| {
            Ok(UsageEntry {
                id: row.get(0)?,
                server_id: row.get(1)?,
                task_id: row.get(2)?,
                workspace_root: row.get(3)?,
                action: row.get(4)?,
                remote_path: row.get(5)?,
                summary: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn schema_version(db: &Connection) -> rusqlite::Result<u32> {
    db.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn auth_type(db: &Connection, id: &str) -> Result<AuthType> {
    let value: String = db
        .query_row(
            "SELECT auth_type FROM ssh_servers WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .context("SSH server not found")?;
    AuthType::parse(&value)
}

fn stored_host_key(db: &Connection, id: &str) -> Result<Option<HostKeyFingerprint>> {
    let value: Option<String> = db
        .query_row(
            "SELECT host_key_fingerprint FROM ssh_servers WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .context("SSH server not found")?;
    value.as_deref().map(HostKeyFingerprint::parse).transpose()
}

fn store_host_key(tx: &Transaction<'_>, id: &str, key: HostKeyFingerprint) -> Result<()> {
    tx.execute(
        "UPDATE ssh_servers SET host_key_fingerprint = ?2, updated_at = unixepoch() WHERE id = ?1",
        params![id, key.to_string()],
    )?;
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "store_tests.rs"]
mod tests;
