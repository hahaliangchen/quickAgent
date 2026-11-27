use std::{io::SeekFrom, time::Duration};

use anyhow::{Context, Result, ensure};
use russh_sftp::{
    client::SftpSession,
    protocol::{FileAttributes, OpenFlags},
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::client::Connection;

pub(super) struct Sftp {
    // Keep the underlying SSH connection alive for the lifetime of the subsystem.
    pub(super) session: SftpSession,
    _connection: Connection,
}

#[derive(Serialize)]
pub(super) struct Entry {
    pub(super) name: String,
    pub(super) kind: &'static str,
    pub(super) size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) modified_ms: Option<i64>,
}

pub(super) fn format_mtime(mtime_secs: Option<u32>) -> (Option<String>, Option<i64>) {
    let Some(secs) = mtime_secs else {
        return (None, None);
    };
    let secs_i64 = i64::from(secs);
    let iso = chrono::DateTime::from_timestamp(secs_i64, 0).map(|dt| dt.to_rfc3339());
    (iso, Some(secs_i64 * 1000))
}

pub(super) fn sort_entries(entries: &mut [Entry], sort_by: &str) {
    match sort_by {
        "mtime_desc" => entries.sort_by(|a, b| {
            b.modified_ms
                .cmp(&a.modified_ms)
                .then_with(|| a.name.cmp(&b.name))
        }),
        _ => entries.sort_by(|a, b| {
            (a.kind != "directory", &a.name).cmp(&(b.kind != "directory", &b.name))
        }),
    }
}

async fn copy_with_sha256(
    mut reader: impl AsyncRead + Unpin,
    mut writer: impl tokio::io::AsyncWrite + Unpin,
) -> std::io::Result<(u64, String)> {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n]).await?;
        total += n as u64;
    }
    let hex = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Ok((total, hex))
}

