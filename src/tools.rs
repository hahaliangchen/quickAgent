use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::go_index::{
    self, IndexGoWorkspaceRequest, ListGoSymbolsRequest, ReadGoSymbolRequest,
    SearchGoSymbolsRequest,
};
use crate::memory::{
    self, ListArchitectureMemoryRequest, ListSymbolBusinessContextRequest, ListWorkMemoryRequest,
    RecordArchitectureMemoryRequest, RecordSymbolBusinessContextRequest, RecordWorkMemoryRequest,
    SearchArchitectureMemoryRequest, SearchSymbolBusinessContextRequest, SearchWorkMemoryRequest,
};
use crate::python_index::{
    self, IndexPythonWorkspaceRequest, ListPythonSymbolsRequest, ReadPythonSymbolRequest,
    SearchPythonSymbolsRequest,
};
use crate::rust_index::{
    self, IndexRustWorkspaceRequest, ListRustSymbolsRequest, ReadRustSymbolRequest,
    SearchRustSymbolsRequest,
};
use crate::ts_index::{
    self, IndexTsWorkspaceRequest, ListTsSymbolsRequest, ReadTsSymbolRequest,
    SearchTsSymbolsRequest,
};

const DEFAULT_MAX_READ_BYTES: u64 = 1024 * 1024;
const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;
const MAX_CHUNK_BYTES: usize = 512 * 1024;
const DEFAULT_MAX_MATCHES: usize = 100;
const MAX_CONTEXT_LINES: usize = 20;
const DEFAULT_WRITE_LIMIT_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_SEARCH_FILE_LIMIT_BYTES: u64 = 2 * 1024 * 1024;
/// Inline byte budget for `search_text` matches/counts so responses stay well below
/// the 96 KiB direct-tool delivery soft cap and remain in-context.
const MAX_SEARCH_RESPONSE_BYTES: usize = 48 * 1024;
/// A matched line longer than this comes back as an excerpt around the match.
const MATCH_TEXT_CHARS: usize = 400;
/// How much of a long line before the match its excerpt keeps.
const MATCH_CONTEXT_CHARS: usize = 100;
const NOISE_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".swarm",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".venv",
    "venv",
    "__pycache__",
];

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("absolute paths are not accepted: {0}")]
    AbsolutePath(String),
    #[error("parent traversal is not accepted: {0}")]
    ParentTraversal(String),
    #[error("path does not exist: {0}")]
    NotFound(String),
    #[error("path is not a file: {0}")]
    NotFile(String),
    #[error("path is not a directory: {0}")]
    NotDirectory(String),
    #[error("workspace_root must be an absolute directory path: {0}")]
    WorkspaceRootMustBeAbsolute(String),
    #[error("workspace_root is required for this tool")]
    WorkspaceRootRequired,
    #[error("file is too large: {actual} bytes exceeds limit {limit} bytes")]
    FileTooLarge { actual: u64, limit: u64 },
    #[error("content is too large: {actual} bytes exceeds limit {limit} bytes")]
    ContentTooLarge { actual: usize, limit: usize },
    #[error("invalid line range: start_line={start}, end_line={end}")]
    InvalidLineRange { start: usize, end: usize },
    #[error("expected_old_text did not match the selected line range")]
    ExpectedTextMismatch,
    #[error("write_conflict: {0}")]
    WriteConflict(String),
    #[error("write_precondition_required: {0}")]
    WritePreconditionRequired(String),
    #[error("path is outside the selected workspace: {0}")]
    OutsideWorkspace(String),
    #[error("invalid_edit: {0}")]
    InvalidEdit(String),
    #[error("invalid search pattern: {0}")]
    InvalidSearchPattern(String),
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("utf-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

pub type Result<T> = std::result::Result<T, ToolError>;

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceInfo {
    pub workspace_root: String,
    pub platform: String,
    pub allowed_scope: String,
    pub default_ignored_dirs: Vec<&'static str>,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceInfoRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GitStatusRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_true")]
    pub include_untracked: bool,
    #[serde(default)]
    pub max_entries: Option<usize>,
}

impl Default for GitStatusRequest {
    fn default() -> Self {
        Self { workspace_root: None, paths: Vec::new(), include_untracked: true, max_entries: None }
    }
}

#[derive(Debug, Serialize)]
pub struct GitStatusResponse {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub clean: bool,
    pub staged: Vec<String>,
    pub modified: Vec<String>,
    pub untracked: Vec<String>,
    pub total_entries: usize,
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct GitDiffRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub rev_range: Option<String>,
    #[serde(default)]
    pub staged: bool,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub stat_only: bool,
    #[serde(default = "default_git_unified")]
    pub unified: usize,
    #[serde(default)]
    pub max_lines: Option<usize>,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

impl Default for GitDiffRequest {
    fn default() -> Self {
        Self { workspace_root: None, rev_range: None, staged: false, paths: Vec::new(), stat_only: false, unified: 3, max_lines: None, max_chars: None }
    }
}

#[derive(Debug, Serialize)]
pub struct GitDiffResponse {
    pub diff: String,
    pub files: Vec<String>,
    pub truncated: bool,
    pub stat_only: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct GitLogRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub rev: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GitCommit {
    pub commit: String,
    pub short_commit: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

#[derive(Debug, Serialize)]
pub struct GitLogResponse {
    pub commits: Vec<GitCommit>,
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct ListDirRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default = "default_dot")]
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub max_depth: Option<usize>,
    #[serde(default)]
    pub respect_gitignore: bool,
    /// Only entries whose path matches one of these globs, as in `rg -g`:
    /// `*.rs` matches at any depth, `src/**/*.ts` from the workspace root,
    /// `|` separates alternatives outside `{...}`, and a leading `!` excludes.
    #[serde(default, alias = "globs", deserialize_with = "one_or_many")]
    pub glob: Vec<String>,
    /// Exclude files or directories matching these patterns (`|` separates
    /// alternatives outside `{...}`).
    #[serde(default, alias = "excludes", alias = "ignore", deserialize_with = "one_or_many")]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub kind: Option<EntryKind>,
    /// Keep entries modified at or after this time (RFC3339, `YYYY-MM-DD`,
    /// epoch seconds/ms, or relative duration like `24h`, `7d`).
    #[serde(default)]
    pub modified_after: Option<String>,
    /// Keep entries modified at or before this time (same formats as `modified_after`).
    #[serde(default)]
    pub modified_before: Option<String>,
    #[serde(default)]
    pub sort_by: ListSort,
    #[serde(default, alias = "limit", alias = "head_limit")]
    pub max_entries: Option<usize>,
}

impl Default for ListDirRequest {
    fn default() -> Self {
        Self {
            workspace_root: None,
            path: default_dot(),
            recursive: false,
            max_depth: None,
            respect_gitignore: false,
            glob: Vec::new(),
            exclude: Vec::new(),
            kind: None,
            modified_after: None,
            modified_before: None,
            sort_by: ListSort::default(),
            max_entries: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListDirResponse {
    pub root: String,
    pub entries: Vec<DirEntryInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unexpanded_dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct DirEntryInfo {
    pub path: String,
    pub kind: EntryKind,
    pub size_bytes: Option<u64>,
    pub modified_at: Option<String>,
    pub modified_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListSort {
    #[default]
    Path,
    ModifiedDesc,
    ModifiedAsc,
    SizeDesc,
    SizeAsc,
}

#[derive(Debug, Deserialize)]
pub struct ReadFileRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    #[serde(default = "default_max_read_bytes")]
    pub max_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ReadFileResponse {
    pub code_hash: String,
    pub path: String,
    pub bytes: usize,
    pub content: String,
    pub total_lines: usize,
}

fn default_tail_end_line() -> isize {
    -1
}

#[derive(Debug, Deserialize)]
pub struct ReadFileLinesRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub start_line: isize,
    #[serde(default = "default_tail_end_line")]
    pub end_line: isize,
}

#[derive(Debug, Serialize)]
pub struct ReadFileLinesResponse {
    pub code_hash: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub lines: Vec<LineContent>,
    pub content: String,
    pub total_lines: usize,
    pub requested_end_line: isize,
    pub end_clamped_to_eof: bool,
}

#[derive(Debug, Serialize)]
pub struct LineContent {
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct ReadFileChunkRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    #[serde(default)]
    pub byte_offset: u64,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub max_lines: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ChunkLine {
    pub line: Option<usize>,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct ReadFileChunkResponse {
    pub path: String,
    pub file_bytes: u64,
    pub start_byte: u64,
    pub end_byte: u64,
    pub next_byte_offset: Option<u64>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub next_line: Option<usize>,
    pub eof: bool,
    pub line_truncated: bool,
    pub bytes: usize,
    pub line_count: usize,
    pub content: String,
    pub lines: Vec<ChunkLine>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchTextRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub query: String,
    /// None: matched as a regular expression when it is written as one
    /// (`a|b`, `name\(`, `^fn`, `.*`), otherwise literally.
    #[serde(default)]
    pub regex: Option<bool>,
    #[serde(default = "default_dot")]
    pub path: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub case_sensitive: bool,
    /// None: files that .gitignore or .ignore excludes are skipped, as rg
    /// does, unless the path searched is itself one of them.
    #[serde(default)]
    pub respect_gitignore: Option<bool>,
    #[serde(default = "default_max_matches", alias = "limit", alias = "head_limit")]
    pub max_matches: usize,
    /// Optional per-file match cap in `content` mode so one noisy file cannot
    /// exhaust `max_matches`.
    #[serde(default)]
    pub max_matches_per_file: Option<usize>,
    /// What comes back: matching lines (`content`), the files that match
    /// (`files`), or each file's count of matching lines (`count`).
    #[serde(default)]
    pub output_mode: SearchOutput,
    /// Only files whose path matches one of these globs, as in `rg -g`:
    /// `*.rs` matches at any depth, `src/**/*.ts` from the workspace root,
    /// `|` separates alternatives outside `{...}`, and a leading `!` excludes.
    #[serde(default, alias = "globs", deserialize_with = "one_or_many")]
    pub glob: Vec<String>,
    /// Exclude files or directories matching these patterns (`|` separates
    /// alternatives outside `{...}`).
    #[serde(default, alias = "excludes", alias = "ignore", deserialize_with = "one_or_many")]
    pub exclude: Vec<String>,
    /// Lines before and after each match, in content mode.
    #[serde(default, alias = "context")]
    pub context_lines: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchOutput {
    #[default]
    Content,
    #[serde(alias = "files_with_matches", alias = "files_only")]
    Files,
    Count,
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany { One(String), Many(Vec<String>) }
    Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(OneOrMany::One(one)) => vec![one],
        Some(OneOrMany::Many(many)) => many,
    })
}

/// A query nobody asked to treat as a regular expression is treated as one
/// when it is written as one and compiles: an alternation, an escape, an
/// anchor or `.*`. 2026-11-13: of 25 searches, 3 written as `a|b|c` or
/// `model_round\(` without regex:true matched literally, found nothing, and
/// the AI turned to grep in a shell. A pattern that matches the empty
/// string (`a || b` read as regex) stays literal, as does `arr[0]`.
fn written_as_regex(query: &str) -> bool {
    let marked = query.contains('|')
        || query.contains('\\')
        || query.contains(".*")
        || query.contains(".+")
        || query.contains("(?")
        || query.starts_with('^')
        || (query.ends_with('$') && !query.ends_with("\\$"));
    marked && regex::Regex::new(query).is_ok_and(|re| !re.is_match(""))
}

#[derive(Debug, Serialize)]
pub struct SearchTextResponse {
    pub query: String,
    pub match_mode: &'static str,
    pub output_mode: SearchOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<&'static str>,
    pub index_used: bool,
    pub index_matches: usize,
    pub text_scan_used: bool,
    pub matches: Vec<TextMatch>,
    /// Files mode: the matching files, in walk order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// Count mode: matching lines per file, and their total.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<Vec<FileCount>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct FileCount {
    pub path: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct TextMatch {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub text: String,
    /// The whole line's length in characters, given when `text` is an excerpt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_chars: Option<usize>,
    /// With context_lines: the lines just before and after, nearest last
    /// and first, each cut like `text` when long.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct WriteFileRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub content: String,
    #[serde(default = "default_true")]
    pub create_parent_dirs: bool,
    pub expected_code_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WriteFileResponse {
    pub path: String,
    pub bytes_written: usize,
    pub go_reindexed: bool,
    pub changed: bool,
    pub created: bool,
    pub code_hash: String,
    pub index_refresh: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ReplaceRangeRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub replacement: String,
    pub expected_old_text: Option<String>,
    pub expected_code_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReplaceRangeResponse {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub bytes_written: usize,
    pub go_reindexed: bool,
    pub changed: bool,
    pub code_hash: String,
    pub index_refresh: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct TextEdit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Deserialize)]
pub struct EditFileRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub edits: Vec<TextEdit>,
    pub expected_code_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GoIndexResult {
    pub index_path: String,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub generated_at_unix: u64,
}

#[derive(Debug, Serialize)]
pub struct PythonIndexResult {
    pub index_path: String,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub generated_at_unix: u64,
}

#[derive(Debug, Serialize)]
pub struct RustIndexResult {
    pub index_path: String,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub generated_at_unix: u64,
}

#[derive(Debug, Serialize)]
pub struct TsIndexResult {
    pub index_path: String,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub generated_at_unix: u64,
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = if root.exists() {
            root.canonicalize()?
        } else {
            return Err(ToolError::NotFound(root.display().to_string()));
        };
        if !root.is_dir() {
            return Err(ToolError::NotDirectory(root.display().to_string()));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn workspace_info(&self, request: WorkspaceInfoRequest) -> Result<WorkspaceInfo> {
        let Some(workspace_root) = request.workspace_root.as_deref() else {
            return Err(ToolError::WorkspaceRootRequired);
        };
        let workspace = self.with_root(Some(workspace_root))?;
        Ok(WorkspaceInfo {
            workspace_root: workspace.root.display().to_string(),
            platform: std::env::consts::OS.to_string(),
            allowed_scope: "reads can use accessible absolute paths, and outside workspace_root they wait for the user's approval unless the permission mode is full access; structured writes must stay inside the selected workspace_root, including resolved symlinks and junctions; relative paths resolve below workspace_root".to_string(),
            default_ignored_dirs: NOISE_DIRS.to_vec(),
        })
    }

    pub fn git_status(&self, request: GitStatusRequest) -> Result<GitStatusResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let paths = git_paths(&request.paths)?;
        let mut args = vec!["status", "--porcelain=v1", "--branch", "-z"];
        args.push(if request.include_untracked { "--untracked-files=all" } else { "--untracked-files=no" });
        if !paths.is_empty() {
            args.push("--");
            args.extend(paths.iter().map(String::as_str));
        }
        let raw = run_git(&workspace.root, &args)?;
        let head = git_text(&workspace.root, &["rev-parse", "--verify", "-q", "HEAD"])
            .ok()
            .filter(|value| !value.is_empty());
        let branch = git_text(&workspace.root, &["branch", "--show-current"])
            .ok()
            .filter(|value| !value.is_empty());
        let mut staged = Vec::new();
        let mut modified = Vec::new();
        let mut untracked = Vec::new();
        let mut total_entries = 0;
        let max_entries = request.max_entries.unwrap_or(200).clamp(1, 1000);
        let mut records = raw.stdout.split('\0');
        while let Some(record) = records.next() {
            if record.is_empty() || record.starts_with("## ") {
                continue;
            }
            let Some(status) = record.get(0..2) else { continue };
            let path = record.get(3..).unwrap_or_default();
            if path.is_empty() { continue; }
            let is_rename_or_copy = status.as_bytes().first().is_some_and(|value| matches!(*value, b'R' | b'C'));
            total_entries += 1;
            if total_entries <= max_entries {
                if status == "??" {
                    untracked.push(path.to_string());
                } else {
                    if status.as_bytes().first().is_some_and(|value| *value != b' ') {
                        staged.push(path.to_string());
                    }
                    if status.as_bytes().get(1).is_some_and(|value| *value != b' ') {
                        modified.push(path.to_string());
                    }
                }
            }
            // With `-z`, porcelain v1 emits the old path as a second
            // NUL-delimited record for renames/copies. Consume it even when
            // the primary entry is beyond max_entries, otherwise it would
            // be counted as a second status record on the next iteration.
            if is_rename_or_copy {
                let _ = records.next();
            }
        }
        let truncated = total_entries > max_entries;
        Ok(GitStatusResponse {
            head,
            branch,
            clean: total_entries == 0,
            staged,
            modified,
            untracked,
            total_entries,
            truncated,
        })
    }

    pub fn git_diff(&self, request: GitDiffRequest) -> Result<GitDiffResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let paths = git_paths(&request.paths)?;
        let mut args = vec!["diff", "--no-color", "--no-ext-diff"];
        let unified = request.unified.min(100);
        let unified_arg = format!("--unified={unified}");
        args.push(&unified_arg);
        if request.staged { args.push("--cached"); }
        if request.stat_only { args.push("--stat"); }
        if let Some(range) = request.rev_range.as_deref() {
            validate_git_arg(range, "rev_range")?;
            args.push(range);
        }
        if !paths.is_empty() {
            args.push("--");
            args.extend(paths.iter().map(String::as_str));
        }
        let raw = run_git(&workspace.root, &args)?;
        let mut diff = raw.stdout;
        let max_chars = request.max_chars.unwrap_or(48_000).clamp(256, 256_000);
        let max_lines = request.max_lines.unwrap_or(2_000).clamp(1, 20_000);
        let mut truncated = false;
        if diff.lines().count() > max_lines {
            diff = diff.lines().take(max_lines).collect::<Vec<_>>().join("\n");
            diff.push('\n');
            truncated = true;
        }
        if diff.chars().count() > max_chars {
            let mut end = diff.char_indices().nth(max_chars).map_or(diff.len(), |(index, _)| index);
            while !diff.is_char_boundary(end) { end -= 1; }
            diff.truncate(end);
            diff.push_str("\n… [diff preview truncated]");
            truncated = true;
        }
        let mut file_args = vec!["diff", "--no-color", "--no-ext-diff", "--name-only"];
        if request.staged { file_args.push("--cached"); }
        if let Some(range) = request.rev_range.as_deref() { file_args.push(range); }
        if !paths.is_empty() { file_args.push("--"); file_args.extend(paths.iter().map(String::as_str)); }
        let files = run_git(&workspace.root, &file_args)?.stdout.lines().map(str::to_owned).filter(|line| !line.is_empty()).collect();
        Ok(GitDiffResponse { diff, files, truncated, stat_only: request.stat_only })
    }

    pub fn git_log(&self, request: GitLogRequest) -> Result<GitLogResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let limit = request.limit.unwrap_or(20).clamp(1, 100);
        let limit_arg = format!("-{}", limit.saturating_add(1).min(101));
        let paths = git_paths(&request.paths)?;
        let mut args = vec!["log", limit_arg.as_str(), "--date=iso-strict", "--format=%H%x00%h%x00%an%x00%aI%x00%s%x1e"];
        if let Some(rev) = request.rev.as_deref() { validate_git_arg(rev, "rev")?; args.push(rev); }
        if !paths.is_empty() { args.push("--"); args.extend(paths.iter().map(String::as_str)); }
        let raw = run_git(&workspace.root, &args)?;
        let mut commits = raw.stdout.split('\x1e').filter_map(|record| {
            let fields: Vec<_> = record.trim_matches('\n').splitn(5, '\0').collect();
            (fields.len() == 5).then(|| GitCommit { commit: fields[0].to_owned(), short_commit: fields[1].to_owned(), author: fields[2].to_owned(), date: fields[3].to_owned(), subject: fields[4].trim_end().to_owned() })
        }).collect::<Vec<_>>();
        let truncated = commits.len() > limit;
        commits.truncate(limit);
        Ok(GitLogResponse { truncated, commits })
    }

    pub fn list_dir(&self, request: ListDirRequest) -> Result<ListDirResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let root = workspace.resolve_existing(&request.path)?;

        let globs = search_globs(&workspace.root, &request.glob, &request.exclude)?;
        let allowed_noise = allowed_noise_dirs(&request.glob);
        let modified_after = request
            .modified_after
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| parse_timestamp_ms(s, false))
            .transpose()?;
        let modified_before = request
            .modified_before
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| parse_timestamp_ms(s, true))
            .transpose()?;

        let filter = DirEntryFilter {
            workspace: &workspace,
            kind: request.kind,
            globs: globs.as_ref(),
            modified_after,
            modified_before,
        };
        if !root.is_dir() {
            let mut entries = Vec::new();
            push_matching_dir_entry(&filter, &root, &mut entries)?;
            return Ok(ListDirResponse {
                root: workspace.relative_display(&root)?,
                entries,
                unexpanded_dirs: Vec::new(),
                hints: Vec::new(),
                truncated: false,
            });
        }

        let unexpanded_noise = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let unexpanded_collector = Arc::clone(&unexpanded_noise);
        let filter_globs = globs.clone();
        let mut builder = WalkBuilder::new(&root);
        builder
            .hidden(false)
            .ignore(request.respect_gitignore)
            .git_ignore(request.respect_gitignore)
            .git_exclude(request.respect_gitignore)
            .parents(request.respect_gitignore)
            .filter_entry(move |entry| {
                if filter_globs.as_ref().is_some_and(|g| {
                    g.is_excluded(
                        entry.path(),
                        entry.file_type().is_some_and(|ft| ft.is_dir()),
                    )
                }) {
                    return false;
                }
                if is_skipped_noise_dir(entry, &allowed_noise) {
                    if let Ok(mut list) = unexpanded_collector.lock() {
                        list.push(entry.path().to_path_buf());
                    }
                    return false;
                }
                true
            });
        if !request.recursive && request.glob.is_empty() {
            builder.max_depth(Some(1));
        } else if let Some(depth) = request.max_depth {
            builder.max_depth(Some(depth.saturating_add(1)));
        }

        let mut entries = Vec::new();
        for item in builder.build().skip(1) {
            let Ok(entry) = item else {
                continue;
            };
            push_matching_dir_entry(&filter, entry.path(), &mut entries)?;
        }

        let skipped_paths = unexpanded_noise
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default();
        let mut unexpanded_dirs = Vec::with_capacity(skipped_paths.len());
        for path in skipped_paths {
            push_matching_dir_entry(&filter, &path, &mut entries)?;
            unexpanded_dirs.push(workspace.relative_display(&path)?);
        }
        unexpanded_dirs.sort();
        unexpanded_dirs.dedup();
        let hints = list_dir_hints(&unexpanded_dirs);

        match request.sort_by {
            ListSort::Path => entries.sort_by(|a, b| a.path.cmp(&b.path)),
            ListSort::ModifiedDesc => entries.sort_by(|a, b| {
                b.modified_ms
                    .cmp(&a.modified_ms)
                    .then_with(|| a.path.cmp(&b.path))
            }),
            ListSort::ModifiedAsc => entries.sort_by(|a, b| {
                a.modified_ms
                    .cmp(&b.modified_ms)
                    .then_with(|| a.path.cmp(&b.path))
            }),
            ListSort::SizeDesc => entries.sort_by(|a, b| {
                b.size_bytes
                    .cmp(&a.size_bytes)
                    .then_with(|| a.path.cmp(&b.path))
            }),
            ListSort::SizeAsc => entries.sort_by(|a, b| {
                a.size_bytes
                    .cmp(&b.size_bytes)
                    .then_with(|| a.path.cmp(&b.path))
            }),
        }

        let mut truncated = false;
        if let Some(limit) = request.max_entries.filter(|&n| n > 0)
            && entries.len() > limit
        {
            entries.truncate(limit);
            truncated = true;
        }

        Ok(ListDirResponse {
            root: workspace.relative_display(&root)?,
            entries,
            unexpanded_dirs,
            hints,
            truncated,
        })
    }

    pub fn read_file(&self, request: ReadFileRequest) -> Result<ReadFileResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = workspace.resolve_existing_file(&request.path)?;
        let metadata = fs::metadata(&path)?;
        let limit = request.max_bytes.min(crate::source_read::FILE_LIMIT as u64);
        if metadata.len() > limit {
            return Err(ToolError::FileTooLarge {
                actual: metadata.len(),
                limit,
            });
        }

        let snapshot = crate::source_read::read(&path, limit as usize)?;
        let content = snapshot.content.clone();
        Ok(ReadFileResponse {
            code_hash: snapshot.hash.clone(),
            path: workspace.relative_display(&path)?,
            bytes: content.len(),
            content,
            total_lines: snapshot.line_count(),
        })
    }

    pub fn read_file_lines(&self, request: ReadFileLinesRequest) -> Result<ReadFileLinesResponse> {
        if request.start_line == 0 || request.end_line == 0 {
            return Err(ToolError::InvalidParameter(
                "start_line and end_line must be non-zero (1-based positive or negative from EOF)"
                    .to_string(),
            ));
        }
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = workspace.resolve_existing_file(&request.path)?;
        let metadata = fs::metadata(&path)?;
        let rel_path = workspace.relative_display(&path)?;

        // Files within FILE_LIMIT use the shared cached snapshot; larger files
        // stream line-by-line so head/tail inspection never triggers FileTooLarge.
        if metadata.len() <= crate::source_read::FILE_LIMIT as u64 {
            let snapshot = crate::source_read::read(&path, crate::source_read::FILE_LIMIT)?;
            let total_lines = snapshot.line_count();
            let Some((start_line, end_line, end_clamped_to_eof)) =
                resolve_line_bounds(total_lines, request.start_line, request.end_line)?
            else {
                return Ok(ReadFileLinesResponse {
                    code_hash: snapshot.hash.clone(),
                    path: rel_path,
                    start_line: 0,
                    end_line: 0,
                    lines: Vec::new(),
                    content: String::new(),
                    total_lines: 0,
                    requested_end_line: request.end_line,
                    end_clamped_to_eof: false,
                });
            };
            let content = snapshot.range(start_line, end_line)?.to_owned();
            let lines = content
                .lines()
                .enumerate()
                .map(|(index, text)| LineContent {
                    line: start_line + index,
                    text: text.to_string(),
                })
                .collect();

            return Ok(ReadFileLinesResponse {
                code_hash: snapshot.hash.clone(),
                path: rel_path,
                start_line,
                end_line,
                lines,
                content,
                total_lines,
                requested_end_line: request.end_line,
                end_clamped_to_eof,
            });
        }

        read_large_file_lines(&path, &rel_path, request.start_line, request.end_line)
    }

    pub fn read_file_chunk(&self, request: ReadFileChunkRequest) -> Result<ReadFileChunkResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = workspace.resolve_existing_file(&request.path)?;
        let rel_path = workspace.relative_display(&path)?;
        let max_bytes = request
            .max_bytes
            .unwrap_or(DEFAULT_CHUNK_BYTES)
            .clamp(1, MAX_CHUNK_BYTES);
        let max_lines = request.max_lines.filter(|&n| n > 0);
        if request.start_line == Some(0) {
            return Err(ToolError::InvalidParameter(
                "start_line must be >= 1 when provided".to_string(),
            ));
        }
        stream_file_chunk(
            &path,
            &rel_path,
            request.byte_offset,
            request.start_line,
            max_bytes,
            max_lines,
        )
    }

    pub fn search_text(&self, request: SearchTextRequest) -> Result<SearchTextResponse> {
        let explicit = request.regex;
        let regex = explicit.unwrap_or_else(|| written_as_regex(&request.query));
        // Compile once, before opening the index or walking files. Escaping
        // preserves literal defaults and gives Unicode-safe match offsets.
        let pattern = if regex {
            request.query.clone()
        } else {
            regex::escape(&request.query)
        };
        let matcher = regex::RegexBuilder::new(&pattern)
            .case_insensitive(!request.case_sensitive)
            .size_limit(2 * 1024 * 1024)
            .build()
            .map_err(|error| ToolError::InvalidSearchPattern(error.to_string()))?;
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let roots = workspace.resolve_search_roots(&request)?;
        let root_filters = workspace.search_root_filters(&roots)?;
        let globs = search_globs(&workspace.root, &request.glob, &request.exclude)?;
        let allowed_noise = allowed_noise_dirs(&request.glob);
        let max_matches = request.max_matches.max(1);
        let scan = ScanOptions {
            mode: request.output_mode,
            respect_gitignore: true,
            limit: max_matches,
            max_matches_per_file: request.max_matches_per_file.map(|n| n.max(1)),
            context_lines: request.context_lines.min(MAX_CONTEXT_LINES),
            globs,
            allowed_noise,
        };
        let mut matches = Vec::new();
        let mut truncated = false;
        let mut seen_symbols = std::collections::HashSet::new();
        let mut index_used = false;

        // 1. 优先尝试从 SQLite 符号索引库中查找精确匹配的符号定义（不因外部搜索在未索引目录创建状态库）
        // Only for plain line results: the index knows no globs, files, counts or per-file caps.
        if !regex
            && scan.mode == SearchOutput::Content
            && scan.globs.is_none()
            && scan.context_lines == 0
            && scan.max_matches_per_file.is_none()
            && workspace
                .root
                .join(".codex-workspace-mcp")
                .join("codex_state.db")
                .is_file()
            && let Ok(conn) = crate::database::init_db(&workspace.root)
        {
            let root_str = workspace.root.to_string_lossy().to_string();
            let query_name = request.query.trim();

            for (lang, table) in [
                ("rust", "rust_symbols"),
                ("go", "go_symbols"),
                ("ts", "ts_symbols"),
                ("python", "python_symbols"),
            ] {
                if truncated {
                    break;
                }
                if crate::database::get_index_generated_at(&conn, &root_str, lang).is_some() {
                    index_used = true;
                    truncated = query_indexed_symbol_table(
                        &conn,
                        &root_str,
                        table,
                        lang,
                        query_name,
                        &root_filters,
                        max_matches,
                        &mut matches,
                        &mut seen_symbols,
                    );
                }
            }
        }
        let index_matches = matches.len();

        // 2. 如果置顶的符号匹配项未把配额占满，继续进行常规全文 Walk 扫描匹配
        let mut text_scan_used = false;
        let mut skipped_ignored = false;
        let mut skipped_noise = false;
        let mut truncated_by_bytes = false;
        let mut match_bytes: usize = matches
            .iter()
            .map(|m| serde_json::to_vec(m).map(|v| v.len()).unwrap_or(0))
            .sum();
        let mut count_bytes: usize = 0;
        let mut files = Vec::new();
        let mut counts = Vec::new();
        if !truncated {
            text_scan_used = true;
            let mut handles = Vec::new();
            for root in roots {
                let workspace_root = workspace.root.clone();
                let matcher = matcher.clone();
                let mut scan = scan.clone();
                // Searching everything by default walked tens of thousands of
                // ignored experiment and build files (2026-11-13: one search
                // of this project took 39.8 s; scoped to src it took 0.2 s).
                // A path the caller names inside an ignored folder is what
                // they asked for, so it is searched in full.
                scan.respect_gitignore = request.respect_gitignore.unwrap_or_else(|| !ignored_target(&workspace_root, &root));
                skipped_ignored |= scan.respect_gitignore;
                handles.push(std::thread::spawn(move || {
                    scan_text_root(workspace_root, root, matcher, &scan)
                }));
            }

            'outer: for handle in handles {
                let Ok(found) = handle.join() else {
                    continue;
                };
                truncated |= found.truncated;
                skipped_noise |= found.skipped_noise;
                for item in found.matches {
                    let line_key = (item.path.clone(), item.line);
                    if seen_symbols.contains(&line_key) {
                        continue;
                    }
                    if matches.len() >= max_matches {
                        truncated = true;
                        break 'outer;
                    }
                    let item_bytes = serde_json::to_vec(&item).map(|v| v.len()).unwrap_or(0);
                    if !matches.is_empty()
                        && match_bytes.saturating_add(item_bytes) > MAX_SEARCH_RESPONSE_BYTES
                    {
                        truncated = true;
                        truncated_by_bytes = true;
                        break 'outer;
                    }
                    match_bytes = match_bytes.saturating_add(item_bytes);
                    matches.push(item);
                }
                for item in found.counts {
                    if counts.len() >= max_matches {
                        truncated = true;
                        break 'outer;
                    }
                    let item_bytes = serde_json::to_vec(&item).map(|v| v.len()).unwrap_or(0);
                    if !counts.is_empty()
                        && count_bytes.saturating_add(item_bytes) > MAX_SEARCH_RESPONSE_BYTES
                    {
                        truncated = true;
                        truncated_by_bytes = true;
                        break 'outer;
                    }
                    count_bytes = count_bytes.saturating_add(item_bytes);
                    files.push(item.path.clone());
                    counts.push(item);
                }
            }
        }

        let mut hints = Vec::new();
        if truncated_by_bytes {
            hints.push("Search output capped at ~48 KiB inline budget; narrow path/query/exclude/context_lines or read specific line ranges.");
        }
        if explicit.is_none() && regex {
            hints.push("regex was not given and the query is written as a regular expression, so it was matched as one. Pass regex:false to match the text literally.");
        } else if !regex && request.query.contains('|') {
            hints.push("Literal mode treats '|' as an ordinary character. For alternatives such as A|B, set regex=true; a zero literal match does not establish that A or B is absent.");
        }
        if skipped_ignored && matches.is_empty() && files.is_empty() && counts.is_empty() {
            hints.push("Files that .gitignore or .ignore excludes were not searched. Pass respect_gitignore:false to search them too.");
        }
        if skipped_noise && matches.is_empty() && files.is_empty() && counts.is_empty() {
            hints.push("Default noise/dependency directories (such as node_modules, target, dist, .git) were not searched inside. Only search inside them if the user explicitly specified them or the task genuinely requires it (pass their path or name them in glob).");
        }
        let (files, total, counts) = match scan.mode {
            SearchOutput::Content => (None, None, None),
            SearchOutput::Files => (Some(files), None, None),
            SearchOutput::Count => (None, Some(counts.iter().map(|c| c.count).sum()), Some(counts)),
        };
        Ok(SearchTextResponse {
            match_mode: if regex { "regex" } else { "literal" },
            output_mode: scan.mode,
            hints,
            query: request.query,
            index_used,
            index_matches,
            text_scan_used,
            matches,
            files,
            counts,
            total,
            truncated,
        })
    }

    fn resolve_search_roots(&self, request: &SearchTextRequest) -> Result<Vec<PathBuf>> {
        let mut requested_paths = if request.paths.is_empty() {
            vec![request.path.clone()]
        } else {
            request.paths.clone()
        };

        if requested_paths.len() == 1 {
            let single = requested_paths[0].trim();
            if self.resolve_existing(single).is_err() {
                let split_paths: Vec<String> = single
                    .split_whitespace()
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
                if split_paths.len() > 1
                    && split_paths
                        .iter()
                        .all(|path| self.resolve_existing(path).is_ok())
                {
                    requested_paths = split_paths;
                }
            }
        }

        requested_paths
            .iter()
            .map(|path| self.resolve_existing(path))
            .collect()
    }

    fn search_root_filters(&self, roots: &[PathBuf]) -> Result<Vec<String>> {
        roots
            .iter()
            .map(|root| {
                self.relative_display(root)
                    .map(|path| normalize_rel_path(&path))
            })
            .collect()
    }

    pub fn write_file(&self, request: WriteFileRequest) -> Result<WriteFileResponse> {
        check_write_size(request.content.len())?;
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = crate::file_edit::workspace_path(&workspace.root, &request.path)?;
        let lock = crate::file_edit::file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        if crate::file_edit::workspace_path(&workspace.root, &request.path)? != path {
            return Err(ToolError::WriteConflict("file location changed while waiting".into()));
        }
        let original = crate::file_edit::snapshot(&path)?;
        if original.is_some() && request.expected_code_hash.is_none() {
            return Err(ToolError::WritePreconditionRequired("overwriting an existing file requires expected_code_hash from a source read; use edit_file for focused edits".into()));
        }
        crate::file_edit::check_hash(original.as_deref(), request.expected_code_hash.as_deref())?;
        if let Some(parent) = path.parent() {
            if request.create_parent_dirs {
                fs::create_dir_all(parent)?;
            } else if !parent.exists() {
                return Err(ToolError::NotFound(parent.display().to_string()));
            }
        }
        let changed =
            crate::file_edit::commit(&path, original.as_deref(), request.content.as_bytes())?;
        drop(_guard);
        let index_refresh = refresh_written_index(&workspace.root, &path, changed);
        Ok(WriteFileResponse {
            path: workspace.relative_display(&path)?,
            bytes_written: if changed { request.content.len() } else { 0 },
            go_reindexed: index_refresh["status"] == "updated" && index_refresh["language"] == "go",
            created: original.is_none(),
            changed,
            code_hash: crate::symbol_description::content_hash(request.content.as_bytes()),
            index_refresh,
        })
    }

    pub fn replace_range(&self, request: ReplaceRangeRequest) -> Result<ReplaceRangeResponse> {
        validate_line_range(request.start_line, request.end_line)?;
        check_write_size(request.replacement.len())?;
        if request.expected_old_text.is_none() && request.expected_code_hash.is_none() {
            return Err(ToolError::WritePreconditionRequired("replace_range requires expected_old_text or expected_code_hash from current source".into()));
        }
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = crate::file_edit::workspace_path(&workspace.root, &request.path)?;
        let lock = crate::file_edit::file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        if crate::file_edit::workspace_path(&workspace.root, &request.path)? != path {
            return Err(ToolError::WriteConflict("file location changed while waiting".into()));
        }
        let original = fs::read(&path)?;
        crate::file_edit::check_hash(Some(&original), request.expected_code_hash.as_deref())?;
        let content = String::from_utf8(original.clone())?;
        let updated = crate::file_edit::line_replacement(
            &content,
            request.start_line,
            request.end_line,
            &request.replacement,
            request.expected_old_text.as_deref(),
        )?;
        check_write_size(updated.len())?;
        let changed = crate::file_edit::commit(&path, Some(&original), updated.as_bytes())?;
        drop(_guard);
        let index_refresh = refresh_written_index(&workspace.root, &path, changed);
        Ok(ReplaceRangeResponse {
            path: workspace.relative_display(&path)?,
            start_line: request.start_line,
            end_line: request.end_line,
            bytes_written: if changed { updated.len() } else { 0 },
            changed,
            code_hash: crate::symbol_description::content_hash(updated.as_bytes()),
            go_reindexed: index_refresh["status"] == "updated" && index_refresh["language"] == "go",
            index_refresh,
        })
    }

    pub fn edit_file(&self, request: EditFileRequest) -> Result<WriteFileResponse> {
        let edit_size = request
            .edits
            .iter()
            .try_fold(0usize, |size, edit| {
                size.checked_add(edit.old_text.len())
                    .and_then(|size| size.checked_add(edit.new_text.len()))
            })
            .ok_or(ToolError::ContentTooLarge {
                actual: usize::MAX,
                limit: DEFAULT_WRITE_LIMIT_BYTES,
            })?;
        check_write_size(edit_size)?;
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = crate::file_edit::workspace_path(&workspace.root, &request.path)?;
        let lock = crate::file_edit::file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        if crate::file_edit::workspace_path(&workspace.root, &request.path)? != path {
            return Err(ToolError::WriteConflict("file location changed while waiting".into()));
        }
        let original = fs::read(&path)?;
        crate::file_edit::check_hash(Some(&original), request.expected_code_hash.as_deref())?;
        let content = String::from_utf8(original.clone())?;
        let updated = crate::file_edit::text_edits(&content, &request.edits)?;
        check_write_size(updated.len())?;
        let changed = crate::file_edit::commit(&path, Some(&original), updated.as_bytes())?;
        drop(_guard);
        let index_refresh = refresh_written_index(&workspace.root, &path, changed);
        Ok(WriteFileResponse {
            path: workspace.relative_display(&path)?,
            bytes_written: if changed { updated.len() } else { 0 },
            created: false,
            changed,
            code_hash: crate::symbol_description::content_hash(updated.as_bytes()),
            go_reindexed: index_refresh["status"] == "updated" && index_refresh["language"] == "go",
            index_refresh,
        })
    }

    pub fn index_go_workspace(&self, request: IndexGoWorkspaceRequest) -> Result<GoIndexResult> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        let result = go_index::index_workspace(&workspace.root).map_err(map_go_index_error)?;
        Ok(GoIndexResult {
            index_path: result.index_path,
            files_indexed: result.files_indexed,
            symbols_indexed: result.symbols_indexed,
            generated_at_unix: result.generated_at_unix,
        })
    }

    pub fn go_index_status(
        &self,
        request: IndexGoWorkspaceRequest,
    ) -> Result<go_index::GoIndexStatus> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        Ok(go_index::status(&workspace.root))
    }

    pub fn list_go_symbols(
        &self,
        request: ListGoSymbolsRequest,
    ) -> Result<go_index::ListGoSymbolsResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        go_index::list_symbols(&workspace.root, request).map_err(map_go_index_error)
    }

    pub fn search_go_symbols(
        &self,
        request: SearchGoSymbolsRequest,
    ) -> Result<go_index::SearchGoSymbolsResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        go_index::search_symbols(&workspace.root, request).map_err(map_go_index_error)
    }

    pub fn read_go_symbol(
        &self,
        request: ReadGoSymbolRequest,
    ) -> Result<go_index::ReadGoSymbolResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        go_index::read_symbol(&workspace.root, request).map_err(map_go_index_error)
    }

    pub fn index_rust_workspace(
        &self,
        request: IndexRustWorkspaceRequest,
    ) -> Result<RustIndexResult> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        let result = rust_index::index_workspace(&workspace.root).map_err(map_rust_index_error)?;
        Ok(RustIndexResult {
            index_path: result.index_path,
            files_indexed: result.files_indexed,
            symbols_indexed: result.symbols_indexed,
            generated_at_unix: result.generated_at_unix,
        })
    }

    pub fn rust_index_status(
        &self,
        request: IndexRustWorkspaceRequest,
    ) -> Result<rust_index::RustIndexStatus> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        Ok(rust_index::status(&workspace.root))
    }

    pub fn list_rust_symbols(
        &self,
        request: ListRustSymbolsRequest,
    ) -> Result<rust_index::ListRustSymbolsResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        rust_index::list_symbols(&workspace.root, request).map_err(map_rust_index_error)
    }

    pub fn search_rust_symbols(
        &self,
        request: SearchRustSymbolsRequest,
    ) -> Result<rust_index::SearchRustSymbolsResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        rust_index::search_symbols(&workspace.root, request).map_err(map_rust_index_error)
    }

    pub fn read_rust_symbol(
        &self,
        request: ReadRustSymbolRequest,
    ) -> Result<rust_index::ReadRustSymbolResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        rust_index::read_symbol(&workspace.root, request).map_err(map_rust_index_error)
    }

    pub fn index_ts_workspace(&self, request: IndexTsWorkspaceRequest) -> Result<TsIndexResult> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        let result = ts_index::index_workspace(&workspace.root).map_err(map_ts_index_error)?;
        Ok(TsIndexResult {
            index_path: result.index_path,
            files_indexed: result.files_indexed,
            symbols_indexed: result.symbols_indexed,
            generated_at_unix: result.generated_at_unix,
        })
    }

    pub fn ts_index_status(
        &self,
        request: IndexTsWorkspaceRequest,
    ) -> Result<ts_index::TsIndexStatus> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        Ok(ts_index::status(&workspace.root))
    }

    pub fn list_ts_symbols(
        &self,
        request: ListTsSymbolsRequest,
    ) -> Result<ts_index::ListTsSymbolsResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        ts_index::list_symbols(&workspace.root, request).map_err(map_ts_index_error)
    }

    pub fn search_ts_symbols(
        &self,
        request: SearchTsSymbolsRequest,
    ) -> Result<ts_index::SearchTsSymbolsResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        ts_index::search_symbols(&workspace.root, request).map_err(map_ts_index_error)
    }

    pub fn read_ts_symbol(
        &self,
        request: ReadTsSymbolRequest,
    ) -> Result<ts_index::ReadTsSymbolResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        ts_index::read_symbol(&workspace.root, request).map_err(map_ts_index_error)
    }

    pub fn index_python_workspace(
        &self,
        request: IndexPythonWorkspaceRequest,
    ) -> Result<PythonIndexResult> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        let result =
            python_index::index_workspace(&workspace.root).map_err(map_python_index_error)?;
        Ok(PythonIndexResult {
            index_path: result.index_path,
            files_indexed: result.files_indexed,
            symbols_indexed: result.symbols_indexed,
            generated_at_unix: result.generated_at_unix,
        })
    }

    pub fn python_index_status(
        &self,
        request: IndexPythonWorkspaceRequest,
    ) -> Result<python_index::PythonIndexStatus> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        Ok(python_index::status(&workspace.root))
    }

    pub fn list_python_symbols(
        &self,
        request: ListPythonSymbolsRequest,
    ) -> Result<python_index::ListPythonSymbolsResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        python_index::list_symbols(&workspace.root, request).map_err(map_python_index_error)
    }

    pub fn search_python_symbols(
        &self,
        request: SearchPythonSymbolsRequest,
    ) -> Result<python_index::SearchPythonSymbolsResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        python_index::search_symbols(&workspace.root, request).map_err(map_python_index_error)
    }

    pub fn read_python_symbol(
        &self,
        request: ReadPythonSymbolRequest,
    ) -> Result<python_index::ReadPythonSymbolResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        python_index::read_symbol(&workspace.root, request).map_err(map_python_index_error)
    }

    pub fn record_work_memory(
        &self,
        request: RecordWorkMemoryRequest,
    ) -> Result<memory::RecordWorkMemoryResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::record(
            &self.root,
            RecordWorkMemoryRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub fn list_work_memory(
        &self,
        request: ListWorkMemoryRequest,
    ) -> Result<memory::ListWorkMemoryResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::list(
            &self.root,
            ListWorkMemoryRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub fn search_work_memory(
        &self,
        request: SearchWorkMemoryRequest,
    ) -> Result<memory::SearchWorkMemoryResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::search(
            &self.root,
            SearchWorkMemoryRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub fn record_architecture_memory(
        &self,
        request: RecordArchitectureMemoryRequest,
    ) -> Result<memory::RecordArchitectureMemoryResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::record_architecture(
            &self.root,
            RecordArchitectureMemoryRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub fn list_architecture_memory(
        &self,
        request: ListArchitectureMemoryRequest,
    ) -> Result<memory::ListArchitectureMemoryResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::list_architecture(
            &self.root,
            ListArchitectureMemoryRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub fn search_architecture_memory(
        &self,
        request: SearchArchitectureMemoryRequest,
    ) -> Result<memory::SearchArchitectureMemoryResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::search_architecture(
            &self.root,
            SearchArchitectureMemoryRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub fn record_symbol_business_context(
        &self,
        request: RecordSymbolBusinessContextRequest,
    ) -> Result<memory::RecordSymbolBusinessContextResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::record_symbol_business_context(
            &self.root,
            RecordSymbolBusinessContextRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub fn list_symbol_business_context(
        &self,
        request: ListSymbolBusinessContextRequest,
    ) -> Result<memory::ListSymbolBusinessContextResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::list_symbol_business_context(
            &self.root,
            ListSymbolBusinessContextRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub fn search_symbol_business_context(
        &self,
        request: SearchSymbolBusinessContextRequest,
    ) -> Result<memory::SearchSymbolBusinessContextResponse> {
        let workspace = self.with_root(Some(&request.workspace_root))?;
        memory::search_symbol_business_context(
            &self.root,
            SearchSymbolBusinessContextRequest {
                workspace_root: workspace.root.display().to_string(),
                ..request
            },
        )
        .map_err(map_memory_error)
    }

    pub(crate) fn with_root(&self, raw_root: Option<&str>) -> Result<Self> {
        match raw_root {
            Some(root) if !root.trim().is_empty() => {
                let root = root.trim();
                if !Path::new(root).is_absolute() {
                    return Err(ToolError::WorkspaceRootMustBeAbsolute(root.to_string()));
                }
                Self::new(root)
            }
            _ => Ok(Self {
                root: self.root.clone(),
            }),
        }
    }

    fn resolve_existing(&self, raw: &str) -> Result<PathBuf> {
        let candidate = self.resolve_for_write(raw)?;
        if !candidate.exists() {
            let hint = self.not_found_hint(raw, &candidate);
            let msg = match hint {
                Some(h) => format!("{raw} ({h})"),
                None => raw.to_string(),
            };
            return Err(ToolError::NotFound(msg));
        }
        candidate.canonicalize().map_err(ToolError::from)
    }

    fn not_found_hint(&self, raw: &str, candidate: &Path) -> Option<String> {
        // 1. Check directory-module equivalents: `foo/bar.rs` -> `foo/bar/mod.rs`,
        //    `foo/bar.ts` -> `foo/bar/index.ts`, etc.
        if let Some(ext) = candidate.extension().and_then(|e| e.to_str()) {
            let stem_dir = candidate.with_extension("");
            if stem_dir.is_dir() {
                let candidates: &[&str] = match ext {
                    "rs" => &["mod.rs"],
                    "ts" | "tsx" | "js" | "jsx" => &["index.ts", "index.tsx", "index.js", "index.jsx"],
                    "py" => &["__init__.py"],
                    _ => &[],
                };
                for entry in candidates {
                    let alt = stem_dir.join(entry);
                    if alt.is_file() {
                        let display = alt
                            .strip_prefix(&self.root)
                            .ok()
                            .map(|p| p.to_string_lossy().replace('\\', "/"))
                            .unwrap_or_else(|| alt.display().to_string());
                        return Some(format!("found directory module: {display}"));
                    }
                }
            }
        }
        // 2. Check close sibling names inside the existing parent directory.
        let parent = candidate.parent()?;
        let wanted = candidate.file_stem().and_then(|s| s.to_str()).unwrap_or(raw).to_ascii_lowercase();
        if !parent.is_dir() || wanted.is_empty() {
            return None;
        }
        let mut matches: Vec<String> = std::fs::read_dir(parent)
            .ok()?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let lower = name.to_ascii_lowercase();
                if lower.starts_with(&wanted) || lower.contains(&wanted) || wanted.contains(lower.trim_end_matches(".rs")) {
                    let suffix = if e.path().is_dir() { "/" } else { "" };
                    Some(format!("{name}{suffix}"))
                } else {
                    None
                }
            })
            .collect();
        matches.sort();
        matches.truncate(5);
        if matches.is_empty() {
            None
        } else {
            Some(format!("similar in parent dir: {}", matches.join(", ")))
        }
    }

    fn resolve_existing_file(&self, raw: &str) -> Result<PathBuf> {
        let path = self.resolve_existing(raw)?;
        if !path.is_file() {
            return Err(ToolError::NotFile(raw.to_string()));
        }
        Ok(path)
    }

    fn resolve_for_write(&self, raw: &str) -> Result<PathBuf> {
        let path = Path::new(raw);
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        let relative = sanitize_relative_path(raw)?;
        Ok(self.root.join(relative))
    }

    fn relative_display(&self, path: &Path) -> Result<String> {
        let value = if let Ok(relative) = path.strip_prefix(&self.root) {
            if relative.as_os_str().is_empty() {
                ".".to_string()
            } else {
                relative.to_string_lossy().replace('\\', "/")
            }
        } else {
            path.to_string_lossy().replace('\\', "/")
        };
        Ok(value)
    }
}

fn sanitize_relative_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(ToolError::AbsolutePath(raw.to_string()));
    }

    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => clean.push(value),
            Component::ParentDir => return Err(ToolError::ParentTraversal(raw.to_string())),
            Component::Prefix(_) | Component::RootDir => {
                return Err(ToolError::AbsolutePath(raw.to_string()));
            }
        }
    }
    Ok(clean)
}

struct GitOutput {
    stdout: String,
}

fn validate_git_arg(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') || value.contains('\0') {
        return Err(ToolError::InvalidParameter(format!("{label} is invalid")));
    }
    Ok(())
}

