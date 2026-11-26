fn copy_regular_file(from: &Path, to: &Path, files: &mut usize, bytes: &mut u64) -> Result<()> {
    if *files >= 5000 { return Err(ToolError::InvalidParameter("recursive copy exceeds 5000 files".into())); }
    let size = fs::symlink_metadata(from)?.len();
    if size > 2 * 1024 * 1024 { return Err(ToolError::InvalidParameter("a copied file exceeds the 2 MiB per-file undo snapshot limit".into())); }
    if bytes.saturating_add(size) > 128 * 1024 * 1024 { return Err(ToolError::InvalidParameter("copy exceeds the 128 MiB undo snapshot limit".into())); }
    crate::context::database::check_source_access(&from.canonicalize()?)?;
    fs::copy(from, to)?;
    *files += 1;
    *bytes += size;
    Ok(())
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
            workspace_root: crate::tools::file_edit::display_path(&workspace.root),
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
        let (check_ok, check_issues) = if request.check {
            let mut check_args = vec!["diff", "--check"];
            if request.staged { check_args.push("--cached"); }
            if let Some(range) = request.rev_range.as_deref() { check_args.push(range); }
            if !paths.is_empty() { check_args.push("--"); check_args.extend(paths.iter().map(String::as_str)); }
            let mut check_cmd = Command::new("git");
            check_cmd.args(["-c", "core.fsmonitor=false", "-c", "color.ui=never"]).args(&check_args)
                .current_dir(&workspace.root).env("GIT_OPTIONAL_LOCKS", "0").env("GIT_TERMINAL_PROMPT", "0");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                check_cmd.creation_flags(0x08000000);
            }
            let output = check_cmd.output().map_err(ToolError::from)?;
            let diagnostics = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            let issues = diagnostics.lines().filter_map(|line| {
                let (location, message) = line.split_once(": ")?;
                let (path, line_number) = location.rsplit_once(':').map_or((location, None), |(path, n)| (path, n.parse().ok()));
                Some(GitDiffIssue { path: Some(path.to_owned()), line: line_number, message: message.to_owned() })
            }).collect::<Vec<_>>();
            (Some(output.status.success() && issues.is_empty()), issues)
        } else { (None, Vec::new()) };
        Ok(GitDiffResponse { diff, files, truncated, stat_only: request.stat_only, check_ok, check_issues })
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

        let globs = search_globs(&workspace.root, &request.glob, &request.exclude, &[])?;
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
        let limit = request.max_bytes.min(crate::index::source_read::FILE_LIMIT as u64);
        if metadata.len() > limit {
            return Err(ToolError::FileTooLarge {
                actual: metadata.len(),
                limit,
            });
        }

        let snapshot = crate::index::source_read::read(&path, limit as usize)?;
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
        if metadata.len() <= crate::index::source_read::FILE_LIMIT as u64 {
            let snapshot = crate::index::source_read::read(&path, crate::index::source_read::FILE_LIMIT)?;
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
        let compile = |pat: &str| {
            regex::RegexBuilder::new(pat)
                .case_insensitive(!request.case_sensitive)
                .size_limit(2 * 1024 * 1024)
                .build()
        };
        let mut auto_repaired_regex = false;
        let (regex, matcher) = match explicit {
            Some(true) => match compile(&request.query) {
                Ok(re) => (true, re),
                Err(first_err) => {
                    if let Some(repaired) = escape_unbalanced_delimiters(&request.query)
                        && let Ok(re) = compile(&repaired)
                    {
                        auto_repaired_regex = true;
                        (true, re)
                    } else {
                        return Err(ToolError::InvalidSearchPattern(format!(
                            "{first_err} (tip: pass a JS RegExp literal like /foo\\(/ or escape parens as \\\\( in JS strings)"
                        )));
                    }
                }
            },
            Some(false) => (
                false,
                compile(&regex::escape(&request.query))
                    .map_err(|error| ToolError::InvalidSearchPattern(error.to_string()))?,
            ),
            None => {
                if written_as_regex(&request.query) {
                    (
                        true,
                        compile(&request.query)
                            .map_err(|error| ToolError::InvalidSearchPattern(error.to_string()))?,
                    )
                } else if has_regex_markers(&request.query)
                    && let Some(repaired) = escape_unbalanced_delimiters(&request.query)
                    && let Ok(re) = compile(&repaired)
                    && !re.is_match("")
                {
                    auto_repaired_regex = true;
                    (true, re)
                } else {
                    (
                        false,
                        compile(&regex::escape(&request.query))
                            .map_err(|error| ToolError::InvalidSearchPattern(error.to_string()))?,
                    )
                }
            }
        };
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let (roots, path_globs, missing_paths) = workspace.resolve_search_roots(&request)?;
        let root_filters = workspace.search_root_filters(&roots)?;
        let globs = search_globs(&workspace.root, &request.glob, &request.exclude, &path_globs)?;
        let mut glob_for_noise = request.glob.clone();
        glob_for_noise.extend(path_globs.iter().cloned());
        let allowed_noise = allowed_noise_dirs(&glob_for_noise);
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
            && let Ok(conn) = crate::context::database::init_db(&workspace.root)
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
                if crate::context::database::get_index_generated_at(&conn, &root_str, lang).is_some() {
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
        if !missing_paths.is_empty() {
            hints.push("Some requested paths did not exist and were skipped (see missing_paths); search ran on the remaining existing paths.");
        }
        if truncated_by_bytes {
            hints.push("Search output capped at ~48 KiB inline budget; narrow path/query/exclude/context_lines or read specific line ranges.");
        }
        if auto_repaired_regex {
            hints.push("Auto-escaped unbalanced regex delimiter(s) such as '('; pass a JS RegExp literal like /foo\\(/ or double-escape '\\\\(' in JS strings for exact regex control.");
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
            missing_paths,
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

    fn resolve_search_entry(&self, raw: &str) -> Result<(PathBuf, Option<String>)> {
        let trimmed = raw.trim();
        if let Ok(existing) = self.resolve_existing(trimmed) {
            return Ok((existing, None));
        }
        let has_glob = trimmed.chars().any(|c| matches!(c, '*' | '?' | '[' | '{'));
        if !has_glob {
            return Ok((self.resolve_existing(trimmed)?, None));
        }
        let stripped = trimmed
            .strip_prefix(r"\\?\")
            .or_else(|| trimmed.strip_prefix("//?/"))
            .unwrap_or(trimmed);
        let normalized = stripped.replace('\\', "/");
        if normalized.split('/').any(|seg| seg == "..") {
            return Err(ToolError::ParentTraversal(trimmed.to_owned()));
        }
        let mut prefix_segments = Vec::new();
        let mut tail_segments = Vec::new();
        let mut seen_glob = false;
        let is_unix_abs = normalized.starts_with('/');
        let win_drive_prefix = {
            let b = normalized.as_bytes();
            b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
        };
        let is_abs = is_unix_abs || win_drive_prefix;
        for seg in normalized.split('/') {
            if seg.is_empty() || (!is_abs && seg == "." && prefix_segments.is_empty() && !seen_glob) {
                continue;
            }
            if !seen_glob && !seg.chars().any(|c| matches!(c, '*' | '?' | '[' | '{')) {
                prefix_segments.push(seg);
            } else {
                seen_glob = true;
                tail_segments.push(seg);
            }
        }
        let base_raw = if prefix_segments.is_empty() {
            if is_unix_abs { "/".to_owned() } else { ".".to_owned() }
        } else if is_unix_abs {
            format!("/{}", prefix_segments.join("/"))
        } else if win_drive_prefix && prefix_segments.len() == 1 {
            format!("{}/", prefix_segments[0])
        } else {
            prefix_segments.join("/")
        };
        let base_dir = self.resolve_existing(&base_raw)?;
        let rel_base = normalize_rel_path(&self.relative_display(&base_dir)?);
        let tail = tail_segments.join("/");
        let pattern = if rel_base.is_empty() || rel_base == "." {
            tail
        } else if tail.is_empty() {
            format!("{rel_base}/**")
        } else {
            format!("{rel_base}/{tail}")
        };
        Ok((base_dir, Some(pattern)))
    }

    fn resolve_search_roots(&self, request: &SearchTextRequest) -> Result<(Vec<PathBuf>, Vec<String>, Vec<String>)> {
        let mut requested_paths = if request.paths.is_empty() {
            vec![request.path.clone()]
        } else {
            request.paths.clone()
        };

        if requested_paths.len() == 1 {
            let single = requested_paths[0].trim();
            if self.resolve_search_entry(single).is_err() {
                let split_paths: Vec<String> = single
                    .split_whitespace()
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
                if split_paths.len() > 1
                    && split_paths
                        .iter()
                        .all(|path| self.resolve_search_entry(path).is_ok())
                {
                    requested_paths = split_paths;
                }
            }
        }

        let multi = requested_paths.len() > 1;
        let mut resolved = Vec::with_capacity(requested_paths.len());
        let mut missing_paths = Vec::new();
        let mut first_missing_err: Option<ToolError> = None;
        let mut any_glob = false;
        for raw in &requested_paths {
            match self.resolve_search_entry(raw) {
                Ok((root, glob)) => {
                    if glob.is_some() {
                        any_glob = true;
                    }
                    resolved.push((root, glob));
                }
                Err(err @ ToolError::NotFound(_)) if multi => {
                    missing_paths.push(raw.trim().to_owned());
                    if first_missing_err.is_none() {
                        first_missing_err = Some(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }
        if resolved.is_empty() {
            return Err(first_missing_err
                .unwrap_or_else(|| ToolError::NotFound(requested_paths.join(", "))));
        }

        let mut roots = Vec::new();
        let mut path_globs = Vec::new();
        for (root, glob) in resolved {
            if any_glob {
                if let Some(pat) = glob {
                    path_globs.push(pat);
                } else {
                    let rel = normalize_rel_path(&self.relative_display(&root)?);
                    if rel.is_empty() || rel == "." {
                        path_globs.push("**".to_owned());
                    } else if root.is_dir() {
                        path_globs.push(format!("{rel}/**"));
                    } else {
                        path_globs.push(rel);
                    }
                }
            }
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        Ok((roots, path_globs, missing_paths))
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

    pub fn stat_paths(&self, request: StatRequest) -> Result<StatResponse> {
        let mut paths = request.paths;
        if let Some(path) = request.path { paths.insert(0, path); }
        if paths.len() > 200 { return Err(ToolError::InvalidParameter("io.stat accepts at most 200 paths".into())); }
        let mut entries = Vec::with_capacity(paths.len());
        for raw in paths {
            let input = Path::new(&raw);
            if !input.is_absolute() && input.components().any(|part| matches!(part, Component::ParentDir | Component::Prefix(_) | Component::RootDir)) {
                return Err(ToolError::ParentTraversal(raw));
            }
            let candidate = if input.is_absolute() { input.to_path_buf() } else { self.root.join(input) };
            if !input.is_absolute() {
                let mut ancestor=candidate.as_path();
                while !ancestor.exists() && fs::symlink_metadata(ancestor).is_err() {
                    ancestor=ancestor.parent().ok_or_else(||ToolError::OutsideWorkspace(raw.clone()))?;
                }
                if !ancestor.canonicalize()?.starts_with(&self.root) { return Err(ToolError::OutsideWorkspace(raw)); }
            }
            let metadata = match fs::symlink_metadata(&candidate) {
                Ok(meta) => Some(meta),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            if let Some(meta) = &metadata {
                if meta.file_type().is_symlink() { return Err(ToolError::InvalidParameter(format!("io.stat does not follow symbolic links: {raw}"))); }
                crate::context::database::check_source_access(&candidate.canonicalize()?)?;
            } else if let Some(parent) = candidate.parent().and_then(|p| p.canonicalize().ok()) {
                crate::context::database::check_source_access(&parent)?;
            }
            let (kind,size,modified,modified_ms,readonly,executable) = if let Some(meta) = metadata {
                let kind = if meta.file_type().is_symlink() { EntryKind::Symlink } else if meta.is_file() { EntryKind::File } else if meta.is_dir() { EntryKind::Directory } else { EntryKind::Other };
                let modified = meta.modified().ok();
                let modified_ms = modified.and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|duration| duration.as_millis().min(i64::MAX as u128) as i64);
                let modified_at = modified.map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339());
                #[cfg(unix)]
                use std::os::unix::fs::PermissionsExt;
                #[cfg(unix)]
                let executable = meta.is_file().then(|| meta.permissions().mode() & 0o111 != 0);
                #[cfg(not(unix))]
                let executable = None;
                (Some(kind),Some(meta.len()),modified_at,modified_ms,Some(meta.permissions().readonly()),executable)
            } else { (None,None,None,None,None,None) };
            let display = crate::tools::file_edit::display_relative_or_abs(&self.root, &candidate);
            entries.push(StatEntry {path:display,exists:kind.is_some(),kind,size_bytes:size,modified_at:modified,modified_ms,readonly,executable});
        }
        Ok(StatResponse { entries })
    }

    pub fn copy_path(&self, request: CopyPathRequest) -> Result<CopyPathResponse> {
        let from_input = Path::new(&request.from);
        let source_candidate = if from_input.is_absolute() { from_input.to_path_buf() } else { self.root.join(from_input) };
        let mut component_path = PathBuf::new();
        for component in source_candidate.components() {
            component_path.push(component.as_os_str());
            if fs::symlink_metadata(&component_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
                return Err(ToolError::InvalidParameter("copy does not follow symbolic links".into()));
            }
        }
        let from = if from_input.is_absolute() && !from_input.starts_with(&self.root) {
            let source = from_input.canonicalize()?;
            crate::context::database::check_source_access(&source)?;
            source
        } else {
            crate::tools::file_edit::workspace_path(&self.root, &request.from)?
        };
        let to = crate::tools::file_edit::workspace_path(&self.root, &request.to)?;
        if from == self.root || to == self.root || in_repository_metadata(&self.root, &from) || in_repository_metadata(&self.root, &to)
            || from.components().any(|part| part.as_os_str() == ".git")
        {
            return Err(ToolError::InvalidParameter("refusing to copy the workspace root or repository metadata".into()));
        }
        if from == to { return Err(ToolError::InvalidParameter("source and destination must differ".into())); }
        let source = fs::symlink_metadata(&from)?;
        if source.file_type().is_symlink() { return Err(ToolError::InvalidParameter("copy does not follow symbolic links".into())); }
        if source.is_dir() && !request.recursive { return Err(ToolError::InvalidParameter("directory copy requires recursive: true".into())); }
        if source.is_dir() && to.starts_with(&from) { return Err(ToolError::InvalidParameter("cannot copy a directory into itself".into())); }
        let lock_a = crate::tools::file_edit::file_lock(&from);
        let lock_b = crate::tools::file_edit::file_lock(&to);
        let (_guard_a, _guard_b) = if from < to {
            (lock_a.lock().unwrap_or_else(|e|e.into_inner()), lock_b.lock().unwrap_or_else(|e|e.into_inner()))
        } else {
            (lock_b.lock().unwrap_or_else(|e|e.into_inner()), lock_a.lock().unwrap_or_else(|e|e.into_inner()))
        };
        if (!from_input.is_absolute() && from != crate::tools::file_edit::workspace_path(&self.root, &request.from)?)
            || to != crate::tools::file_edit::workspace_path(&self.root, &request.to)?
        { return Err(ToolError::WriteConflict("copy path changed while waiting".into())); }
        let existing = fs::symlink_metadata(&to).ok();
        if existing.is_some() && !request.overwrite {
            return Err(ToolError::WritePreconditionRequired(format!("destination exists: pass overwrite: true to replace {}",request.to)));
        }
        if existing.as_ref().is_some_and(|meta| meta.is_dir() != source.is_dir()) {
            return Err(ToolError::InvalidParameter("source and destination kinds must match when overwriting".into()));
        }
        if existing.is_some() && source.is_dir() {
            let mut dirs=vec![to.clone()];
            while let Some(dir)=dirs.pop() {
                for item in fs::read_dir(&dir)? {
                    let entry=item?;
                    let path=entry.path();
                    if entry.file_name()==".git" { return Err(ToolError::InvalidParameter("refusing to replace a directory containing repository metadata".into())); }
                    let metadata=fs::symlink_metadata(&path)?;
                    if metadata.is_dir() && !metadata.file_type().is_symlink() { dirs.push(path); }
                }
            }
        }
        let parent = to.parent().ok_or_else(||ToolError::OutsideWorkspace(request.to.clone()))?;
        if request.create_parent_dirs { fs::create_dir_all(parent)?; }
        else if !parent.is_dir() { return Err(ToolError::NotFound(parent.display().to_string())); }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let file_name = to.file_name().ok_or_else(||ToolError::InvalidParameter("destination must have a file name".into()))?;
        let stage = parent.join(format!(".{}.copy-stage-{}-{stamp}",file_name.to_string_lossy(),std::process::id()));
        let backup = parent.join(format!(".{}.copy-backup-{}-{stamp}",file_name.to_string_lossy(),std::process::id()));
        let mut count=0usize; let mut bytes=0u64;
        let staged = (|| -> Result<()> {
            if source.is_file() { copy_regular_file(&from,&stage,&mut count,&mut bytes)?; }
            else {
                fs::create_dir(&stage)?;
                let mut stack=vec![(from.clone(),stage.clone())];
                let mut visited=0usize;
                while let Some((src,dst))=stack.pop() {
                    for item in fs::read_dir(&src)? {
                        visited+=1;
                        if visited>10_000 { return Err(ToolError::InvalidParameter("recursive copy exceeds 10000 directory entries".into())); }
                        let entry=item?; let source_path=entry.path();
                        if entry.file_name()==".git" { continue; }
                        let target_path=dst.join(entry.file_name()); let metadata=fs::symlink_metadata(&source_path)?;
                        if metadata.file_type().is_symlink() { continue; }
                        if metadata.is_dir() { fs::create_dir(&target_path)?; stack.push((source_path,target_path)); }
                        else if metadata.is_file() { copy_regular_file(&source_path,&target_path,&mut count,&mut bytes)?; }
                    }
                }
            }
            Ok(())
        })();
        if let Err(error)=staged { let _=if stage.is_dir(){fs::remove_dir_all(&stage)}else{fs::remove_file(&stage)}; return Err(error); }
        if existing.is_some() { fs::rename(&to,&backup)?; }
        if let Err(error)=fs::rename(&stage,&to) {
            if existing.is_some() { let _=fs::rename(&backup,&to); }
            let _=if stage.is_dir(){fs::remove_dir_all(&stage)}else{fs::remove_file(&stage)};
            return Err(error.into());
        }
        if existing.is_some() { let _=if backup.is_dir(){fs::remove_dir_all(&backup)}else{fs::remove_file(&backup)}; }
        let kind=if source.is_dir(){"directory"}else{"file"};
        Ok(CopyPathResponse{from:self.relative_display(&from)?,to:self.relative_display(&to)?,kind:kind.into(),files_copied:count,bytes_copied:bytes,overwritten:existing.is_some()})
    }


    pub fn write_file(&self, request: WriteFileRequest) -> Result<WriteFileResponse> {
        check_write_size(request.content.len())?;
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = crate::tools::file_edit::workspace_path(&workspace.root, &request.path)?;
        let lock = crate::tools::file_edit::file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        if crate::tools::file_edit::workspace_path(&workspace.root, &request.path)? != path {
            return Err(ToolError::WriteConflict("file location changed while waiting".into()));
        }
        let original = crate::tools::file_edit::snapshot(&path)?;
        if original.is_some() && request.expected_code_hash.is_none() {
            return Err(ToolError::WritePreconditionRequired("overwriting an existing file requires expected_code_hash from a source read; use edit_file for focused edits".into()));
        }
        crate::tools::file_edit::check_hash(original.as_deref(), request.expected_code_hash.as_deref())?;
        if let Some(parent) = path.parent() {
            if request.create_parent_dirs {
                fs::create_dir_all(parent)?;
            } else if !parent.exists() {
                return Err(ToolError::NotFound(parent.display().to_string()));
            }
        }
        let changed =
            crate::tools::file_edit::commit(&path, original.as_deref(), request.content.as_bytes())?;
        drop(_guard);
        let index_refresh = refresh_written_index(&workspace.root, &path, changed);
        Ok(WriteFileResponse {
            path: workspace.relative_display(&path)?,
            bytes_written: if changed { request.content.len() } else { 0 },
            go_reindexed: index_refresh["status"] == "updated" && index_refresh["language"] == "go",
            created: original.is_none(),
            changed,
            code_hash: crate::index::symbol_description::content_hash(request.content.as_bytes()),
            index_refresh,
            preview: None,
        })
    }

    /// Remove one file, or a whole directory tree when `recursive` is set.
    ///
    /// Refuses the workspace root and the repository metadata directory: a
    /// delete that lands there cannot be recovered by re-reading source.
    pub fn delete_path(&self, request: DeletePathRequest) -> Result<DeletePathResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = crate::tools::file_edit::workspace_path(&workspace.root, &request.path)?;
        if path == workspace.root {
            return Err(ToolError::InvalidParameter(
                "refusing to delete the workspace root".into(),
            ));
        }
        if in_repository_metadata(&workspace.root, &path) {
            return Err(ToolError::InvalidParameter(
                "refusing to delete inside .git".into(),
            ));
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ToolError::NotFound(request.path));
            }
            Err(error) => return Err(error.into()),
        };
        let lock = crate::tools::file_edit::file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let kind = if metadata.is_dir() {
            if !request.recursive {
                return Err(ToolError::InvalidParameter(format!(
                    "path is a directory: pass recursive: true to remove it: {}",
                    request.path
                )));
            }
            fs::remove_dir_all(&path)?;
            "directory"
        } else {
            fs::remove_file(&path)?;
            "file"
        };
        Ok(DeletePathResponse {
            path: workspace.relative_display(&path)?,
            deleted: true,
            kind: kind.to_string(),
        })
    }

    /// Move or rename one path inside the workspace; the destination may only
    /// replace a file when `overwrite` says so.
    pub fn rename_path(&self, request: RenamePathRequest) -> Result<RenamePathResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let from = crate::tools::file_edit::workspace_path(&workspace.root, &request.path)?;
        let to = crate::tools::file_edit::workspace_path(&workspace.root, &request.to)?;
        if from == workspace.root || to == workspace.root {
            return Err(ToolError::InvalidParameter(
                "refusing to move the workspace root".into(),
            ));
        }
        if in_repository_metadata(&workspace.root, &from) || in_repository_metadata(&workspace.root, &to) {
            return Err(ToolError::InvalidParameter(
                "refusing to move paths inside .git".into(),
            ));
        }
        if fs::symlink_metadata(&from).is_err() {
            return Err(ToolError::NotFound(request.path));
        }
        let to_meta = fs::symlink_metadata(&to).ok();
        if to_meta.as_ref().is_some_and(|m| m.is_dir()) {
            return Err(ToolError::InvalidParameter(format!(
                "destination is a directory: {}",
                request.to
            )));
        }
        if to_meta.is_some() && !request.overwrite {
            return Err(ToolError::WritePreconditionRequired(format!(
                "destination exists: pass overwrite: true to replace {}",
                request.to
            )));
        }
        if request.create_parent_dirs
            && let Some(parent) = to.parent()
        {
            fs::create_dir_all(parent)?;
        }
        let (first_lock, second_lock) = if from == to {
            (crate::tools::file_edit::file_lock(&from), None)
        } else if from < to {
            (
                crate::tools::file_edit::file_lock(&from),
                Some(crate::tools::file_edit::file_lock(&to)),
            )
        } else {
            (
                crate::tools::file_edit::file_lock(&to),
                Some(crate::tools::file_edit::file_lock(&from)),
            )
        };
        let _guard1 = first_lock.lock().unwrap_or_else(|error| error.into_inner());
        let _guard2 = second_lock
            .as_ref()
            .map(|lock| lock.lock().unwrap_or_else(|error| error.into_inner()));
        let overwritten = fs::symlink_metadata(&to).is_ok();
        fs::rename(&from, &to)?;
        Ok(RenamePathResponse {
            path: workspace.relative_display(&from)?,
            to: workspace.relative_display(&to)?,
            overwritten,
            index_refresh: refresh_written_index(&workspace.root, &to, true),
        })
    }

    /// Create one directory, or the whole chain when `recursive` is set.
    pub fn create_dir(&self, request: CreateDirRequest) -> Result<CreateDirResponse> {
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = crate::tools::file_edit::workspace_path(&workspace.root, &request.path)?;
        if in_repository_metadata(&workspace.root, &path) {
            return Err(ToolError::InvalidParameter(
                "refusing to create directory inside .git".into(),
            ));
        }
        if path.is_dir() {
            return Ok(CreateDirResponse {
                path: workspace.relative_display(&path)?,
                created: false,
            });
        }
        if path.exists() {
            return Err(ToolError::NotDirectory(request.path));
        }
        if request.recursive {
            fs::create_dir_all(&path)?;
        } else {
            fs::create_dir(&path)?;
        }
        Ok(CreateDirResponse {
            path: workspace.relative_display(&path)?,
            created: true,
        })
    }
    pub fn replace_range(&self, request: ReplaceRangeRequest) -> Result<ReplaceRangeResponse> {
        validate_replace_range(request.start_line, request.end_line)?;
        check_write_size(request.replacement.len())?;
        if request.expected_old_text.is_none() && request.expected_code_hash.is_none() {
            return Err(ToolError::WritePreconditionRequired("replace_range requires expected_old_text or expected_code_hash from current source".into()));
        }
        let workspace = self.with_root(request.workspace_root.as_deref())?;
        let path = crate::tools::file_edit::workspace_path(&workspace.root, &request.path)?;
        let lock = crate::tools::file_edit::file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        if crate::tools::file_edit::workspace_path(&workspace.root, &request.path)? != path {
            return Err(ToolError::WriteConflict("file location changed while waiting".into()));
        }
        let original = fs::read(&path)?;
        crate::tools::file_edit::check_hash(Some(&original), request.expected_code_hash.as_deref())?;
        let content = String::from_utf8(original.clone())?;
        let updated = crate::tools::file_edit::line_replacement(
            &content,
            request.start_line,
            request.end_line,
            &request.replacement,
            request.expected_old_text.as_deref(),
        )?;
        check_write_size(updated.len())?;
        let changed = crate::tools::file_edit::commit(&path, Some(&original), updated.as_bytes())?;
        drop(_guard);
        let index_refresh = refresh_written_index(&workspace.root, &path, changed);
        Ok(ReplaceRangeResponse {
            path: workspace.relative_display(&path)?,
            start_line: request.start_line,
            end_line: request.end_line,
            bytes_written: if changed { updated.len() } else { 0 },
            changed,
            code_hash: crate::index::symbol_description::content_hash(updated.as_bytes()),
            go_reindexed: index_refresh["status"] == "updated" && index_refresh["language"] == "go",
            index_refresh,
            preview: changed.then(|| crate::tools::file_edit::preview_for_line_ranges(
                &updated,
                &[(
                    request.start_line,
                    request.start_line + request.replacement.lines().count().max(1) - 1,
                )],
            )),
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
        let path = crate::tools::file_edit::workspace_path(&workspace.root, &request.path)?;
        let lock = crate::tools::file_edit::file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        if crate::tools::file_edit::workspace_path(&workspace.root, &request.path)? != path {
            return Err(ToolError::WriteConflict("file location changed while waiting".into()));
        }
        let original = fs::read(&path)?;
        crate::tools::file_edit::check_hash(Some(&original), request.expected_code_hash.as_deref())?;
        let content = String::from_utf8(original.clone())?;
        let (updated, changed_ranges) = crate::tools::file_edit::text_edits_with_ranges_replace_all(
            &content,
            &request.edits,
            request.replace_all,
        )?;
        check_write_size(updated.len())?;
        let changed = crate::tools::file_edit::commit(&path, Some(&original), updated.as_bytes())?;
        drop(_guard);
        let index_refresh = refresh_written_index(&workspace.root, &path, changed);
        Ok(WriteFileResponse {
            path: workspace.relative_display(&path)?,
            bytes_written: if changed { updated.len() } else { 0 },
            created: false,
            changed,
            code_hash: crate::index::symbol_description::content_hash(updated.as_bytes()),
            go_reindexed: index_refresh["status"] == "updated" && index_refresh["language"] == "go",
            index_refresh,
            preview: changed.then(|| crate::tools::file_edit::preview_for_byte_ranges(&updated, &changed_ranges)),
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
        let display_path = |path: &Path| {
            path.strip_prefix(&self.root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| path.display().to_string())
        };

        // A missing dependency path is usually an alias this tool cannot see:
        // `@/components/Foo`, `~/lib/x` or `host:io` resolves in Vite or tsc,
        // never on disk. Say so once instead of leaving the caller to guess.
        if raw.replace('\\', "/").contains("node_modules/")
            || candidate.to_string_lossy().replace('\\', "/").contains("node_modules/")
        {
            return Some(
                "Hint: if this is a Vite/TypeScript workspace alias, check vite.config.ts or tsconfig.json"
                    .to_string(),
            );
        }

        // Check the reverse direction too: `foo/bar/mod.rs` -> `foo/bar.rs`,
        // `foo/bar/index.ts` -> `foo/bar.ts`, and `foo/bar/__init__.py` -> `foo/bar.py`.
        if let (Some(parent), Some(file_name), Some(ext)) = (
            candidate.parent(),
            candidate.file_name().and_then(|name| name.to_str()),
            candidate.extension().and_then(|ext| ext.to_str()),
        ) {
            let reverse_module = match (file_name, ext) {
                ("mod.rs", "rs") => Some(parent.with_extension("rs")),
                ("index.ts", "ts") | ("index.tsx", "tsx") | ("index.js", "js") | ("index.jsx", "jsx") => {
                    Some(parent.with_extension(ext))
                }
                ("__init__.py", "py") => Some(parent.with_extension("py")),
                _ => None,
            };
            if let Some(module) = reverse_module.filter(|path| path.is_file()) {
                return Some(format!("found sibling module file: {}", display_path(&module)));
            }
        }

        // Check directory-module equivalents: `foo/bar.rs` -> `foo/bar/mod.rs`,
        // `foo/bar.ts` -> `foo/bar/index.ts`, etc.
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
                        return Some(format!("found directory module: {}", display_path(&alt)));
                    }
                }
            }
        }

        // Check close sibling names inside the existing parent directory (or nearest
        // existing ancestor when intermediate directories do not exist).
        let Some(parent) = candidate.parent() else {
            return None;
        };
        let target_name = candidate
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(raw);
        let wanted_stem = base_stem(target_name);
        if wanted_stem.is_empty() {
            return None;
        }
        if !parent.is_dir() {
            // Walk up to the nearest existing ancestor inside the workspace so
            // missing intermediate folders (e.g. `frontend/src/store/index.ts`)
            // still get concrete directory guidance.
            let mut missing_seg = parent.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let mut cur = parent.parent();
            while let Some(dir) = cur {
                if !dir.starts_with(&self.root) {
                    break;
                }
                if dir.is_dir() {
                    let similar = ranked_similar_entries(dir, missing_seg, 5);
                    if !similar.is_empty() {
                        return Some(format!(
                            "missing dir `{missing_seg}` under {}/; similar: {}",
                            display_path(dir),
                            similar.join(", ")
                        ));
                    }
                    let entries = list_dir_preview(dir, 8);
                    if !entries.is_empty() {
                        return Some(format!(
                            "missing dir `{missing_seg}` under {}/; entries: {}",
                            display_path(dir),
                            entries.join(", ")
                        ));
                    }
                    break;
                }
                missing_seg = dir.file_name().and_then(|s| s.to_str()).unwrap_or(missing_seg);
                cur = dir.parent();
            }
            return None;
        }

        let mut smart_hints = Vec::new();
        // When a stylesheet (`.css`/`.scss`/`.less`) is missing, peek at any
        // same-stem component sibling (e.g. `SettingsModal.tsx`) for actual CSS imports.
        if is_style_filename(target_name) {
            if let Some((sibling_name, imports)) = sibling_style_imports(parent, &wanted_stem) {
                smart_hints.push(format!("{sibling_name} imports: {}", imports.join(", ")));
            }
        }
        let similar = ranked_similar_entries(parent, target_name, 5);
        if !similar.is_empty() {
            smart_hints.push(format!("similar in parent dir: {}", similar.join(", ")));
        }
        if !smart_hints.is_empty() {
            return Some(smart_hints.join("; "));
        }

        // If no fuzzy name matched, show the sibling module and a small,
        // deterministic inventory of real parent-directory entries.
        let mut hints = Vec::new();
        if let Some(ext) = candidate.extension().and_then(|ext| ext.to_str()) {
            let sibling = parent.with_extension(ext);
            if sibling.is_file() {
                hints.push(format!("found sibling module file: {}", display_path(&sibling)));
            }
        }
        let entries = list_dir_preview(parent, 8);
        if !entries.is_empty() {
            hints.push(format!("entries in {}/: {}", display_path(parent), entries.join(", ")));
        }
        (!hints.is_empty()).then(|| hints.join("; "))
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
        Ok(crate::tools::file_edit::display_relative_or_abs(&self.root, path))
    }
}

/// Git's own directory is machinery, not user content: a delete or move that
/// lands there cannot be undone by re-reading anything.
fn in_repository_metadata(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .map(|relative| {
            relative
                .components()
                .any(|part| part.as_os_str() == ".git")
        })
        .unwrap_or(false)
}

/// Extract the logical stem before compound extensions like `.module.css`,
/// `.inc.rs`, `.test.mjs`, or `.d.ts` while preserving leading dotfiles.
fn base_stem(filename: &str) -> String {
    let trimmed = filename.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix('.') {
        let head = rest.split('.').next().unwrap_or(rest);
        format!(".{head}")
    } else {
        trimmed.split('.').next().unwrap_or(trimmed).to_owned()
    }
}

fn suffix_after_stem(filename: &str) -> &str {
    let trimmed = filename.trim_end_matches('/');
    let start = usize::from(trimmed.starts_with('.'));
    trimmed[start..]
        .find('.')
        .map(|idx| &trimmed[start + idx..])
        .unwrap_or("")
}

fn is_style_filename(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.ends_with(".css") || lower.ends_with(".scss") || lower.ends_with(".less")
}

/// Split CamelCase / snake_case / kebab-case stems into lowercase search tokens.
fn name_tokens(stem: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = stem.chars().collect();
    for (idx, &ch) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() {
            if current.len() >= 3 {
                tokens.push(current.to_ascii_lowercase());
            }
            current.clear();
            continue;
        }
        let prev_lower = idx > 0 && chars[idx - 1].is_ascii_lowercase();
        let next_lower = idx + 1 < chars.len() && chars[idx + 1].is_ascii_lowercase();
        if ch.is_ascii_uppercase() && !current.is_empty() && (prev_lower || next_lower) {
            if current.len() >= 3 {
                tokens.push(current.to_ascii_lowercase());
            }
            current.clear();
        }
        current.push(ch);
    }
    if current.len() >= 3 {
        tokens.push(current.to_ascii_lowercase());
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

fn levenshtein_bounded(a: &str, b: &str, max_dist: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max_dist {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        let mut row_min = curr[0];
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let best = (prev[j + 1] + 1)
                .min(curr[j] + 1)
                .min(prev[j] + cost);
            curr[j + 1] = best;
            row_min = row_min.min(best);
        }
        if row_min > max_dist {
            return None;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    (prev[b.len()] <= max_dist).then_some(prev[b.len()])
}

/// If `SettingsModal.module.css` is missing, peek at `SettingsModal.{tsx,ts,jsx,js}`
/// for its actual stylesheet imports so the model gets the real CSS paths immediately.
fn sibling_style_imports(parent: &Path, wanted_stem: &str) -> Option<(String, Vec<String>)> {
    let wanted_lower = wanted_stem.to_ascii_lowercase();
    let mut candidate_file: Option<(String, PathBuf)> = None;
    for entry in std::fs::read_dir(parent).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if matches!(
            Path::new(&lower).extension().and_then(|e| e.to_str()),
            Some("tsx" | "ts" | "jsx" | "js")
        ) && base_stem(&name).to_ascii_lowercase() == wanted_lower
        {
            candidate_file = Some((name, path));
            break;
        }
    }
    let (sibling_name, path) = candidate_file?;
    let content = std::fs::read_to_string(&path).ok()?;
    let mut imports = Vec::new();
    for line in content.lines().take(60) {
        let trimmed = line.trim();
        if !trimmed.starts_with("import ") {
            continue;
        }
        for quote in ['\'', '"'] {
            let parts: Vec<&str> = trimmed.split(quote).collect();
            if parts.len() >= 3 {
                let spec = parts[parts.len() - 2].trim();
                if is_style_filename(spec) && !imports.iter().any(|existing| existing == spec) {
                    imports.push(spec.to_owned());
                    if imports.len() >= 6 {
                        return Some((sibling_name, imports));
                    }
                }
            }
        }
    }
    (!imports.is_empty()).then_some((sibling_name, imports))
}

fn ranked_similar_entries(dir: &Path, target_name: &str, limit: usize) -> Vec<String> {
    let wanted_stem = base_stem(target_name);
    let wanted_lower = wanted_stem.to_ascii_lowercase();
    if wanted_lower.is_empty() {
        return Vec::new();
    }
    let wanted_tokens = name_tokens(&wanted_stem);
    let wanted_suffix = suffix_after_stem(target_name).to_ascii_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut scored: Vec<(usize, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = path.is_dir();
            if is_dir && NOISE_DIRS.contains(&name.as_str()) {
                return None;
            }
            let cand_stem = base_stem(&name);
            let cand_lower = cand_stem.to_ascii_lowercase();
            let mut score = 0usize;
            if cand_lower == wanted_lower {
                score += 100;
            } else if wanted_lower.len() >= 3
                && cand_lower.len() >= 3
                && (cand_lower.starts_with(&wanted_lower)
                    || cand_lower.contains(&wanted_lower)
                    || wanted_lower.contains(&cand_lower))
            {
                score += 60;
            } else if wanted_lower.len() >= 4 && cand_lower.len() >= 4 {
                if let Some(dist) = levenshtein_bounded(&wanted_lower, &cand_lower, 2) {
                    score += if dist <= 1 { 45 } else { 30 };
                }
            }
            if !wanted_tokens.is_empty() {
                let cand_tokens = name_tokens(&cand_stem);
                let shared = wanted_tokens
                    .iter()
                    .filter(|token| cand_tokens.contains(token))
                    .count();
                score += shared * 22;
            }
            if score == 0 {
                return None;
            }
            if !wanted_suffix.is_empty()
                && suffix_after_stem(&name).eq_ignore_ascii_case(&wanted_suffix)
            {
                score += 10;
            }
            let display = if is_dir { format!("{name}/") } else { name };
            Some((score, display))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, name)| name).collect()
}

fn list_dir_preview(dir: &Path, limit: usize) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = path.is_dir();
            if is_dir && NOISE_DIRS.contains(&name.as_str()) {
                return None;
            }
            Some(if is_dir { format!("{name}/") } else { name })
        })
        .collect();
    names.sort();
    names.truncate(limit);
    names
}