impl Sftp {
    pub(super) async fn open(connection: Connection) -> Result<Self> {
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut channel = connection.channel_open_session().await?;
            channel.request_subsystem(true, "sftp").await?;
            loop {
                match channel.wait().await {
                    Some(russh::ChannelMsg::Success) => break,
                    Some(russh::ChannelMsg::Failure | russh::ChannelMsg::Close) | None => {
                        anyhow::bail!("SSH server refused the SFTP subsystem")
                    }
                    _ => (),
                }
            }
            let session = SftpSession::new(channel.into_stream()).await?;
            session.set_timeout(30);
            Ok(Self {
                session,
                _connection: connection,
            })
        })
        .await
        .context("SFTP subsystem setup timed out")?
    }

    pub(super) async fn display_path(&self, path: &str) -> String {
        self.session
            .canonicalize(path)
            .await
            .unwrap_or_else(|_| path.to_owned())
    }

    pub(super) async fn list(&self, path: &str) -> Result<(String, Vec<Entry>)> {
        let path = self.session.canonicalize(path).await?;
        let mut entries: Vec<_> = self
            .session
            .read_dir(&path)
            .await?
            .filter_map(|entry| {
                let name = entry.file_name();
                if name == "." || name == ".." || name.contains(['/', '\0']) {
                    return None;
                }
                let meta = entry.metadata();
                let (modified_at, modified_ms) = format_mtime(meta.mtime);
                Some(Entry {
                    name,
                    kind: if meta.is_dir() {
                        "directory"
                    } else if meta.is_symlink() {
                        "symlink"
                    } else if meta.is_regular() {
                        "file"
                    } else {
                        "other"
                    },
                    size: meta.size,
                    modified_at,
                    modified_ms,
                })
            })
            .collect();
        sort_entries(&mut entries, "name");
        Ok((path, entries))
    }

    pub(super) async fn preview(&self, path: &str) -> Result<(String, bool)> {
        const LIMIT: usize = 64 * 1024;
        let mut input = self.session.open(path).await?.take(LIMIT as u64 + 4);
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).await?;
        let truncated = bytes.len() > LIMIT;
        if truncated {
            bytes.truncate(LIMIT);
        }
        ensure!(!bytes.contains(&0), "This file is not UTF-8 text");
        if let Err(error) = std::str::from_utf8(&bytes) {
            ensure!(
                truncated && error.error_len().is_none(),
                "This file is not UTF-8 text"
            );
            bytes.truncate(error.valid_up_to());
        }
        Ok((String::from_utf8(bytes)?, truncated))
    }

    pub(super) async fn upload(
        &self,
        path: &str,
        input: impl AsyncRead + Unpin,
        expected: u64,
    ) -> Result<u64> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!("Cannot generate upload name"))?;
        let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let parent = path
            .rsplit_once('/')
            .map_or("".to_owned(), |(parent, _)| format!("{parent}/"));
        let temporary = format!("{parent}.grow-upload-{nonce}");
        let mut file = self
            .session
            .open_with_flags(
                &temporary,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            )
            .await?;
        let result = async {
            let count = tokio::time::timeout(Duration::from_secs(1800), async {
                tokio::io::copy(&mut input.take(expected + 1), &mut file).await
            })
            .await
            .context("SFTP upload timed out")??;
            ensure!(
                count == expected,
                "Upload ended before its declared size or exceeded it"
            );
            file.shutdown().await?;
            // SFTP v3 rename fails when the destination already exists.
            self.session.rename(&temporary, path).await?;
            Ok(count)
        }
        .await;
        drop(file);
        if result.is_err() {
            let _ = self.session.remove_file(&temporary).await;
        }
        result
    }

    pub(super) async fn ensure_parent_dirs(&self, path: &str) -> Result<()> {
        let Some((parent, _)) = path.rsplit_once('/') else {
            return Ok(());
        };
        if parent.is_empty() {
            return Ok(());
        }
        let absolute = parent.starts_with('/');
        let mut current = if absolute {
            String::from("/")
        } else {
            String::new()
        };
        for segment in parent.split('/').filter(|s| !s.is_empty()) {
            ensure!(
                segment != ".." && !segment.contains('\0'),
                "invalid remote parent directory"
            );
            if !current.is_empty() && !current.ends_with('/') {
                current.push('/');
            }
            current.push_str(segment);
            if self.session.metadata(&current).await.is_err() {
                let _ = self.session.create_dir(&current).await;
            }
        }
        Ok(())
    }

    pub(super) async fn upload_with_options(
        &self,
        path: &str,
        input: impl AsyncRead + Unpin,
        expected: u64,
        create_parents: bool,
        overwrite: bool,
        permissions: Option<u32>,
    ) -> Result<(u64, String)> {
        ensure!(
            !path.is_empty() && !path.contains('\0') && !path.ends_with('/'),
            "invalid remote file path"
        );
        if create_parents {
            self.ensure_parent_dirs(path).await?;
        }
        let exists = self.session.metadata(path).await.is_ok();
        ensure!(
            !exists || overwrite,
            "remote file already exists; pass overwrite: true to replace it"
        );
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!("Cannot generate upload name"))?;
        let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let parent = path
            .rsplit_once('/')
            .map_or("".to_owned(), |(parent, _)| format!("{parent}/"));
        let temporary = format!("{parent}.grow-upload-{nonce}");
        let mut file = self
            .session
            .open_with_flags(
                &temporary,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            )
            .await?;
        let result = async {
            let (count, sha256) = tokio::time::timeout(Duration::from_secs(1800), async {
                copy_with_sha256(input.take(expected + 1), &mut file).await
            })
            .await
            .context("SFTP upload timed out")??;
            ensure!(
                count == expected,
                "Upload ended before its declared size or exceeded it"
            );
            file.shutdown().await?;
            if exists && overwrite {
                let _ = self.session.remove_file(path).await;
            }
            self.session.rename(&temporary, path).await?;
            if let Some(mode) = permissions {
                // IMPORTANT: use `FileAttributes::empty()`, NOT `Default::default()` —
                // in `russh-sftp`, `Default::default()` sets `size: Some(0)` and
                // `atime/mtime: Some(0)`, causing OpenSSH `SETSTAT` to truncate the
                // uploaded file to 0 bytes and reset its timestamp to 1970-01-01.
                let attrs = FileAttributes {
                    permissions: Some(mode & 0o777),
                    ..FileAttributes::empty()
                };
                // Best-effort: preserve local Unix permissions (e.g. 0755 for binaries/scripts)
                // without failing uploads on SFTP servers that reject SETSTAT.
                let _ = self.session.set_metadata(path, attrs).await;
            }
            if let Ok(meta) = self.session.metadata(path).await
                && let Some(remote_size) = meta.size
            {
                ensure!(
                    remote_size == count,
                    "remote file size mismatch after upload: expected {count} bytes, got {remote_size} bytes"
                );
            }
            Ok((count, sha256))
        }
        .await;
        drop(file);
        if result.is_err() {
            let _ = self.session.remove_file(&temporary).await;
        }
        result
    }

    pub(super) async fn download_to_writer(
        &self,
        remote_path: &str,
        mut output: impl tokio::io::AsyncWrite + Unpin,
        max_bytes: u64,
    ) -> Result<(u64, String)> {
        ensure!(
            !remote_path.is_empty() && !remote_path.contains('\0') && !remote_path.ends_with('/'),
            "invalid remote file path"
        );
        let meta = self.session.metadata(remote_path).await?;
        if let Some(size) = meta.size {
            ensure!(
                size <= max_bytes,
                "remote file exceeds {} MiB AI download limit",
                max_bytes / (1024 * 1024)
            );
        }
        let input = self.session.open(remote_path).await?.take(max_bytes + 1);
        let (count, sha256) = tokio::time::timeout(Duration::from_secs(1800), async {
            copy_with_sha256(input, &mut output).await
        })
        .await
        .context("SFTP download timed out")??;
        ensure!(
            count <= max_bytes,
            "remote file exceeds {} MiB AI download limit",
            max_bytes / (1024 * 1024)
        );
        output.flush().await?;
        Ok((count, sha256))
    }

    pub(super) async fn read_lines(
        &self,
        path: &str,
        start_line: usize,
        end_line: Option<usize>,
        max_chars: usize,
    ) -> Result<Value> {
        const READ_CAP: u64 = 2 * 1024 * 1024;
        ensure!(start_line >= 1, "start_line must be >= 1");
        if let Some(end) = end_line {
            ensure!(end >= start_line, "end_line must be >= start_line");
        }
        let mut input = self.session.open(path).await?.take(READ_CAP + 1);
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).await?;
        let file_truncated = bytes.len() as u64 > READ_CAP;
        if file_truncated {
            bytes.truncate(READ_CAP as usize);
        }
        ensure!(!bytes.contains(&0), "This remote file is not UTF-8 text");
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) if file_truncated => {
                let valid = error.utf8_error().valid_up_to();
                let mut raw = error.into_bytes();
                raw.truncate(valid);
                String::from_utf8(raw)?
            }
            Err(_) => anyhow::bail!("This remote file is not UTF-8 text"),
        };
        let all_lines: Vec<&str> = text.lines().collect();
        let total_lines = all_lines.len();
        let start_idx = start_line.saturating_sub(1).min(total_lines);
        let end_idx = end_line.map_or(total_lines, |e| e.min(total_lines));
        let mut out = String::new();
        let mut returned_lines = 0usize;
        let mut truncated = file_truncated;
        for line in &all_lines[start_idx..end_idx] {
            let addition = line.chars().count() + usize::from(!out.is_empty());
            if out.chars().count() + addition > max_chars {
                truncated = true;
                break;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
            returned_lines += 1;
        }
        Ok(json!({
            "path": path,
            "start_line": start_line,
            "returned_lines": returned_lines,
            "total_lines": total_lines,
            "truncated": truncated,
            "content": out,
        }))
    }

    pub(super) async fn read_text_full(&self, path: &str) -> Result<String> {
        const MAX_EDIT: u64 = 4 * 1024 * 1024;
        let mut input = self.session.open(path).await?.take(MAX_EDIT + 1);
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).await?;
        ensure!(
            bytes.len() as u64 <= MAX_EDIT,
            "remote text file exceeds 4 MiB edit limit"
        );
        ensure!(!bytes.contains(&0), "This remote file is not UTF-8 text");
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("This remote file is not UTF-8 text"))
    }

    pub(super) async fn tail_lines(
        &self,
        path: &str,
        lines: usize,
        filter: Option<&str>,
        max_chars: usize,
    ) -> Result<Value> {
        const TAIL_WINDOW: u64 = 512 * 1024;
        let meta = self.session.metadata(path).await?;
        let file_size = meta.size.unwrap_or(0);
        let mut file = self.session.open(path).await?;
        let seek_offset = file_size.saturating_sub(TAIL_WINDOW);
        if seek_offset > 0 {
            file.seek(SeekFrom::Start(seek_offset)).await?;
        }
        let mut bytes = Vec::new();
        file.take(TAIL_WINDOW + 1).read_to_end(&mut bytes).await?;
        ensure!(!bytes.contains(&0), "This remote file is not UTF-8 text");
        let text = String::from_utf8_lossy(&bytes);
        let mut collected: Vec<&str> = text.lines().collect();
        if seek_offset > 0 && !collected.is_empty() {
            // Drop potentially partial first line from mid-file seek.
            collected.remove(0);
        }
        let needle = filter
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase());
        if let Some(ref needle) = needle {
            collected.retain(|line| line.to_lowercase().contains(needle));
        }
        let matched_lines = collected.len();
        let take_start = matched_lines.saturating_sub(lines);
        let slice = &collected[take_start..];
        let mut out = String::new();
        let mut truncated = false;
        for line in slice.iter().rev() {
            let addition = line.chars().count() + usize::from(!out.is_empty());
            if out.chars().count() + addition > max_chars {
                truncated = true;
                break;
            }
            if !out.is_empty() {
                out.insert(0, '\n');
            }
            out.insert_str(0, line);
        }
        Ok(json!({
            "path": path,
            "file_size": file_size,
            "window_from_byte": seek_offset,
            "matched_lines": matched_lines,
            "returned_lines": if out.is_empty() { 0 } else { out.lines().count() },
            "truncated": truncated,
            "content": out,
        }))
    }

    pub(super) async fn write_text_atomic(
        &self,
        path: &str,
        content: &str,
        create_parents: bool,
        backup: bool,
    ) -> Result<u64> {
        const MAX_WRITE: usize = 4 * 1024 * 1024;
        ensure!(
            content.len() <= MAX_WRITE,
            "remote text write exceeds 4 MiB limit; use ssh__upload for larger files"
        );
        if create_parents {
            self.ensure_parent_dirs(path).await?;
        }
        if backup && self.session.metadata(path).await.is_ok() {
            let bak = format!("{path}.bak");
            let src = self.session.open(path).await?;
            let mut existing = Vec::new();
            src.take(MAX_WRITE as u64 + 1)
                .read_to_end(&mut existing)
                .await?;
            ensure!(
                existing.len() <= MAX_WRITE,
                "existing file exceeds 4 MiB backup limit"
            );
            self.upload_with_options(
                &bak,
                existing.as_slice(),
                existing.len() as u64,
                false,
                true,
                None,
            )
            .await?;
        }
        let (bytes, _) = self
            .upload_with_options(path, content.as_bytes(), content.len() as u64, false, true, None)
            .await?;
        Ok(bytes)
    }
}