fn git_paths(paths: &[String]) -> Result<Vec<String>> {
    paths.iter().map(|path| {
        let clean = sanitize_relative_path(path)?;
        if clean.as_os_str().is_empty() {
            return Err(ToolError::InvalidParameter("git path must not be empty".to_string()));
        }
        Ok(clean.to_string_lossy().replace('\\', "/"))
    }).collect()
}

fn run_git(root: &Path, args: &[&str]) -> Result<GitOutput> {
    let output = Command::new("git")
        .args(["-c", "core.quotepath=off", "-c", "color.ui=never", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(ToolError::from)?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        return Err(ToolError::InvalidParameter(format!("git {} failed: {}", args.first().copied().unwrap_or("command"), stderr)));
    }
    Ok(GitOutput { stdout })
}

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(run_git(root, args)?.stdout.trim().to_owned())
}

fn validate_line_range(start: usize, end: usize) -> Result<()> {
    if start == 0 || end == 0 || start > end {
        Err(ToolError::InvalidLineRange { start, end })
    } else {
        Ok(())
    }
}

fn resolve_line_bounds(
    total_lines: usize,
    start_line: isize,
    end_line: isize,
) -> Result<Option<(usize, usize, bool)>> {
    if start_line == 0 || end_line == 0 {
        return Err(ToolError::InvalidParameter(
            "start_line and end_line must be non-zero (1-based positive or negative from EOF)"
                .to_string(),
        ));
    }
    if total_lines == 0 {
        if start_line < 0 || (start_line == 1 && (end_line > 0 || end_line == -1)) {
            return Ok(None);
        }
        return Err(ToolError::InvalidEdit(format!(
            "source_range_out_of_bounds: start_line={start_line}, total_lines=0; no source returned"
        )));
    }
    let start = if start_line > 0 {
        start_line as usize
    } else {
        total_lines.saturating_sub(start_line.unsigned_abs()) + 1
    };
    let (end_raw, from_eof) = if end_line > 0 {
        (end_line as usize, false)
    } else {
        let back = end_line.unsigned_abs();
        if back > total_lines {
            return Err(ToolError::InvalidParameter(format!(
                "invalid line range: start_line={start_line}, end_line={end_line} before start of {total_lines}-line file"
            )));
        }
        (total_lines - back + 1, true)
    };
    if start > end_raw {
        return Err(ToolError::InvalidParameter(format!(
            "invalid line range: start_line={start_line} resolves after end_line={end_line}"
        )));
    }
    if start > total_lines {
        return Err(ToolError::InvalidEdit(format!(
            "source_range_out_of_bounds: start_line={start_line}, total_lines={total_lines}; no source returned"
        )));
    }
    let end = end_raw.min(total_lines);
    let clamped = !from_eof && end < end_raw;
    Ok(Some((start, end, clamped)))
}

