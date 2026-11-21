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
        let display_path = |path: &Path| {
            path.strip_prefix(&self.root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| path.display().to_string())
        };

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

        // Check close sibling names inside the existing parent directory.
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
        if !matches.is_empty() {
            return Some(format!("similar in parent dir: {}", matches.join(", ")));
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
        let mut entries: Vec<String> = std::fs::read_dir(parent)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_noise_dir = entry.path().is_dir() && NOISE_DIRS.contains(&name.as_str());
                (!is_noise_dir).then(|| {
                    if entry.path().is_dir() {
                        format!("{name}/")
                    } else {
                        name
                    }
                })
            })
            .collect();
        entries.sort();
        entries.truncate(8);
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
