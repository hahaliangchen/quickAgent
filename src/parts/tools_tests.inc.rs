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
    fn not_found_hint_covers_reverse_modules_and_parent_inventory() {
        let root = temp_workspace("not_found_modules");
        fs::create_dir_all(root.join("foo")).unwrap();
        fs::write(root.join("foo.rs"), "pub mod foo {}\n").unwrap();
        fs::write(root.join("foo.ts"), "export const tsModule = true\n").unwrap();
        fs::write(root.join("pkg.py"), "VALUE = 1\n").unwrap();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("dir.rs"), "pub mod dir {}\n").unwrap();
        fs::write(root.join("dir/alpha.rs"), "pub fn alpha() {}\n").unwrap();
        fs::write(root.join("dir/store.rs"), "pub fn store() {}\n").unwrap();

        let workspace = Workspace::new(&root).unwrap();
        let read = |path: &str| {
            workspace.read_file(ReadFileRequest {
                workspace_root: None,
                path: path.to_owned(),
                max_bytes: 1024,
            })
        };
        let rust_hint = read("foo/mod.rs").unwrap_err().to_string();
        assert!(rust_hint.contains("found sibling module file: foo.rs"), "{rust_hint}");
        let ts_hint = read("foo/index.ts").unwrap_err().to_string();
        assert!(ts_hint.contains("found sibling module file: foo.ts"), "{ts_hint}");
        let py_hint = read("pkg/__init__.py").unwrap_err().to_string();
        assert!(py_hint.contains("found sibling module file: pkg.py"), "{py_hint}");
        let parent_hint = read("dir/nonexistent.rs").unwrap_err().to_string();
        assert!(parent_hint.contains("found sibling module file: dir.rs"), "{parent_hint}");
        assert!(parent_hint.contains("entries in dir/: alpha.rs, store.rs"), "{parent_hint}");

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
        let response = workspace
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
        assert!(response.preview.as_deref().is_some_and(|preview| preview.contains("L2|BETA")));
        assert_eq!(
            fs::read_to_string(root.join("demo.txt")).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edit_file_returns_merged_line_numbered_preview() {
        let root = temp_workspace("edit_preview");
        fs::write(root.join("demo.txt"), "one\ntwo\nthree\nfour\nfive\nsix\nseven\n").unwrap();
        let workspace = Workspace::new(&root).unwrap();
        let response = workspace
            .edit_file(EditFileRequest {
                workspace_root: None,
                path: "demo.txt".to_string(),
                edits: vec![
                    TextEdit { old_text: "two".to_string(), new_text: "TWO".to_string() },
                    TextEdit { old_text: "six".to_string(), new_text: "SIX".to_string() },
                ],
                replace_all: false,
                expected_code_hash: None,
            })
            .unwrap();
        let preview = response.preview.expect("changed edit has preview");
        assert!(preview.contains("L2|TWO") && preview.contains("L6|SIX"), "{preview}");
        assert!(preview.contains("L1|one") && preview.contains("L7|seven"), "{preview}");
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
        let spaced_pipe = search(serde_json::json!({"query":" | "}));
        assert_eq!((spaced_pipe.match_mode, spaced_pipe.matches.len()), ("literal", 0));
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