fn fnv1a_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

fn read_large_file_lines(
    path: &Path,
    rel_path: &str,
    start_line: isize,
    end_line: isize,
) -> Result<ReadFileLinesResponse> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut hash = 0xcbf29ce484222325u64;
    let mut total_lines: usize = 0;

    let tail_cap = (start_line < 0).then(|| start_line.unsigned_abs());
    let pos_start = (start_line > 0).then(|| start_line as usize);
    let pos_end = (end_line > 0).then(|| end_line as usize);
    let mut tail_ring: VecDeque<(usize, String)> = VecDeque::new();
    let mut forward_kept: Vec<(usize, String)> = Vec::new();
    let mut kept_bytes: usize = 0;

    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        hash = fnv1a_update(hash, &buf);
        total_lines += 1;
        let line_no = total_lines;

        if let Some(cap) = tail_cap {
            let text = String::from_utf8(std::mem::take(&mut buf))?;
            if tail_ring.len() == cap
                && let Some((_, dropped)) = tail_ring.pop_front()
            {
                kept_bytes = kept_bytes.saturating_sub(dropped.len());
            }
            kept_bytes += text.len();
            if kept_bytes > crate::source_read::FILE_LIMIT {
                return Err(ToolError::ContentTooLarge {
                    actual: kept_bytes,
                    limit: crate::source_read::FILE_LIMIT,
                });
            }
            tail_ring.push_back((line_no, text));
        } else if let Some(s) = pos_start {
            let in_window = line_no >= s && pos_end.is_none_or(|e| line_no <= e);
            if in_window {
                let text = String::from_utf8(std::mem::take(&mut buf))?;
                kept_bytes += text.len();
                if kept_bytes > crate::source_read::FILE_LIMIT {
                    return Err(ToolError::ContentTooLarge {
                        actual: kept_bytes,
                        limit: crate::source_read::FILE_LIMIT,
                    });
                }
                forward_kept.push((line_no, text));
            }
        }
    }

    let code_hash = format!("fnv1a64:{hash:016x}");
    let Some((resolved_start, resolved_end, end_clamped_to_eof)) =
        resolve_line_bounds(total_lines, start_line, end_line)?
    else {
        return Ok(ReadFileLinesResponse {
            code_hash,
            path: rel_path.to_string(),
            start_line: 0,
            end_line: 0,
            lines: Vec::new(),
            content: String::new(),
            total_lines: 0,
            requested_end_line: end_line,
            end_clamped_to_eof: false,
        });
    };

    let selected: Vec<(usize, String)> = if tail_cap.is_some() {
        tail_ring
            .into_iter()
            .filter(|(line_no, _)| *line_no >= resolved_start && *line_no <= resolved_end)
            .collect()
    } else {
        forward_kept
            .into_iter()
            .filter(|(line_no, _)| *line_no >= resolved_start && *line_no <= resolved_end)
            .collect()
    };

    let mut content = String::new();
    let mut lines = Vec::with_capacity(selected.len());
    for (line_no, raw) in selected {
        content.push_str(&raw);
        let stripped = raw.trim_end_matches(['\n', '\r']).to_string();
        lines.push(LineContent {
            line: line_no,
            text: stripped,
        });
    }

    Ok(ReadFileLinesResponse {
        code_hash,
        path: rel_path.to_string(),
        start_line: resolved_start,
        end_line: resolved_end,
        lines,
        content,
        total_lines,
        requested_end_line: end_line,
        end_clamped_to_eof,
    })
}

fn stream_file_chunk(
    path: &Path,
    rel_path: &str,
    byte_offset: u64,
    start_line: Option<usize>,
    max_bytes: usize,
    max_lines: Option<usize>,
) -> Result<ReadFileChunkResponse> {
    let mut file = fs::File::open(path)?;
    let file_bytes = file.metadata()?.len();
    if byte_offset > file_bytes {
        return Err(ToolError::InvalidParameter(format!(
            "byte_offset {byte_offset} exceeds file length {file_bytes}"
        )));
    }
    if byte_offset == file_bytes {
        return Ok(ReadFileChunkResponse {
            path: rel_path.to_string(),
            file_bytes,
            start_byte: byte_offset,
            end_byte: byte_offset,
            next_byte_offset: None,
            start_line: None,
            end_line: None,
            next_line: None,
            eof: true,
            line_truncated: false,
            bytes: 0,
            line_count: 0,
            content: String::new(),
            lines: Vec::new(),
        });
    }

    let max_bytes = max_bytes.clamp(4, MAX_CHUNK_BYTES);
    file.seek(SeekFrom::Start(byte_offset))?;
    let mut reader = BufReader::new(file);
    let mut raw_line = Vec::new();
    let mut content = String::new();
    let mut lines = Vec::new();
    let mut consumed: u64 = 0;
    let mut current_line = start_line.or_else(|| (byte_offset == 0).then_some(1));
    let mut line_truncated = false;

    loop {
        if max_lines.is_some_and(|cap| lines.len() >= cap) {
            break;
        }
        raw_line.clear();
        let n = reader.read_until(b'\n', &mut raw_line)?;
        if n == 0 {
            break;
        }
        if !lines.is_empty() && content.len() + raw_line.len() > max_bytes {
            break;
        }
        if raw_line.len() > max_bytes {
            let prefix_bytes = &raw_line[..max_bytes];
            let valid_len = match std::str::from_utf8(prefix_bytes) {
                Ok(_) => prefix_bytes.len(),
                Err(err) => err.valid_up_to(),
            };
            if valid_len == 0 {
                return Err(ToolError::InvalidParameter(
                    "invalid UTF-8 sequence at chunk boundary".to_string(),
                ));
            }
            let prefix = std::str::from_utf8(&raw_line[..valid_len])
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            content.push_str(prefix);
            lines.push(ChunkLine {
                line: current_line,
                text: prefix.trim_end_matches('\r').to_string(),
            });
            consumed += prefix.len() as u64;
            line_truncated = true;
            break;
        }
        let text = String::from_utf8(std::mem::take(&mut raw_line))?;
        consumed += text.len() as u64;
        content.push_str(&text);
        lines.push(ChunkLine {
            line: current_line,
            text: text.trim_end_matches(['\n', '\r']).to_string(),
        });
        current_line = current_line.map(|line| line + 1);
    }

    let end_byte = byte_offset + consumed;
    let eof = end_byte >= file_bytes;
    let next_byte_offset = (!eof).then_some(end_byte);
    let next_line = if eof { None } else { current_line };
    let start_line_out = lines.first().and_then(|line| line.line);
    let end_line_out = lines.last().and_then(|line| line.line);
    let line_count = lines.len();
    let bytes = content.len();

    Ok(ReadFileChunkResponse {
        path: rel_path.to_string(),
        file_bytes,
        start_byte: byte_offset,
        end_byte,
        next_byte_offset,
        start_line: start_line_out,
        end_line: end_line_out,
        next_line,
        eof,
        line_truncated,
        bytes,
        line_count,
        content,
        lines,
    })
}

fn check_write_size(size: usize) -> Result<()> {
    if size > DEFAULT_WRITE_LIMIT_BYTES {
        return Err(ToolError::ContentTooLarge {
            actual: size,
            limit: DEFAULT_WRITE_LIMIT_BYTES,
        });
    }
    Ok(())
}

fn refresh_written_index(root: &Path, path: &Path, changed: bool) -> serde_json::Value {
    use serde_json::json;
    if !changed {
        return json!({"status":"unchanged"});
    }
    let (language, table, result) = match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => (
            "rust",
            "rust_symbols",
            rust_index::maybe_reindex_after_write(root, path)
                .map(|value| value.is_some())
                .map_err(|error| error.to_string()),
        ),
        Some("go") => (
            "go",
            "go_symbols",
            go_index::maybe_reindex_after_write(root, path)
                .map(|value| value.is_some())
                .map_err(|error| error.to_string()),
        ),
        Some("py") => (
            "python",
            "python_symbols",
            python_index::maybe_reindex_after_write(root, path)
                .map(|value| value.is_some())
                .map_err(|error| error.to_string()),
        ),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs") => (
            "ts",
            "ts_symbols",
            ts_index::maybe_reindex_after_write(root, path)
                .map(|value| value.is_some())
                .map_err(|error| error.to_string()),
        ),
        _ => return json!({"status":"not_applicable"}),
    };
    match result {
        Ok(true) => {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let file_status=crate::database::init_db(root).ok().and_then(|conn|conn.query_row(
                "SELECT status,error FROM symbol_index_files WHERE workspace_root=?1 AND language=?2 AND file_path=?3",
                rusqlite::params![root.to_string_lossy(),language,relative],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).ok());
            match file_status {
                Some((status, error)) => {
                    json!({"status":if status=="indexed" {"updated"} else {"incomplete"},"language":language,"scope":"changed_file","file_status":status,"error":error,"index":crate::symbol_index_state::health(root,language,table).ok()})
                }
                None => {
                    json!({"status":"excluded","language":language,"scope":"changed_file","guidance":"File saved but not included by the index ignore rules."})
                }
            }
        }
        Ok(false) => json!({"status":"not_initialized","language":language}),
        Err(error) => {
            tracing::warn!(%language,%error,"file saved but index refresh failed");
            json!({"status":"failed","language":language,"error":error,"file_saved":true})
        }
    }
}

fn entry_kind(metadata: &fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

fn default_dot() -> String {
    ".".to_string()
}

fn default_true() -> bool {
    true
}

fn default_git_unified() -> usize {
    3
}

fn default_max_read_bytes() -> u64 {
    DEFAULT_MAX_READ_BYTES
}

fn default_max_matches() -> usize {
    DEFAULT_MAX_MATCHES
}

fn split_pipe_patterns(patterns: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in patterns {
        let mut start = 0usize;
        let mut brace_depth = 0usize;
        let mut escaped = false;
        for (idx, ch) in raw.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '{' => brace_depth += 1,
                '}' => brace_depth = brace_depth.saturating_sub(1),
                '|' if brace_depth == 0 => {
                    let part = raw[start..idx].trim();
                    if !part.is_empty() {
                        out.push(part.to_owned());
                    }
                    start = idx + 1;
                }
                _ => {}
            }
        }
        let tail = raw[start..].trim();
        if !tail.is_empty() {
            out.push(tail.to_owned());
        }
    }
    out
}

fn allowed_noise_dirs(globs: &[String]) -> std::collections::HashSet<&'static str> {
    let mut allowed = std::collections::HashSet::new();
    for trimmed in split_pipe_patterns(globs) {
        if trimmed.is_empty() || trimmed.starts_with('!') {
            continue;
        }
        for &noise in NOISE_DIRS {
            if trimmed.split('/').any(|seg| seg == noise) {
                allowed.insert(noise);
            }
        }
    }
    allowed
}

fn format_modified(metadata: &fs::Metadata) -> (Option<String>, Option<i64>) {
    let Ok(time) = metadata.modified() else {
        return (None, None);
    };
    let dt: chrono::DateTime<chrono::Utc> = time.into();
    (
        Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        Some(dt.timestamp_millis()),
    )
}

fn parse_relative_duration_ms(raw: &str) -> Option<i64> {
    let body = raw.strip_prefix('-').unwrap_or(raw);
    if body.len() < 2 {
        return None;
    }
    let (digits, unit) = body.split_at(body.len() - 1);
    let value: i64 = digits.parse().ok()?;
    if value < 0 {
        return None;
    }
    let multiplier = match unit {
        "s" => 1_000i64,
        "m" => 60 * 1_000i64,
        "h" => 60 * 60 * 1_000i64,
        "d" => 24 * 60 * 60 * 1_000i64,
        "w" => 7 * 24 * 60 * 60 * 1_000i64,
        _ => return None,
    };
    value.checked_mul(multiplier)
}

fn parse_timestamp_ms(raw: &str, inclusive_end: bool) -> Result<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ToolError::InvalidParameter("timestamp cannot be empty".into()));
    }
    if let Ok(num) = trimmed.parse::<i64>() {
        return if num.abs() >= 100_000_000_000 {
            Ok(num)
        } else {
            num.checked_mul(1_000)
                .ok_or_else(|| ToolError::InvalidParameter(format!("timestamp out of range: {raw}")))
        };
    }
    if let Some(delta_ms) = parse_relative_duration_ms(trimmed) {
        return Ok(chrono::Utc::now().timestamp_millis() - delta_ms);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.timestamp_millis());
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(ndt.and_utc().timestamp_millis());
        }
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let ndt = if inclusive_end {
            date.and_hms_milli_opt(23, 59, 59, 999)
        } else {
            date.and_hms_opt(0, 0, 0)
        }
        .ok_or_else(|| ToolError::InvalidParameter(format!("invalid date: {raw}")))?;
        return Ok(ndt.and_utc().timestamp_millis());
    }
    Err(ToolError::InvalidParameter(format!(
        "invalid timestamp {raw:?}; use RFC3339, YYYY-MM-DD, epoch seconds/ms, or e.g. 24h / 7d"
    )))
}

fn map_python_index_error(error: python_index::PythonIndexError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

fn map_go_index_error(error: go_index::GoIndexError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

fn map_memory_error(error: memory::MemoryError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

struct DirEntryFilter<'a> {
    workspace: &'a Workspace,
    kind: Option<EntryKind>,
    globs: Option<&'a CompiledGlobs>,
    modified_after: Option<i64>,
    modified_before: Option<i64>,
}

/// Applies caller filters to a path and records matching metadata in `entries`.
fn push_matching_dir_entry(
    filter: &DirEntryFilter<'_>,
    path: &Path,
    entries: &mut Vec<DirEntryInfo>,
) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    let kind = entry_kind(&metadata);
    if filter.kind.is_some_and(|want| want != kind) {
        return Ok(());
    }
    if let Some(globs) = filter.globs
        && !globs.matches(path, metadata.is_dir())
    {
        return Ok(());
    }
    let (modified_at, modified_ms) = format_modified(&metadata);
    if let Some(after) = filter.modified_after
        && !modified_ms.is_some_and(|ms| ms >= after)
    {
        return Ok(());
    }
    if let Some(before) = filter.modified_before
        && !modified_ms.is_some_and(|ms| ms <= before)
    {
        return Ok(());
    }
    entries.push(DirEntryInfo {
        path: filter.workspace.relative_display(path)?,
        kind,
        size_bytes: metadata.is_file().then_some(metadata.len()),
        modified_at,
        modified_ms,
    });
    Ok(())
}

/// Returns `true` when `entry` is a nested default noise directory whose subtree should stay unexpanded.
fn is_skipped_noise_dir(
    entry: &ignore::DirEntry,
    allowed_noise: &std::collections::HashSet<&'static str>,
) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_some_and(|ft| ft.is_dir()) {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| NOISE_DIRS.contains(&name) && !allowed_noise.contains(name))
}

fn list_dir_hints(unexpanded_dirs: &[String]) -> Vec<String> {
    if unexpanded_dirs.is_empty() {
        return Vec::new();
    }
    let preview = unexpanded_dirs
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let extra = unexpanded_dirs.len().saturating_sub(5);
    let suffix = if extra > 0 {
        format!(" (+{extra} more in unexpanded_dirs)")
    } else {
        String::new()
    };
    vec![format!(
        "Default noise/dependency directories were not traversed inside: {preview}{suffix} (directory entries remain visible when matched). Do not inspect inside these directories unless the user explicitly specified them or the task genuinely requires it (if needed, pass the directory path or name it in glob)."
    )]
}

#[derive(Clone)]
struct ScanOptions {
    mode: SearchOutput,
    respect_gitignore: bool,
    /// Matches in content mode, files otherwise.
    limit: usize,
    max_matches_per_file: Option<usize>,
    context_lines: usize,
    globs: Option<CompiledGlobs>,
    allowed_noise: std::collections::HashSet<&'static str>,
}

#[derive(Default)]
struct ScanFound {
    matches: Vec<TextMatch>,
    /// Files and counts modes: every matching file with its matching lines.
    counts: Vec<FileCount>,
    truncated: bool,
    skipped_noise: bool,
}

#[derive(Clone)]
struct CompiledGlobs {
    workspace_root: PathBuf,
    include: Option<ignore::overrides::Override>,
    exclude: Option<ignore::overrides::Override>,
}

impl CompiledGlobs {
    fn is_excluded(&self, path: &Path, is_dir: bool) -> bool {
        let Some(exclude) = &self.exclude else {
            return false;
        };
        if exclude.matched(path, is_dir).is_whitelist() {
            return true;
        }
        let mut cursor = path.parent();
        while let Some(dir) = cursor {
            if dir == self.workspace_root || !dir.starts_with(&self.workspace_root) {
                break;
            }
            if exclude.matched(dir, true).is_whitelist() {
                return true;
            }
            cursor = dir.parent();
        }
        false
    }

    fn matches(&self, path: &Path, is_dir: bool) -> bool {
        if self.is_excluded(path, is_dir) {
            return false;
        }
        match &self.include {
            Some(include) => include.matched(path, is_dir).is_whitelist(),
            None => true,
        }
    }
}

/// Globs as `rg -g` reads them, rooted at the workspace so `src/**/*.ts`
/// means the same from any search path, with `|` splitting outside `{...}`
/// and explicit `exclude` patterns taking strict priority over `glob`.
fn search_globs(
    root: &Path,
    globs: &[String],
    excludes: &[String],
) -> Result<Option<CompiledGlobs>> {
    let mut include_patterns = Vec::new();
    let mut exclude_patterns = Vec::new();
    for pattern in split_pipe_patterns(globs) {
        if let Some(neg) = pattern.strip_prefix('!') {
            let neg = neg.trim();
            if !neg.is_empty() {
                exclude_patterns.push(neg.to_owned());
            }
        } else {
            include_patterns.push(pattern);
        }
    }
    for pattern in split_pipe_patterns(excludes) {
        let pat = pattern.strip_prefix('!').unwrap_or(&pattern).trim();
        if !pat.is_empty() {
            exclude_patterns.push(pat.to_owned());
        }
    }
    if include_patterns.is_empty() && exclude_patterns.is_empty() {
        return Ok(None);
    }
    let build_override = |patterns: &[String]| -> Result<Option<ignore::overrides::Override>> {
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut builder = ignore::overrides::OverrideBuilder::new(root);
        for pat in patterns {
            builder
                .add(pat)
                .map_err(|error| ToolError::InvalidSearchPattern(format!("glob {pat}: {error}")))?;
        }
        Ok(Some(
            builder
                .build()
                .map_err(|error| ToolError::InvalidSearchPattern(error.to_string()))?,
        ))
    };
    Ok(Some(CompiledGlobs {
        workspace_root: root.to_path_buf(),
        include: build_override(&include_patterns)?,
        exclude: build_override(&exclude_patterns)?,
    }))
}

/// Whether a path being searched is itself excluded by the workspace's
/// top-level .gitignore or .ignore, alone or through a parent folder.
fn ignored_target(workspace_root: &Path, target: &Path) -> bool {
    if target == workspace_root || !target.starts_with(workspace_root) {
        return false;
    }
    let mut rules = ignore::gitignore::GitignoreBuilder::new(workspace_root);
    for name in [".gitignore", ".ignore"] {
        let file = workspace_root.join(name);
        if file.is_file() {
            rules.add(file);
        }
    }
    rules.build().is_ok_and(|rules| rules.matched_path_or_any_parents(target, target.is_dir()).is_ignore())
}

fn scan_text_root(
    workspace_root: PathBuf,
    root: PathBuf,
    matcher: regex::Regex,
    scan: &ScanOptions,
) -> ScanFound {
    let mut found = ScanFound::default();
    let skipped_noise = Arc::new(AtomicBool::new(false));
    let skipped_noise_flag = Arc::clone(&skipped_noise);
    let allowed_noise = scan.allowed_noise.clone();
    let filter_globs = scan.globs.clone();

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .ignore(scan.respect_gitignore)
        .git_ignore(scan.respect_gitignore)
        .git_exclude(scan.respect_gitignore)
        .parents(scan.respect_gitignore)
        .filter_entry(move |entry| {
            if filter_globs.as_ref().is_some_and(|g| {
                g.is_excluded(
                    entry.path(),
                    entry.file_type().is_some_and(|ft| ft.is_dir()),
                )
            }) {
                return false;
            }
            if is_skipped_noise_dir(entry, &allowed_noise) {
                skipped_noise_flag.store(true, Ordering::Relaxed);
                return false;
            }
            true
        });

    'outer: for item in builder.build() {
        let entry = match item {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if let Some(globs) = &scan.globs
            && !globs.matches(path, false)
        {
            continue;
        }
        if crate::database::check_source_access(path).is_err() {
            continue;
        }
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        if metadata.len() > DEFAULT_SEARCH_FILE_LIMIT_BYTES {
            continue;
        }
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        // Paths use `/` on every platform, like list_dir and the
        // symbol index matches this list is merged with.
        let rel_path = path
            .strip_prefix(&workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        if scan.mode != SearchOutput::Content {
            let count = match scan.mode {
                SearchOutput::Files => usize::from(content.lines().any(|line| matcher.is_match(line))),
                _ => content.lines().filter(|line| matcher.is_match(line)).count(),
            };
            if count > 0 {
                if found.counts.len() >= scan.limit {
                    found.truncated = true;
                    break;
                }
                found.counts.push(FileCount { path: rel_path, count });
            }
            continue;
        }
        let lines = content.lines().collect::<Vec<_>>();
        let mut file_matches = 0usize;
        for (line_index, line) in lines.iter().enumerate() {
            if let Some(found_at) = matcher.find(line) {
                if scan.max_matches_per_file.is_some_and(|cap| file_matches >= cap) {
                    found.truncated = true;
                    break;
                }
                if found.matches.len() >= scan.limit {
                    found.truncated = true;
                    break 'outer;
                }
                file_matches += 1;
                let column = line[..found_at.start()].chars().count() + 1;
                let (text, line_chars) = match_excerpt(line, column - 1);
                let near = |range: std::ops::Range<usize>| {
                    lines[range].iter().map(|line| match_excerpt(line, 0).0).collect::<Vec<_>>()
                };
                found.matches.push(TextMatch {
                    path: rel_path.clone(),
                    line: line_index + 1,
                    column,
                    text,
                    line_chars,
                    before: near(line_index.saturating_sub(scan.context_lines)..line_index),
                    after: near(line_index + 1..(line_index + 1 + scan.context_lines).min(lines.len())),
                });
            }
        }
    }

    found.skipped_noise = skipped_noise.load(Ordering::Relaxed);
    found
}

/// A matched line as it is returned: whole when short, otherwise the part
/// around the match, so one long line (minified code, a JSON log record)
/// cannot fill a reader's context. `at` is the match's character offset.
fn match_excerpt(line: &str, at: usize) -> (String, Option<usize>) {
    let total = line.chars().count();
    if total <= MATCH_TEXT_CHARS {
        return (line.to_owned(), None);
    }
    let start = at
        .saturating_sub(MATCH_CONTEXT_CHARS)
        .min(total - MATCH_TEXT_CHARS);
    (
        line.chars().skip(start).take(MATCH_TEXT_CHARS).collect(),
        Some(total),
    )
}

fn query_indexed_symbol_table(
    conn: &rusqlite::Connection,
    workspace_root: &str,
    table: &str,
    lang: &str,
    query_name: &str,
    root_filters: &[String],
    max_matches: usize,
    matches: &mut Vec<TextMatch>,
    seen_symbols: &mut std::collections::HashSet<(String, usize)>,
) -> bool {
    let sql = format!(
        "SELECT kind, file_path, start_line, signature FROM {} WHERE name = ? AND workspace_root = ?",
        table
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return false;
    };
    let Ok(mut rows) = stmt.query(rusqlite::params![query_name, workspace_root]) else {
        return false;
    };

    while let Ok(Some(row)) = rows.next() {
        let kind: String = row.get(0).unwrap_or_default();
        let file_path: String = row.get(1).unwrap_or_default();
        let start_line: usize = row.get(2).unwrap_or(0);
        let signature: String = row.get(3).unwrap_or_default();

        if !path_allowed_by_filters(&file_path, root_filters) {
            continue;
        }
        if matches.len() >= max_matches {
            return true;
        }

        matches.push(TextMatch {
            path: file_path.clone(),
            line: start_line,
            column: 1,
            text: format!("[Symbol Definition ({} {})] {}", lang, kind, signature),
            line_chars: None,
            before: Vec::new(),
            after: Vec::new(),
        });
        seen_symbols.insert((file_path, start_line));
    }

    false
}

fn normalize_rel_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

fn path_allowed_by_filters(path: &str, filters: &[String]) -> bool {
    let path = normalize_rel_path(path);
    filters
        .iter()
        .any(|filter| filter == "." || path == *filter || path.starts_with(&format!("{}/", filter)))
}

fn map_ts_index_error(error: ts_index::TsIndexError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

fn map_rust_index_error(error: rust_index::RustIndexError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("codex_workspace_mcp_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    // A separate process observes the real OS lock, not SQLite's cached view
    // of locks owned by this process. Normal test runs have nothing to probe.
    #[test]
    #[cfg(unix)]
    fn wal_lock_probe_child() {
        let Some(path) = std::env::var_os("CODEX_WAL_LOCK_PROBE_PATH") else { return };
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        match conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;") {
            Ok(()) => println!("WAL_LOCK_PROBE=unlocked"),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy => println!("WAL_LOCK_PROBE=locked"),
            Err(error) => panic!("unexpected lock probe error: {error}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn wal_lock_survives_workspace_text_search() {
        let root = temp_workspace("wal_search_lock");
        let state_dir = root.join(".codex-workspace-mcp");
        fs::create_dir_all(&state_dir).unwrap();
        let path = state_dir.join("codex_state.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE test(value); BEGIN IMMEDIATE;").unwrap();
        let locked = || {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tools::tests::wal_lock_probe_child", "--nocapture"])
                .env("CODEX_WAL_LOCK_PROBE_PATH", &path).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout).contains("WAL_LOCK_PROBE=locked")
        };
        assert!(locked(), "fixture must hold an externally visible SQLite write lock");
        fs::write(root.join("source.rs"), "fn expected_source() {}\n").unwrap();
        let matches = scan_text_root(root.clone(), root.clone(), regex::Regex::new("expected_source").unwrap(), &plain_scan()).matches;
        assert_eq!(matches.len(), 1);
        let still_locked = locked();
        if !still_locked {
            // A failed guard has invalidated this connection's locks. Avoid
            // SQLite cleanup against SHM that the probe may have reset.
            std::mem::forget(conn);
        } else {
            conn.execute_batch("ROLLBACK").unwrap();
        }
        let _ = fs::remove_dir_all(root);
        assert!(still_locked, "plain source search released the live SQLite WAL write lock");
    }

    #[test]
    fn source_tools_reject_host_sqlite_files_but_allow_saved_programs() {
        let root = temp_workspace("sqlite_source_guard");
        let state = root.join(".codex-workspace-mcp");
        fs::create_dir_all(&state).unwrap();
        let workspace = Workspace::new(&root).unwrap();
        for name in ["codex_state.db", "codex_state.db-wal", "codex_state.db-shm", "codex_state.db-journal"] {
            let path = state.join(name);
            // Text content makes an accidental raw open observable, without
            // relying on a binary decoding failure or a native crash.
            fs::write(&path, "reserved SQLite state\n").unwrap();
            let read = crate::source_read::read(&path, 1024);
            assert!(matches!(read, Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied));
            assert!(crate::file_edit::workspace_path(&root, path.to_str().unwrap()).is_err());
            assert!(crate::file_edit::snapshot(&path).is_err());
            let matches = scan_text_root(root.clone(), path, regex::Regex::new("reserved").unwrap(), &plain_scan()).matches;
            assert!(matches.is_empty(), "explicit file searches must not bypass the guard");
        }
        let programs = state.join("chats/example");
        fs::create_dir_all(&programs).unwrap();
        let program = programs.join("turn-1.js");
        fs::write(&program, "console.log('saved program');\n").unwrap();
        assert!(crate::source_read::read(&program, 1024).is_ok());
        let matches = scan_text_root(root.clone(), root.clone(), regex::Regex::new("saved program").unwrap(), &plain_scan()).matches;
        assert_eq!(matches.len(), 1);
        #[cfg(unix)] {
            let alias = root.join("state-alias.txt");
            std::os::unix::fs::symlink(state.join("codex_state.db"), &alias).unwrap();
            assert!(crate::source_read::read(&alias, 1024).is_err());
            assert!(workspace.read_file(ReadFileRequest {
                workspace_root: None, path: alias.to_string_lossy().into_owned(), max_bytes: 1024,
            }).is_err());
        }
        let _ = workspace;
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = temp_workspace("traversal");
        let workspace = Workspace::new(&root).unwrap();
        let result = workspace.read_file(ReadFileRequest {
            workspace_root: None,
            path: "../secret.txt".to_string(),
            max_bytes: 1024,
        });
        assert!(matches!(result, Err(ToolError::ParentTraversal(_))));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_absolute_paths_outside_workspace() {
        let root = temp_workspace("absolute_root");
        let workspace = Workspace::new(&root).unwrap();
        let outside = temp_workspace("absolute_outside");
        let outside_file = outside.join("log.txt");
        fs::write(&outside_file, "outside log").unwrap();

        let result = workspace
            .read_file(ReadFileRequest {
                workspace_root: None,
                path: outside_file.display().to_string(),
                max_bytes: 1024,
            })
            .unwrap();

        assert_eq!(result.content, "outside log");
        assert!(result.path.contains("log.txt"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn reads_line_ranges() {
        let root = temp_workspace("lines");
        fs::write(root.join("demo.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        let result = workspace
            .read_file_lines(ReadFileLinesRequest {
                workspace_root: None,
                path: "demo.txt".to_string(),
                start_line: 2,
                end_line: 3,
            })
            .unwrap();
        assert_eq!(result.lines.len(), 2);
        assert_eq!(result.lines[0].text, "beta");

        let tail = workspace
            .read_file_lines(ReadFileLinesRequest {
                workspace_root: None,
                path: "demo.txt".to_string(),
                start_line: -2,
                end_line: -1,
            })
            .unwrap();
        assert_eq!(tail.start_line, 2);
        assert_eq!(tail.end_line, 3);
        assert_eq!(tail.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["beta", "gamma"]);
        assert!(!tail.end_clamped_to_eof);

        // Negative start exceeding line count clamps cleanly to line 1.
        let all_tail = workspace
            .read_file_lines(ReadFileLinesRequest {
                workspace_root: None,
                path: "demo.txt".to_string(),
                start_line: -50,
                end_line: -1,
            })
            .unwrap();
        assert_eq!(all_tail.start_line, 1);
        assert_eq!(all_tail.end_line, 3);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_empty_file_line_ranges() {
        let root = temp_workspace("empty_lines");
        fs::write(root.join("empty.log"), "").unwrap();
        let workspace = Workspace::new(&root).unwrap();

        for (start_line, end_line) in [(1, 35), (1, -1), (-35, -1)] {
            let result = workspace
                .read_file_lines(ReadFileLinesRequest {
                    workspace_root: None,
                    path: "empty.log".to_string(),
                    start_line,
                    end_line,
                })
                .unwrap();
            assert_eq!(result.total_lines, 0);
            assert_eq!(result.start_line, 0);
            assert_eq!(result.end_line, 0);
            assert!(result.lines.is_empty());
            assert!(result.content.is_empty());
            assert!(!result.end_clamped_to_eof);
        }

        let err = workspace
            .read_file_lines(ReadFileLinesRequest {
                workspace_root: None,
                path: "empty.log".to_string(),
                start_line: 2,
                end_line: 10,
            })
            .unwrap_err();
        assert!(err.to_string().contains("source_range_out_of_bounds"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn streams_tail_on_file_larger_than_file_limit() {
        let root = temp_workspace("large_lines");
        let path = root.join("huge.log");
        {
            use std::io::Write;
            let mut file = fs::File::create(&path).unwrap();
            let pad = "x".repeat(250);
            for i in 1..=70_000 {
                writeln!(file, "line-{i:05} {pad}").unwrap();
            }
        }
        assert!(fs::metadata(&path).unwrap().len() > crate::source_read::FILE_LIMIT as u64);
        let workspace = Workspace::new(&root).unwrap();
        let tail = workspace
            .read_file_lines(ReadFileLinesRequest {
                workspace_root: None,
                path: "huge.log".to_string(),
                start_line: -3,
                end_line: -1,
            })
            .unwrap();
        assert_eq!(tail.total_lines, 70_000);
        assert_eq!(tail.start_line, 69_998);
        assert_eq!(tail.end_line, 70_000);
        assert!(tail.lines[0].text.starts_with("line-69998 "));
        assert!(tail.lines[2].text.starts_with("line-70000 "));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn streams_file_chunks_sequentially_and_handles_long_utf8_lines() {
        let root = temp_workspace("chunks");
        fs::write(
            root.join("stream.log"),
            "first line\nsecond line\nthird line\nfourth line\n",
        )
        .unwrap();
        let workspace = Workspace::new(&root).unwrap();
        let mut offset = 0;
        let mut line = Some(1);
        let mut collected = Vec::new();
        loop {
            let chunk = workspace
                .read_file_chunk(ReadFileChunkRequest {
                    workspace_root: None,
                    path: "stream.log".to_string(),
                    byte_offset: offset,
                    start_line: line,
                    max_bytes: Some(25),
                    max_lines: None,
                })
                .unwrap();
            assert!(!chunk.line_truncated);
            collected.extend(chunk.lines.into_iter().map(|l| (l.line, l.text)));
            if chunk.eof {
                assert!(chunk.next_byte_offset.is_none());
                assert!(chunk.next_line.is_none());
                break;
            }
            offset = chunk.next_byte_offset.unwrap();
            line = chunk.next_line;
        }
        assert_eq!(
            collected,
            vec![
                (Some(1), "first line".to_string()),
                (Some(2), "second line".to_string()),
                (Some(3), "third line".to_string()),
                (Some(4), "fourth line".to_string()),
            ]
        );

        // A single line longer than max_bytes splits on a valid UTF-8 boundary and resumes cleanly.
        let utf8_line = "中".repeat(10) + "\nend\n";
        fs::write(root.join("wide.txt"), &utf8_line).unwrap();
        let c1 = workspace
            .read_file_chunk(ReadFileChunkRequest {
                workspace_root: None,
                path: "wide.txt".to_string(),
                byte_offset: 0,
                start_line: None,
                max_bytes: Some(10),
                max_lines: None,
            })
            .unwrap();
        assert!(c1.line_truncated);
        assert_eq!(c1.content, "中中中");
        assert!(!c1.eof);
        let c2 = workspace
            .read_file_chunk(ReadFileChunkRequest {
                workspace_root: None,
                path: "wide.txt".to_string(),
                byte_offset: c1.next_byte_offset.unwrap(),
                start_line: c1.next_line,
                max_bytes: Some(64),
                max_lines: None,
            })
            .unwrap();
        assert_eq!(c2.content, "中中中中中中中\nend\n");
        assert!(c2.eof);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn replaces_range_with_expected_text() {
        let root = temp_workspace("replace");
        fs::write(root.join("demo.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        workspace
            .replace_range(ReplaceRangeRequest {
                workspace_root: None,
                path: "demo.txt".to_string(),
                start_line: 2,
                end_line: 2,
                replacement: "BETA".to_string(),
                expected_old_text: Some("beta".to_string()),
                expected_code_hash: None,
            })
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("demo.txt")).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    fn plain_scan() -> ScanOptions {
        ScanOptions { mode: SearchOutput::Content, respect_gitignore: false, limit: 100, max_matches_per_file: None, context_lines: 0, globs: None, allowed_noise: std::collections::HashSet::new() }
    }

    #[test]
    fn search_finds_text() {
        let root = temp_workspace("search");
        fs::write(root.join("demo.txt"), "alpha\nBeta\ngamma\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        let result = workspace
            .search_text(SearchTextRequest {
                workspace_root: None,
                query: "beta".to_string(),
                path: ".".to_string(),
                paths: Vec::new(),
                regex: Some(false),
                case_sensitive: false,
                respect_gitignore: Some(false),
                max_matches: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].line, 2);
        assert!(!result.index_used);
        assert_eq!(result.index_matches, 0);
        assert!(result.text_scan_used);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_accepts_multiple_paths() {
        let root = temp_workspace("search_paths");
        fs::create_dir_all(root.join("one")).unwrap();
        fs::create_dir_all(root.join("two")).unwrap();
        fs::write(root.join("one").join("a.txt"), "needle one\n").unwrap();
        fs::write(root.join("two").join("b.txt"), "needle two\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();

        let result = workspace
            .search_text(SearchTextRequest {
                workspace_root: None,
                query: "needle".to_string(),
                path: ".".to_string(),
                paths: vec!["one".to_string(), "two".to_string()],
                regex: Some(false),
                case_sensitive: false,
                respect_gitignore: Some(false),
                max_matches: 10,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(result.matches.len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_splits_space_separated_path_when_all_parts_exist() {
        let root = temp_workspace("search_split_paths");
        fs::create_dir_all(root.join("one")).unwrap();
        fs::create_dir_all(root.join("two")).unwrap();
        fs::write(root.join("one").join("a.txt"), "needle one\n").unwrap();
        fs::write(root.join("two").join("b.txt"), "needle two\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();

        let result = workspace
            .search_text(SearchTextRequest {
                workspace_root: None,
                query: "needle".to_string(),
                path: "one two".to_string(),
                paths: Vec::new(),
                regex: Some(false),
                case_sensitive: false,
                respect_gitignore: Some(false),
                max_matches: 10,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(result.matches.len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_includes_ignored_files_when_asked() {
        let root = temp_workspace("search_gitignored_default");
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join(".ignore"), "logs/\n").unwrap();
        fs::write(root.join("logs").join("run.log"), "needle in ignored log\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();

        let result = workspace
            .search_text(SearchTextRequest {
                workspace_root: None,
                query: "needle".to_string(),
                path: ".".to_string(),
                paths: Vec::new(),
                regex: Some(false),
                case_sensitive: false,
                respect_gitignore: Some(false),
                max_matches: 10,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(result.matches.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_can_respect_gitignore_when_requested() {
        let root = temp_workspace("search_gitignored_requested");
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join(".ignore"), "logs/\n").unwrap();
        fs::write(root.join("logs").join("run.log"), "needle in ignored log\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();

        let result = workspace
            .search_text(SearchTextRequest {
                workspace_root: None,
                query: "needle".to_string(),
                path: ".".to_string(),
                paths: Vec::new(),
                regex: Some(false),
                case_sensitive: false,
                respect_gitignore: Some(true),
                max_matches: 10,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(result.matches.len(), 0);
        let _ = fs::remove_dir_all(root);
    }

    /// 2026-11-13: the first search of a turn walked every ignored folder of
    /// the project and took 39.8 s. Left to its default a search now skips
    /// what is ignored and says so when it finds nothing.
    #[test]
    fn search_skips_ignored_files_unless_asked_or_pointed_at_them() {
        let root = temp_workspace("search_ignored_default");
        fs::create_dir_all(root.join("logs").join("old")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join(".ignore"), "logs/\n*.tmp\n").unwrap();
        fs::write(root.join("logs").join("old").join("run.log"), "needle in an ignored log\n").unwrap();
        fs::write(root.join("src").join("scratch.tmp"), "needle in an ignored file\n").unwrap();
        fs::write(root.join("src").join("kept.txt"), "needle kept\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        use serde_json::json;
        let search = |args: serde_json::Value| {
            let found = workspace.search_text(serde_json::from_value(args).unwrap()).unwrap();
            let mut paths = found.matches.iter().map(|m| m.path.clone()).collect::<Vec<_>>();
            paths.sort();
            (paths, found.hints.iter().any(|hint| hint.contains("respect_gitignore:false")))
        };
        assert_eq!(search(json!({"query":"needle"})), (vec!["src/kept.txt".to_string()], false));
        assert_eq!(search(json!({"query":"needle","respect_gitignore":false})).0, ["logs/old/run.log", "src/kept.txt", "src/scratch.tmp"]);
        // Naming an ignored folder, or one inside it, searches it.
        assert_eq!(search(json!({"query":"needle","path":"logs"})).0, ["logs/old/run.log"]);
        assert_eq!(search(json!({"query":"needle","path":"logs/old"})).0, ["logs/old/run.log"]);
        // Nothing found while ignored files were left out: the result says so.
        assert_eq!(search(json!({"query":"ignored log"})), (Vec::<String>::new(), true));
        assert_eq!(search(json!({"query":"ignored log","respect_gitignore":false})).1, false);
        let _ = fs::remove_dir_all(root);
    }

    /// The 2026-11-06 desktop run: on Windows the text scan returned
    /// `src\a.rs` while list_dir and the symbol index give `src/a.rs`, so a
    /// program keying one by the other found nothing.
    #[test]
    fn search_paths_use_forward_slashes_like_list_dir() {
        let root = temp_workspace("search_slashes");
        fs::create_dir_all(root.join("nested").join("deeper")).unwrap();
        fs::write(root.join("nested").join("deeper").join("file.txt"), "needle\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();

        let result = workspace
            .search_text(SearchTextRequest {
                workspace_root: None,
                query: "needle".to_string(),
                path: ".".to_string(),
                paths: Vec::new(),
                regex: Some(false),
                case_sensitive: false,
                respect_gitignore: Some(false),
                max_matches: 10,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "nested/deeper/file.txt");
        let _ = fs::remove_dir_all(root);
    }

    /// 2026-11-13: `a|b|c` and `model_round\(` sent without regex:true found
    /// nothing, and the AI fell back to grep in a shell; `limit` was dropped
    /// silently. A query written as a regex is now matched as one, the AI
    /// can ask for files, counts, globs and context the way rg gives them.
    #[test]
    fn search_reads_like_grep() {
        let root = temp_workspace("search_grep");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.rs"), "fn alpha() {}\nmodel_round(1);\nlet beta = 2;\nlet x = a || b;\n").unwrap();
        fs::write(root.join("src").join("b.ts"), "alpha\nbeta\nbeta\n").unwrap();
        fs::write(root.join("notes.md"), "arr[0] alpha\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        let search = |args: serde_json::Value| {
            workspace.search_text(serde_json::from_value(args).unwrap()).unwrap()
        };

        let either = search(serde_json::json!({"query":"alpha|beta"}));
        assert_eq!((either.match_mode, either.matches.len()), ("regex", 6));
        assert!(!either.hints.is_empty(), "the AI is told it was read as a regex");
        let call = search(serde_json::json!({"query":"model_round\\("}));
        assert_eq!((call.match_mode, call.matches.len()), ("regex", 1));
        let forced = search(serde_json::json!({"query":"alpha|beta","regex":false}));
        assert_eq!((forced.match_mode, forced.matches.len()), ("literal", 0));
        // Code that only looks like a pattern stays literal.
        let or = search(serde_json::json!({"query":"a || b"}));
        assert_eq!((or.match_mode, or.matches.len()), ("literal", 1));
        let index = search(serde_json::json!({"query":"arr[0]"}));
        assert_eq!((index.match_mode, index.matches.len()), ("literal", 1));

        let limited = search(serde_json::json!({"query":"beta","limit":1}));
        assert_eq!((limited.matches.len(), limited.truncated), (1, true));

        let files = search(serde_json::json!({"query":"beta","output_mode":"files_with_matches"}));
        let mut listed = files.files.clone().unwrap();
        listed.sort();
        assert_eq!(listed, ["src/a.rs", "src/b.ts"]);
        assert!(files.matches.is_empty());
        let counted = search(serde_json::json!({"query":"beta","output_mode":"count","glob":"*.ts"}));
        let counts = counted.counts.as_ref().unwrap();
        assert_eq!((counts.len(), counts[0].path.as_str(), counts[0].count, counted.total), (1, "src/b.ts", 2, Some(2)));
        let rooted = search(serde_json::json!({"query":"alpha","glob":["src/**","!*.ts"]}));
        assert_eq!(rooted.matches.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(), ["src/a.rs"]);

        let around = search(serde_json::json!({"query":"model_round","context":1}));
        let hit = &around.matches[0];
        assert_eq!((hit.before.as_slice(), hit.after.as_slice()), (&["fn alpha() {}".to_owned()][..], &["let beta = 2;".to_owned()][..]));
        let value = serde_json::to_value(&search(serde_json::json!({"query":"alpha","path":"notes.md"}))).unwrap();
        assert!(value["matches"][0].get("before").is_none() && value.get("files").is_none(), "{value}");
        let _ = fs::remove_dir_all(root);
    }

    /// The same run: 60 matches in old JSONL logs came back as 400,000
    /// characters, because each match carried its whole line.
    #[test]
    fn search_returns_an_excerpt_of_a_long_line() {
        let root = temp_workspace("search_long_line");
        let long = format!("{}needle{}", "x".repeat(5_000), "y".repeat(5_000));
        fs::write(root.join("log.jsonl"), format!("{long}\nshort needle\n")).unwrap();
        let workspace = Workspace::new(&root).unwrap();

        let result = workspace
            .search_text(SearchTextRequest {
                workspace_root: None,
                query: "needle".to_string(),
                path: ".".to_string(),
                paths: Vec::new(),
                regex: Some(false),
                case_sensitive: false,
                respect_gitignore: Some(false),
                max_matches: 10,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(result.matches.len(), 2);
        let cut = &result.matches[0];
        assert_eq!((cut.line, cut.column, cut.line_chars), (1, 5_001, Some(10_006)));
        assert_eq!(cut.text.chars().count(), MATCH_TEXT_CHARS);
        assert!(cut.text.contains("needle"), "{}", cut.text);
        let short = &result.matches[1];
        assert_eq!((short.text.as_str(), short.line_chars), ("short needle", None));
        assert!(serde_json::to_value(short).unwrap().get("line_chars").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_long_line_excerpt_keeps_a_match_at_its_end() {
        let line = format!("{}needle", "中".repeat(1_000));
        let (text, total) = match_excerpt(&line, 1_000);
        assert_eq!(total, Some(1_006));
        assert_eq!(text.chars().count(), MATCH_TEXT_CHARS);
        assert!(text.ends_with("needle"), "{text}");
    }

    #[test]
    fn path_filters_match_exact_and_child_paths() {
        let filters = vec!["src/pages".to_string(), "README.md".to_string()];

        assert!(path_allowed_by_filters("src/pages/index.tsx", &filters));
        assert!(path_allowed_by_filters("src\\pages\\index.tsx", &filters));
        assert!(path_allowed_by_filters("README.md", &filters));
        assert!(!path_allowed_by_filters("src/redux/store.ts", &filters));
    }

    #[test]
    fn call_can_select_workspace_root() {
        let default_root = temp_workspace("default_root");
        let selected_root = temp_workspace("selected_root");
        fs::write(default_root.join("demo.txt"), "default\n").unwrap();
        fs::write(selected_root.join("demo.txt"), "selected\n").unwrap();
        let workspace = Workspace::new(&default_root).unwrap();

        let result = workspace
            .read_file(ReadFileRequest {
                workspace_root: Some(selected_root.display().to_string()),
                path: "demo.txt".to_string(),
                max_bytes: 1024,
            })
            .unwrap();

        assert_eq!(result.content, "selected\n");
        let _ = fs::remove_dir_all(default_root);
        let _ = fs::remove_dir_all(selected_root);
    }

    #[test]
    fn selected_workspace_root_must_be_absolute() {
        let root = temp_workspace("absolute_root");
        let workspace = Workspace::new(&root).unwrap();
        let result = workspace.read_file(ReadFileRequest {
            workspace_root: Some("relative-project".to_string()),
            path: "demo.txt".to_string(),
            max_bytes: 1024,
        });

        assert!(matches!(
            result,
            Err(ToolError::WorkspaceRootMustBeAbsolute(_))
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_info_requires_workspace_root() {
        let root = temp_workspace("workspace_info_required");
        let workspace = Workspace::new(&root).unwrap();
        let result = workspace.workspace_info(WorkspaceInfoRequest {
            workspace_root: None,
        });

        assert!(matches!(result, Err(ToolError::WorkspaceRootRequired)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_go_file_refreshes_existing_go_index() {
        let root = temp_workspace("go_reindex");
        fs::write(root.join("demo.go"), "package demo\n\nfunc OldName() {}\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        workspace
            .index_go_workspace(IndexGoWorkspaceRequest {
                workspace_root: root.display().to_string(),
            })
            .unwrap();

        let result = workspace
            .write_file(WriteFileRequest {
                workspace_root: Some(root.display().to_string()),
                path: "demo.go".to_string(),
                content: "package demo\n\nfunc NewName() {}\n".to_string(),
                create_parent_dirs: true,
                expected_code_hash: Some(crate::symbol_description::content_hash(
                    &fs::read(root.join("demo.go")).unwrap(),
                )),
            })
            .unwrap();

        assert!(result.go_reindexed);
        let search = workspace
            .search_go_symbols(SearchGoSymbolsRequest {
                options: Default::default(),
                workspace_root: root.display().to_string(),
                query: "NewName".to_string(),
                limit: 5,
            })
            .unwrap();
        assert_eq!(search.matches.len(), 1);
        assert_eq!(search.matches[0].name, "NewName");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_dir_supports_glob_kind_sort_mtime_and_limit() {
        let root = temp_workspace("list_dir_find_features");
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join("target/logs")).unwrap();
        fs::write(root.join("src/small.rs"), "a").unwrap();
        fs::write(root.join("src/nested/big.rs"), "1234567890").unwrap();
        fs::write(root.join("target/logs/fail.log"), "err").unwrap();
        let workspace = Workspace::new(&root).unwrap();

        let rust_files = workspace
            .list_dir(ListDirRequest {
                glob: vec!["**/*.rs".to_string()],
                kind: Some(EntryKind::File),
                sort_by: ListSort::SizeDesc,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rust_files.entries.len(), 2);
        assert_eq!(rust_files.entries[0].path, "src/nested/big.rs");
        assert_eq!(rust_files.entries[1].path, "src/small.rs");
        assert!(rust_files.entries[0].modified_ms.is_some());
        assert!(rust_files.entries[0].modified_at.as_deref().is_some_and(|s| s.ends_with('Z')));
        assert!(!rust_files.truncated);

        let limited = workspace
            .list_dir(ListDirRequest {
                glob: vec!["**/*.rs".to_string()],
                sort_by: ListSort::SizeDesc,
                max_entries: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(limited.entries.len(), 1);
        assert_eq!(limited.entries[0].path, "src/nested/big.rs");
        assert!(limited.truncated);

        let target_logs = workspace
            .list_dir(ListDirRequest {
                glob: vec!["target/**/*.log".to_string()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(target_logs.entries.len(), 1);
        assert_eq!(target_logs.entries[0].path, "target/logs/fail.log");

        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let none_recent = workspace
            .list_dir(ListDirRequest {
                recursive: true,
                modified_after: Some(future.clone()),
                ..Default::default()
            })
            .unwrap();
        assert!(none_recent.entries.is_empty());

        let before_future = workspace
            .list_dir(ListDirRequest {
                glob: vec!["**/*.rs".to_string()],
                modified_before: Some(future),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(before_future.entries.len(), 2);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_text_max_matches_per_file_keeps_other_files_visible() {
        let root = temp_workspace("search_per_file_cap");
        fs::write(root.join("a_noisy.log"), "hit 1\nhit 2\nhit 3\nhit 4\n").unwrap();
        fs::write(root.join("b_signal.log"), "hit B1\nhit B2\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();

        let response = workspace
            .search_text(SearchTextRequest {
                query: "hit".to_string(),
                max_matches: 10,
                max_matches_per_file: Some(2),
                ..Default::default()
            })
            .unwrap();

        assert!(response.truncated);
        assert_eq!(response.matches.len(), 4);
        assert_eq!(
            response.matches.iter().filter(|m| m.path == "a_noisy.log").count(),
            2
        );
        assert_eq!(
            response.matches.iter().filter(|m| m.path == "b_signal.log").count(),
            2
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn structured_git_tools_bound_status_diff_and_log() {
        let root = temp_workspace("git_tools");
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).current_dir(&root).output().unwrap();
            assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write(root.join("tracked.txt"), "one\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "initial"]);
        fs::write(root.join("tracked.txt"), "two\n").unwrap();
        fs::write(root.join("scratch.txt"), "scratch\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        let status = workspace.git_status(GitStatusRequest::default()).unwrap();
        assert!(!status.clean);
        assert!(status.modified.iter().any(|path| path == "tracked.txt"));
        assert!(status.untracked.iter().any(|path| path == "scratch.txt"));
        let diff = workspace.git_diff(GitDiffRequest { stat_only: true, ..Default::default() }).unwrap();
        assert!(diff.files.iter().any(|path| path == "tracked.txt"));
        let log = workspace.git_log(GitLogRequest::default()).unwrap();
        assert_eq!(log.commits.first().map(|commit| commit.subject.as_str()), Some("initial"));
        git(&["mv", "tracked.txt", "renamed.txt"]);
        let bounded = workspace.git_status(GitStatusRequest { max_entries: Some(1), ..Default::default() }).unwrap();
        assert_eq!(bounded.total_entries, 2, "rename's old path must not be counted as a second entry");
        assert!(bounded.truncated);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_dir_shows_noise_dirs_without_traversing_inside_by_default() {
        let root = temp_workspace("list_dir_visible_noise_dirs");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("target/sub/node_modules/nwsapi/dist")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/master\n").unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "module.exports = 1;\n").unwrap();
        fs::write(root.join("target/debug/app.log"), "ok\n").unwrap();
        fs::write(root.join("target/sub/node_modules/nwsapi/dist/lint.log"), "noise\n").unwrap();

        let workspace = Workspace::new(&root).unwrap();

        let top = workspace.list_dir(ListDirRequest::default()).unwrap();
        let top_paths = top.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>();
        assert!(top_paths.contains(&".git"));
        assert!(top_paths.contains(&"node_modules"));
        assert!(top_paths.contains(&"target"));
        assert!(top_paths.contains(&"src"));
        assert_eq!(top.unexpanded_dirs, vec![".git", "node_modules", "target"]);
        assert!(!top.hints.is_empty());

        let target_logs = workspace
            .list_dir(ListDirRequest {
                path: "target".to_string(),
                recursive: true,
                glob: vec!["*.log".to_string()],
                ..Default::default()
            })
            .unwrap();
        let log_paths = target_logs
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(log_paths, vec!["target/debug/app.log"]);
        assert_eq!(target_logs.unexpanded_dirs, vec!["target/sub/node_modules"]);
        assert!(!target_logs.hints.is_empty());

        let target_all = workspace
            .list_dir(ListDirRequest {
                path: "target".to_string(),
                recursive: true,
                ..Default::default()
            })
            .unwrap();
        let all_paths = target_all
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>();
        assert!(all_paths.contains(&"target/sub/node_modules"));
        assert!(!all_paths.contains(&"target/sub/node_modules/nwsapi/dist/lint.log"));
        assert_eq!(target_all.unexpanded_dirs, vec!["target/sub/node_modules"]);

        let clean_src = workspace
            .list_dir(ListDirRequest {
                path: "src".to_string(),
                recursive: true,
                ..Default::default()
            })
            .unwrap();
        assert!(clean_src.unexpanded_dirs.is_empty());
        assert!(clean_src.hints.is_empty());

        let explicit_dir = workspace
            .list_dir(ListDirRequest {
                path: "node_modules".to_string(),
                ..Default::default()
            })
            .unwrap();
        assert!(explicit_dir.entries.iter().any(|e| e.path == "node_modules/pkg"));

        let explicit_glob = workspace
            .list_dir(ListDirRequest {
                glob: vec!["node_modules/**".to_string()],
                ..Default::default()
            })
            .unwrap();
        assert!(explicit_glob.entries.iter().any(|e| e.path == "node_modules/pkg/index.js"));
        assert!(!explicit_glob.unexpanded_dirs.iter().any(|p| p == "node_modules"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_text_skips_noise_dirs_unless_targeted() {
        let root = temp_workspace("search_text_noise_dirs");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn clean() {}\n").unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "const x = 'ONLY_IN_NOISE_DEP';\n").unwrap();
        fs::write(root.join("target/debug/out.log"), "ONLY_IN_TARGET_LOG\n").unwrap();

        let workspace = Workspace::new(&root).unwrap();

        let broad = workspace
            .search_text(SearchTextRequest {
                query: "ONLY_IN_NOISE_DEP".to_string(),
                ..Default::default()
            })
            .unwrap();
        assert!(broad.matches.is_empty());
        assert!(broad.hints.iter().any(|h| h.contains("node_modules")));

        let explicit_path = workspace
            .search_text(SearchTextRequest {
                query: "ONLY_IN_NOISE_DEP".to_string(),
                path: "node_modules".to_string(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(explicit_path.matches.len(), 1);
        assert_eq!(explicit_path.matches[0].path, "node_modules/pkg/index.js");

        let explicit_glob = workspace
            .search_text(SearchTextRequest {
                query: "ONLY_IN_TARGET_LOG".to_string(),
                glob: vec!["target/**".to_string()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(explicit_glob.matches.len(), 1);
        assert_eq!(explicit_glob.matches[0].path, "target/debug/out.log");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_dir_supports_single_file_path_and_filters() {
        let root = temp_workspace("list_dir_single_file");
        fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/flow_libraries.rs");
        fs::write(&file, "pub fn example() {}\n").unwrap();

        let workspace = Workspace::new(&root).unwrap();

        let listed = workspace
            .list_dir(ListDirRequest {
                path: "src/flow_libraries.rs".to_string(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(listed.root, "src/flow_libraries.rs");
        assert!(!listed.truncated);
        assert!(listed.unexpanded_dirs.is_empty());
        assert_eq!(listed.entries.len(), 1);
        assert_eq!(listed.entries[0].path, "src/flow_libraries.rs");
        assert_eq!(listed.entries[0].kind, EntryKind::File);
        assert_eq!(listed.entries[0].size_bytes, Some(20));
        assert!(listed.entries[0].modified_at.is_some());

        let only_dirs = workspace
            .list_dir(ListDirRequest {
                path: "src/flow_libraries.rs".to_string(),
                kind: Some(EntryKind::Directory),
                ..Default::default()
            })
            .unwrap();
        assert!(only_dirs.entries.is_empty());

        let only_files = workspace
            .list_dir(ListDirRequest {
                path: "src/flow_libraries.rs".to_string(),
                kind: Some(EntryKind::File),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(only_files.entries.len(), 1);

        let glob_hit = workspace
            .list_dir(ListDirRequest {
                path: "src/flow_libraries.rs".to_string(),
                glob: vec!["*.rs".to_string()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(glob_hit.entries.len(), 1);

        let glob_miss = workspace
            .list_dir(ListDirRequest {
                path: "src/flow_libraries.rs".to_string(),
                glob: vec!["*.ts".to_string()],
                ..Default::default()
            })
            .unwrap();
        assert!(glob_miss.entries.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_text_caps_response_within_inline_byte_budget() {
        let root = temp_workspace("search_byte_budget");
        let long_zh = "中文上下文内容用于验证字节预算截断上限_".repeat(12);
        let mut big_content = String::new();
        for i in 0..240 {
            big_content.push_str(&format!("before_{i}: {long_zh}\n"));
            big_content.push_str(&format!("NEEDLE_HIT_{i}: {long_zh}\n"));
            big_content.push_str(&format!("after_{i}: {long_zh}\n"));
        }
        fs::write(root.join("dense.txt"), big_content).unwrap();

        let workspace = Workspace::new(&root).unwrap();
        let response = workspace
            .search_text(SearchTextRequest {
                query: "NEEDLE_HIT_".to_string(),
                max_matches: 200,
                context_lines: 1,
                ..Default::default()
            })
            .unwrap();

        assert!(response.truncated, "byte-capped search must mark truncated=true");
        assert!(!response.matches.is_empty(), "must retain matches up to budget");
        assert!(response.matches.len() < 200, "must stop before max_matches due to byte budget");
        assert!(
            response.hints.iter().any(|h| h.contains("48 KiB")),
            "expected 48 KiB budget hint: {:?}",
            response.hints
        );
        let total_json_bytes = serde_json::to_vec(&response).unwrap().len();
        assert!(
            total_json_bytes <= 52 * 1024,
            "response must stay well under 96 KiB soft limit, got {total_json_bytes} bytes"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_dir_and_search_text_support_pipe_globs_exclude_and_swarm_noise() {
        let root = temp_workspace("pipe_glob_exclude_swarm");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/kylin-arm64/customer")).unwrap();
        fs::create_dir_all(root.join("target/logs")).unwrap();
        fs::create_dir_all(root.join(".swarm/reports")).unwrap();

        fs::write(root.join("src/flow.rs"), "pub fn pipe_marker() {}\n").unwrap();
        fs::write(root.join("src/app.ts"), "export const pipe_marker = 1;\n").unwrap();
        fs::write(root.join("src/skip.tmp"), "pipe_marker\n").unwrap();
        fs::write(root.join("target/dev.log"), "pipe_marker dev\n").unwrap();
        fs::write(root.join("target/skip.tmp"), "pipe_marker tmp\n").unwrap();
        fs::write(root.join("target/logs/run.log"), "pipe_marker run\n").unwrap();
        fs::write(root.join("target/kylin-arm64/customer/a.log"), "pipe_marker kylin\n").unwrap();
        fs::write(root.join(".swarm/reports/worker.md"), "pipe_marker swarm\n").unwrap();

        let workspace = Workspace::new(&root).unwrap();

        // `.swarm` is in NOISE_DIRS: visible unexpanded in list_dir, skipped by default in search_text.
        let top = workspace.list_dir(ListDirRequest::default()).unwrap();
        assert!(top.unexpanded_dirs.contains(&".swarm".to_string()));
        let default_search = workspace
            .search_text(SearchTextRequest {
                query: "pipe_marker".to_string(),
                ..Default::default()
            })
            .unwrap();
        assert!(!default_search.matches.iter().any(|m| m.path.starts_with(".swarm/")));

        // Bare directory name in `exclude` excludes nested descendants even when `glob` matches `*.log`.
        let target_logs = workspace
            .list_dir(ListDirRequest {
                path: "target".to_string(),
                glob: vec!["*.log".to_string()],
                exclude: vec!["kylin-arm64".to_string()],
                ..Default::default()
            })
            .unwrap();
        let target_paths: Vec<&str> = target_logs.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(target_paths, vec!["target/dev.log", "target/logs/run.log"]);

        // Workspace-relative directory glob in `exclude` also works.
        let no_logs_dir = workspace
            .list_dir(ListDirRequest {
                path: "target".to_string(),
                glob: vec!["*.log".to_string()],
                exclude: vec!["kylin-arm64|target/logs/**".to_string()],
                ..Default::default()
            })
            .unwrap();
        let only_dev: Vec<&str> = no_logs_dir.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(only_dev, vec!["target/dev.log"]);

        // Pipe-separated `glob` and braced `{...}` globs both work.
        let piped = workspace
            .list_dir(ListDirRequest {
                path: "src".to_string(),
                glob: vec!["*.rs|*.ts".to_string()],
                ..Default::default()
            })
            .unwrap();
        let piped_paths: Vec<&str> = piped.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(piped_paths, vec!["src/app.ts", "src/flow.rs"]);

        let braced = workspace
            .list_dir(ListDirRequest {
                path: "src".to_string(),
                glob: vec!["{*.rs,*.ts}".to_string()],
                ..Default::default()
            })
            .unwrap();
        let braced_paths: Vec<&str> = braced.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(braced_paths, vec!["src/app.ts", "src/flow.rs"]);

        // `search_text` honors pipe-separated `glob` and `exclude`.
        let searched = workspace
            .search_text(SearchTextRequest {
                query: "pipe_marker".to_string(),
                path: "target".to_string(),
                glob: vec!["*.rs|*.log|*.tmp".to_string()],
                exclude: vec!["kylin-arm64|*.tmp".to_string()],
                max_matches: 100,
                ..Default::default()
            })
            .unwrap();
        let matched_files: Vec<&str> = searched.matches.iter().map(|m| m.path.as_str()).collect();
        assert!(!matched_files.iter().any(|p| p.contains("kylin-arm64")));
        assert!(!matched_files.iter().any(|p| p.ends_with(".tmp")));
        assert!(matched_files.contains(&"target/dev.log"));
        assert!(matched_files.contains(&"target/logs/run.log"));

        let _ = fs::remove_dir_all(root);
    }
}
