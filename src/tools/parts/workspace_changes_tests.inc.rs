    use super::*;

    fn shell_state() -> Result<(PathBuf, AgentServiceState)> {
        let root = std::env::temp_dir().join(format!("shell-changes-{}", crate::server::agent_service::uuid_like()));
        fs::create_dir_all(&root)?;
        let state = crate::server::agent_service::tests::test_state(&root, "127.0.0.1:1".parse()?);
        connection(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','running',0,0)", [])?;
        Ok((root, state))
    }

    #[test]
    fn canonical_safe_paths_preserve_missing_paths_and_boundaries() -> Result<()> {
        let base = std::env::temp_dir().join(format!("canonical-path-{}", crate::server::agent_service::uuid_like()));
        let workspace = base.join("workspace");
        let outside = base.join("outside");
        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&outside)?;
        fs::write(workspace.join("file.txt"), "inside")?;
        let root = fs::canonicalize(&workspace)?;
        for raw in ["file.txt", "missing/deep/file.txt"] {
            assert_eq!(safe_path(&workspace, raw)?, safe_path_canonical(&root, raw)?);
        }
        assert_eq!(safe_path_canonical(&root, "missing/deep/file.txt")?, root.join("missing/deep/file.txt"));
        assert!(safe_path_canonical(&root, "../outside/file.txt").is_err());
        assert!(safe_path_canonical(&root, &outside.join("file.txt").to_string_lossy()).is_err());
        let _ = fs::remove_dir_all(base);
        Ok(())
    }

    #[test]
    fn canonical_safe_paths_reject_leaf_links_and_ancestor_escapes() -> Result<()> {
        let base = std::env::temp_dir().join(format!("canonical-links-{}", crate::server::agent_service::uuid_like()));
        let workspace = base.join("workspace");
        let outside = base.join("outside");
        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&outside)?;
        let target = outside.join("file.txt");
        fs::write(&target, "outside")?;
        let leaf = workspace.join("leaf.txt");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&target, &leaf);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, &leaf);
        if let Err(error) = linked {
            if error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314) {
                println!("SYMLINK_CHECK_SKIPPED: {error}");
                let _ = fs::remove_dir_all(base);
                return Ok(());
            }
            return Err(error.into());
        }
        let escape = workspace.join("escape");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &escape)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &escape)?;
        let root = fs::canonicalize(&workspace)?;
        assert!(safe_path_canonical(&root, "leaf.txt").is_err());
        assert!(safe_path_canonical(&root, "escape/file.txt").is_err());
        assert!(safe_path_canonical(&root, "escape/missing/file.txt").is_err());
        fs::remove_file(leaf)?;
        #[cfg(windows)]
        fs::remove_dir(escape)?;
        #[cfg(unix)]
        fs::remove_file(escape)?;
        let _ = fs::remove_dir_all(base);
        Ok(())
    }

    #[tokio::test]
    async fn shell_interval_stays_incomplete_until_exit_and_adopts_only_matching_baselines() -> Result<()> {
        let (root, state) = shell_state()?;
        let workspace = state.workspace.as_ref();
        fs::write(root.join("own.txt"), "before\n")?;
        fs::write(root.join("external.txt"), "outside\n")?;
        fs::write(root.join("removed.txt"), "remove\n")?;
        state.shell_matching_baselines("t", workspace.root(), |_, _| false)?;
        let conn = connection(&root)?;
        for (path, bytes) in [("own.txt", b"before\n".as_slice()), ("external.txt", b"baseline\n".as_slice()), ("removed.txt", b"remove\n".as_slice())] {
            conn.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,?2,1,?3)",
                params![workspace.root().to_string_lossy(), path, bytes])?;
        }
        let before = shell_begin(&state, workspace, "t", 1).await?;
        assert_eq!(summary(&root, "t", 1)?["complete"], false);
        fs::write(root.join("own.txt"), "intermediate\n")?;
        // Returning a live session does not finish the interval.
        assert_eq!(summary(&root, "t", 1)?["undo_available"], false);
        fs::write(root.join("own.txt"), "after\n")?;
        fs::write(root.join("external.txt"), "outside plus shell\n")?;
        fs::write(root.join("created.txt"), "created\n")?;
        fs::remove_file(root.join("removed.txt"))?;
        let value = shell_finish(&state, workspace, "t", 1, before, "test-1", &["@main".into()]).await?;
        assert_eq!(value["complete"], true);
        assert_eq!(value["total"], 4);
        assert_eq!(value["undo_available"], true);
        let own: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path='own.txt'", [], |row| row.get(0))?;
        let external: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path='external.txt'", [], |row| row.get(0))?;
        assert_eq!(own, b"after\n");
        assert_eq!(external, b"baseline\n");
        let writers: usize = conn.query_row("SELECT COUNT(*) FROM agent_file_writer_versions WHERE task_id='t' AND owner='@main'", [], |row| row.get(0))?;
        assert_eq!(writers, 2);
        // Revalidate after-images before the next command: an intervening
        // external write must be its before-image, not the previous after-image.
        fs::write(root.join("own.txt"), "outside again\n")?;
        let before = shell_begin(&state, workspace, "t", 2).await?;
        fs::write(root.join("own.txt"), "next command\n")?;
        shell_finish(&state, workspace, "t", 2, before, "test-2", &["@main".into()]).await?;
        let original: Vec<u8> = conn.query_row("SELECT before_bytes FROM agent_file_changes WHERE task_id='t' AND turn=2 AND path='own.txt'", [], |row| row.get(0))?;
        assert_eq!(original, b"outside again\n");
        let base: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path='own.txt'", [], |row| row.get(0))?;
        assert_eq!(base, b"after\n");
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn failed_shell_begin_releases_its_marker_and_can_be_retried() -> Result<()> {
        for stage in ["emit", "incomplete"] {
            let (root, state) = shell_state()?;
            let conn = connection(&root)?;
            if stage == "emit" {
                conn.execute_batch("CREATE TRIGGER reject_shell_begin BEFORE INSERT ON agent_task_events WHEN NEW.kind='workspace/changes' BEGIN SELECT RAISE(ABORT,'injected emit failure'); END;")?;
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let blocked = root.join("unreadable.txt");
                    fs::write(&blocked, "secret")?;
                    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000))?;
                }
                conn.execute_batch("CREATE TRIGGER reject_shell_begin BEFORE INSERT ON agent_change_turns BEGIN SELECT RAISE(ABORT,'injected incomplete failure'); END;")?;
            }
            let result = shell_begin(&state, &state.workspace, "t", 1).await;
            assert!(result.is_err(), "injected {stage} failure did not fire");
            let active: usize = conn.query_row("SELECT COUNT(*) FROM agent_shell_change_intervals", [], |row| row.get(0))?;
            assert_eq!(active, 0, "failed {stage} begin stranded a running marker");
            if stage == "emit" { assert_eq!(summary(&root, "t", 1)?["complete"], false); }
            conn.execute_batch("DROP TRIGGER reject_shell_begin;")?;
            #[cfg(unix)]
            if stage == "incomplete" {
                use std::os::unix::fs::PermissionsExt;
                let blocked = root.join("unreadable.txt");
                let _ = fs::set_permissions(&blocked, fs::Permissions::from_mode(0o644));
                let _ = fs::remove_file(&blocked);
            }
            let before = shell_begin(&state, &state.workspace, "t", 1).await?;
            shell_finish(&state, &state.workspace, "t", 1, before, "retry", &["@main".into()]).await?;
            drop(conn);
            drop(state);
            let _ = fs::remove_dir_all(root);
        }
        Ok(())
    }

    #[test]
    fn failed_shell_begin_cleanup_cannot_remove_a_different_token() -> Result<()> {
        let (root, state) = shell_state()?;
        let workspace = state.workspace.root();
        let conn = connection(&root)?;
        conn.execute("INSERT INTO agent_shell_change_intervals(workspace,task_id,turn,token) VALUES (?1,'t',1,'new-token')", [workspace.to_string_lossy()])?;
        abandon_shell_begin(&root, workspace, "t", 1, "old-token")?;
        let active: String = conn.query_row("SELECT token FROM agent_shell_change_intervals", [], |row| row.get(0))?;
        assert_eq!(active, "new-token");
        let flags: usize = conn.query_row("SELECT COUNT(*) FROM agent_change_turns", [], |row| row.get(0))?;
        assert_eq!(flags, 0, "old-token cleanup must not mark the new interval's turn incomplete");
        abandon_shell_begin(&root, workspace, "t", 1, "new-token")?;
        let active: usize = conn.query_row("SELECT COUNT(*) FROM agent_shell_change_intervals", [], |row| row.get(0))?;
        assert_eq!(active, 0);
        assert_eq!(summary(&root, "t", 1)?["complete"], false);
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn shell_interval_metadata_survives_summary_and_preserves_effective_owners() -> Result<()> {
        let (root, state) = shell_state()?;
        fs::write(root.join("owned.txt"), "before")?;
        state.shell_matching_baselines("t", state.workspace.root(), |_, _| false)?;
        let conn = connection(&root)?;
        conn.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,'owned.txt',1,?2)",
            params![state.workspace.root().to_string_lossy(), b"before".as_slice()])?;
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        fs::write(root.join("owned.txt"), "single")?;
        let single = shell_finish(&state, &state.workspace, "t", 1, before, "single", &["a".into(), "a".into()]).await?;
        assert_eq!(single["shell_intervals"][0]["writer"], "a");
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        fs::write(root.join("owned.txt"), "shared")?;
        let shared = shell_finish(&state, &state.workspace, "t", 1, before, "shared", &["b".into(), "a".into()]).await?;
        assert_eq!(shared["shell_intervals"].as_array().unwrap().len(), 2);
        let saved = summary(&root, "t", 1)?;
        assert_eq!(shared, saved);
        let metadata = saved["shell_intervals"].as_array().unwrap().iter().find(|interval| interval["interval_id"] == "shared").unwrap();
        assert_eq!(metadata["owners"], json!(["a", "b"]));
        assert_eq!(metadata["writer"], "@shared_shell:shared");
        assert_eq!(metadata["attribution"], "unknown");
        let recorded: String = conn.query_row("SELECT metadata FROM agent_file_writer_metadata WHERE task_id='t' AND owner='@shared_shell:shared'", [], |row| row.get(0))?;
        assert_eq!(&serde_json::from_str::<Value>(&recorded)?, metadata);
        assert_eq!(ShellAttribution::new("serial", &["@main".into(), "@main".into()])?.writer(), "@main");
        assert_eq!(ShellAttribution::new("unknown", &[])?.metadata()["attribution"], "unknown");
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn untouched_large_file_keeps_shell_turn_undoable_while_mutated_large_file_is_incomplete() -> Result<()> {
        // Case 1: repo has an untouched file > FILE_LIMIT, shell edits only a small file.
        let (root, state) = shell_state()?;
        fs::File::create(root.join("large.bin"))?.set_len(FILE_LIMIT + 1)?;
        fs::write(root.join("small.txt"), "before\n")?;
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        fs::write(root.join("small.txt"), "after\n")?;
        let value = shell_finish(&state, &state.workspace, "t", 1, before, "untouched-large", &["@main".into()]).await?;
        assert_eq!(value["complete"], true, "untouched large file must not poison turn completeness: {value}");
        assert_eq!(value["undo_available"], true, "normal file edits remain undoable: {value}");
        assert_eq!(value["total"], 1);
        let conn = connection(&root)?;
        conn.execute("INSERT INTO agent_session_meta(task_id,workspace) VALUES ('t',?1)", [root.to_string_lossy()])?;
        conn.execute("UPDATE agent_tasks SET status='completed' WHERE id='t'", [])?;
        drop(conn);
        let undone = undo(&root, "t", 1)?;
        assert_eq!(undone["reverted"], true);
        assert_eq!(fs::read_to_string(root.join("small.txt"))?, "before\n");
        assert_eq!(fs::metadata(root.join("large.bin"))?.len(), FILE_LIMIT + 1);
        drop(state);
        let _ = fs::remove_dir_all(root);

        // Case 2: shell actually mutates the > FILE_LIMIT file -> explicitly incomplete & non-undoable.
        let (root2, state2) = shell_state()?;
        fs::File::create(root2.join("large.bin"))?.set_len(FILE_LIMIT + 1)?;
        let before2 = shell_begin(&state2, &state2.workspace, "t", 1).await?;
        fs::File::create(root2.join("large.bin"))?.set_len(FILE_LIMIT + 2)?;
        let value2 = shell_finish(&state2, &state2.workspace, "t", 1, before2, "mutated-large", &["@main".into()]).await?;
        assert_eq!(value2["complete"], false, "mutated large file must remain incomplete: {value2}");
        assert_eq!(value2["undo_available"], false);
        assert_eq!(value2["files"][0]["path"], "large.bin");
        assert_eq!(value2["files"][0]["unavailable"], true);
        drop(state2);
        let _ = fs::remove_dir_all(root2);
        Ok(())
    }

    #[test]
    fn cached_bytes_require_matching_metadata_and_obey_the_byte_budget() -> Result<()> {
        let root = std::env::temp_dir().join(format!("snapshot-cache-{}", crate::server::agent_service::uuid_like()));
        fs::create_dir_all(&root)?;
        let path = root.join("file.txt");
        fs::write(&path, b"old")?;
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        fs::File::options().write(true).open(&path)?.set_times(fs::FileTimes::new().set_modified(past))?;
        let (first, stat) = cached_image(&path, &mut 10, None)?;
        let cached = (stat.context("missing metadata")?, first.bytes.context("missing bytes")?);
        let (hit, _) = cached_image(&path, &mut 10, Some(&cached))?;
        assert_eq!(hit.bytes.as_deref(), Some(b"old".as_slice()));
        let (limited, _) = cached_image(&path, &mut 2, Some(&cached))?;
        assert!(limited.bytes.is_none(), "cache hits must still charge the snapshot budget");
        fs::write(&path, b"new")?; // Same length, different modification time.
        let (fresh, _) = cached_image(&path, &mut 10, Some(&cached))?;
        assert_eq!(fresh.bytes.as_deref(), Some(b"new".as_slice()));
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[tokio::test]
    async fn mutation_guards_isolate_workspaces_and_share_undo_locks() -> Result<()> {
        let (root, state) = shell_state()?;
        let other = root.join("other");
        fs::create_dir_all(&other)?;
        let conn = connection(&root)?;
        conn.execute("INSERT INTO agent_session_meta(task_id,workspace) VALUES ('t',?1)", [state.workspace.root().to_string_lossy()])?;
        conn.execute("UPDATE agent_tasks SET status='completed' WHERE id='t'", [])?;
        let cancel = CancellationToken::new();
        let broad = mutation_guard(&root, "exec_command", &json!({}), &cancel).await?;
        let independent = tokio::time::timeout(std::time::Duration::from_millis(200),
            mutation_guard(&other, "exec_command", &json!({}), &cancel)).await??;
        drop(independent);
        let independent_file = tokio::time::timeout(std::time::Duration::from_millis(200),
            mutation_guard(&other, "write_file", &json!({"path":"a.txt"}), &cancel)).await??;
        drop(independent_file);
        let alias = root.join(".");
        let (_, alias_lock) = workspace_mutations(&alias)?;
        assert!(alias_lock.try_write().is_err(), "canonical aliases share the shell lock");
        assert!(undo(&root, "t", 1).unwrap_err().to_string().contains("workspace files are being mutated"));
        drop(broad);
        assert_eq!(undo(&root, "t", 1).unwrap_err().to_string(), "conflict: no complete, uncontested snapshot is available for this turn");
        drop(conn);
        drop(state);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    /// Diagnostic only: does not change the current attribution policy.
    #[tokio::test]
    #[ignore = "read-only audit experiment: prints attribution and cache limitations"]
    async fn shell_snapshot_audit_observations() -> Result<()> {
        let (root, state) = shell_state()?;
        fs::create_dir_all(root.join("node_modules"))?;
        for relative in ["cache.txt", "concurrent.txt", "node_modules/managed.txt"] {
            fs::write(root.join(relative), b"old")?;
        }
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let cache_path = root.join("cache.txt");
        fs::File::options().write(true).open(&cache_path)?.set_times(fs::FileTimes::new().set_modified(old_time))?;
        state.shell_matching_baselines("t", state.workspace.root(), |_, _| false)?;
        let conn = connection(&root)?;
        for relative in ["cache.txt", "concurrent.txt", "node_modules/managed.txt"] {
            conn.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,?2,1,?3)",
                params![state.workspace.root().to_string_lossy(), relative, b"old".as_slice()])?;
        }
        capture(&root, "run_program", &json!({}))?;
        fs::write(&cache_path, b"new")?;
        fs::File::options().write(true).open(&cache_path)?.set_times(fs::FileTimes::new().set_modified(old_time))?;
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        println!("SHELL_AUDIT preserved_mtime actual={:?} captured={:?} matched_managed={}",
            fs::read(&cache_path)?, before.before.as_ref().and_then(|snapshot| snapshot.get("cache.txt")).and_then(|image| image.bytes.as_ref()),
            before.managed.iter().any(|path| path == "cache.txt"));
        // No subprocess is started: these stand in for an external writer's
        // changes during the interval, including a spawn-failure interval.
        fs::write(root.join("concurrent.txt"), b"external during interval")?;
        fs::write(root.join("node_modules/managed.txt"), b"command in excluded directory")?;
        let value = shell_finish(&state, &state.workspace, "t", 1, before, "audit", &["@main".into()]).await?;
        for relative in ["cache.txt", "concurrent.txt", "node_modules/managed.txt"] {
            let baseline: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path=?1", [relative], |row| row.get(0))?;
            println!("SHELL_AUDIT path={relative} actual={:?} baseline={:?}",
                String::from_utf8_lossy(&fs::read(root.join(relative))?), String::from_utf8_lossy(&baseline));
        }
        println!("SHELL_AUDIT summary={value}");
        drop(conn);
        drop(state);
        invalidate_snapshot_cache(&root);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    /// Opt-in benchmark: application cache cold/warm, not an OS disk-cache flush.
    #[test]
    #[ignore = "snapshot performance measurement; creates temporary fixture trees"]
    fn snapshot_performance_measurement() -> Result<()> {
        use std::time::{Duration, Instant, SystemTime};
        for (count, size) in [(1_000usize, 4_096usize), (5_000, 4_096), (10_000, 1_024), (1_000, 65_536)] {
            let root = std::env::temp_dir().join(format!("snapshot-perf-{}", crate::server::agent_service::uuid_like()));
            fs::create_dir_all(&root)?;
            let old = SystemTime::now() - Duration::from_secs(60);
            let content = vec![b'x'; size];
            for i in 0..count {
                let path = root.join(format!("file-{i:05}.txt"));
                fs::write(&path, &content)?;
                fs::File::options().write(true).open(&path)?.set_times(fs::FileTimes::new().set_modified(old))?;
            }
            for mode in ["cold", "warm", "changed_one", "recent_timestamps"] {
                let mut samples = Vec::new();
                let mut io = (0, 0);
                let mut retained = 0;
                let mut incomplete = false;
                for run in 0..5 {
                    if mode == "cold" { invalidate_snapshot_cache(&root); }
                    if mode == "changed_one" {
                        let mut changed = content.clone();
                        changed[0] = b'a' + run;
                        fs::write(root.join("file-00000.txt"), changed)?;
                    }
                    if mode == "recent_timestamps" {
                        for i in 0..count {
                            fs::File::options().write(true).open(root.join(format!("file-{i:05}.txt")))?
                                .set_times(fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(30)))?;
                        }
                    }
                    SNAPSHOT_READS.with(|reads| reads.set((0, 0)));
                    let start = Instant::now();
                    let snapshot = capture(&root, "run_program", &json!({}))?;
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    let reads = SNAPSHOT_READS.with(|reads| reads.get());
                    io.0 += reads.0;
                    io.1 += reads.1;
                    retained = snapshot.values().filter_map(|image| image.bytes.as_ref()).map(Vec::len).sum::<usize>();
                    incomplete = snapshot_is_incomplete(&snapshot);
                }
                samples.sort_by(f64::total_cmp);
                println!("SNAPSHOT_PERF files={count} file_bytes={size} mode={mode} samples=5 median_ms={:.3} min_ms={:.3} max_ms={:.3} mean_reads={:.1} mean_read_bytes={:.0} retained_bytes={retained} incomplete={incomplete}",
                    samples[2], samples[0], samples[4], io.0 as f64 / 5.0, io.1 as f64 / 5.0);
            }
            invalidate_snapshot_cache(&root);
            let _ = fs::remove_dir_all(root);
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "shell lifecycle performance measurement including managed baseline updates"]
    async fn shell_lifecycle_performance_measurement() -> Result<()> {
        use std::time::{Duration, Instant, SystemTime};
        for mode in ["unmanaged_noop", "managed_noop", "managed_one", "managed_all"] {
            let (root, state) = shell_state()?;
            let content = vec![b'x'; 1_024];
            let old = SystemTime::now() - Duration::from_secs(60);
            for i in 0..1_000 {
                let path = root.join(format!("file-{i:05}.txt"));
                fs::write(&path, &content)?;
                fs::File::options().write(true).open(&path)?.set_times(fs::FileTimes::new().set_modified(old))?;
            }
            if mode != "unmanaged_noop" {
                state.shell_matching_baselines("t", state.workspace.root(), |_, _| false)?;
                let mut conn = connection(&root)?;
                let tx = conn.transaction()?;
                for i in 0..1_000 {
                    tx.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,?2,1,?3)",
                        params![state.workspace.root().to_string_lossy(), format!("file-{i:05}.txt"), &content])?;
                }
                tx.commit()?;
            }
            capture(&root, "run_program", &json!({}))?;
            let mut begins = Vec::new();
            let mut finishes = Vec::new();
            for run in 0..3 {
                let start = Instant::now();
                let before = shell_begin(&state, &state.workspace, "t", run + 1).await?;
                begins.push(start.elapsed().as_secs_f64() * 1000.0);
                let changed = if mode == "managed_all" { 1_000 } else if mode == "managed_one" { 1 } else { 0 };
                let mut changed_content = content.clone();
                changed_content[0] = b'a' + run as u8;
                for i in 0..changed {
                    fs::write(root.join(format!("file-{i:05}.txt")), &changed_content)?;
                }
                let start = Instant::now();
                let value = shell_finish(&state, &state.workspace, "t", run + 1, before, &format!("perf-{run}"), &["@main".into()]).await?;
                finishes.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(value["complete"], true);
            }
            begins.sort_by(f64::total_cmp);
            finishes.sort_by(f64::total_cmp);
            println!("SHELL_LIFECYCLE_PERF files=1000 file_bytes=1024 mode={mode} samples=3 begin_median_ms={:.3} finish_median_ms={:.3} begin_max_ms={:.3} finish_max_ms={:.3}",
                begins[1], finishes[1], begins[2], finishes[2]);
            invalidate_snapshot_cache(&root);
            drop(state);
            let _ = fs::remove_dir_all(root);
        }
        Ok(())
    }

    #[tokio::test]
    async fn broad_mutations_wait_for_file_writes_and_cancel_cleanly() {
        let root = std::env::temp_dir().join(format!("changes-lock-{}", crate::server::agent_service::uuid_like()));
        fs::create_dir_all(&root).unwrap();
        let cancel = CancellationToken::new();
        let file = mutation_guard(&root, "write_file", &json!({"path":"a.txt"}), &cancel).await.unwrap();
        let broad_args = json!({});
        let pending = mutation_guard(&root, "run_project_script", &broad_args, &cancel);
        tokio::pin!(pending);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut pending).await.is_err());
        cancel.cancel();
        assert!(pending.await.is_err());
        drop(file);
        let fresh = CancellationToken::new();
        let broad = mutation_guard(&root, "run_project_script", &json!({}), &fresh).await.unwrap();
        let file_args = json!({"path":"b.txt"});
        let pending_file = mutation_guard(&root, "write_file", &file_args, &fresh);
        tokio::pin!(pending_file);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut pending_file).await.is_err());
        drop(broad);
        drop(pending_file.await.unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn undo_restores_files_in_the_tasks_own_project() {
        let base =
            std::env::temp_dir().join(format!("changes-{}", crate::server::agent_service::uuid_like()));
        let (root, project) = (base.join("server"), base.join("project"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("a.txt"), "new\n").unwrap();
        fs::write(root.join("a.txt"), "server copy\n").unwrap();
        let conn = crate::server::agent_service::open_db(&root).unwrap();
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','completed',0,0)", []).unwrap();
        conn.execute(
            "INSERT INTO agent_session_meta(task_id,workspace) VALUES ('t',?1)",
            [project.to_string_lossy()],
        )
        .unwrap();
        let mut before = Snapshot::new();
        before.insert(
            "a.txt".into(),
            Image {
                exists: true,
                bytes: Some(b"old\n".to_vec()),
                hash: None,
                stamp: String::new(),
            },
        );
        let mut after = Snapshot::new();
        after.insert(
            "a.txt".into(),
            Image {
                exists: true,
                bytes: Some(b"new\n".to_vec()),
                hash: None,
                stamp: String::new(),
            },
        );
        record(&root, "t", 1, before, after).unwrap();
        undo(&root, "t", 1).unwrap();
        assert_eq!(fs::read_to_string(project.join("a.txt")).unwrap(), "old\n");
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "server copy\n",
            "the server checkout is untouched"
        );
        let _ = fs::remove_dir_all(&base);
    }

    fn line_file(count: usize, label: &str) -> String {
        (0..count).map(|i| format!("{label} {i}\n")).collect()
    }

    /// 3_000 lines on both sides exceed the exact table (9M cells), which is the
    /// case that used to report the whole file as changed.
    fn edited_big_file() -> (String, String, usize) {
        let before = line_file(3_000, "line");
        let mut lines: Vec<String> = before.lines().map(str::to_string).collect();
        let mut changed = 0;
        for index in (100..lines.len()).step_by(70) {
            lines[index] = format!("line {index} edited");
            changed += 1;
        }
        (before, lines.into_iter().map(|line| format!("{line}\n")).collect(), changed)
    }

    #[test]
    fn a_large_file_reports_its_edited_lines() {
        let (before, after, changed) = edited_big_file();
        assert!(3_000 * 3_000 > DIFF_TABLE_CELLS, "the fixture must take the search path");
        assert_eq!(counts(&before, &after), (changed, changed, false));
    }

    #[test]
    fn an_out_of_budget_search_estimates_the_edited_lines_and_marks_them_coarse() {
        let (before, after, changed) = edited_big_file();
        // One step of search budget cannot reach the end of a 6_000-line residual.
        assert_eq!(counts_within(&before, &after, 0, 1), (changed, changed, true));
    }

    #[test]
    fn the_table_and_the_greedy_search_agree_on_small_inputs() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = ["a\n", "b\n", "c\n", "d\n"];
        for _ in 0..300 {
            let (n, m) = ((next() % 13) as usize, (next() % 13) as usize);
            let a: Vec<&str> = (0..n).map(|_| alphabet[(next() % 4) as usize]).collect();
            let b: Vec<&str> = (0..m).map(|_| alphabet[(next() % 4) as usize]).collect();
            let common = lcs(&a, &b);
            let expected = (m - common, n - common, false);
            let before = a.concat();
            let after = b.concat();
            assert_eq!(counts_within(&before, &after, usize::MAX, 0), expected, "table: {before:?} {after:?}");
            assert_eq!(counts_within(&before, &after, 0, usize::MAX), expected, "search: {before:?} {after:?}");
        }
    }

    #[test]
    fn a_recorded_large_file_change_reports_exact_counts() -> Result<()> {
        let base = std::env::temp_dir().join(format!("changes-counts-{}", crate::server::agent_service::uuid_like()));
        fs::create_dir_all(&base)?;
        crate::server::agent_service::open_db(&base)?.execute(
            "INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('t','','fake','completed',0,0)",
            [],
        )?;
        let (before, after, changed) = edited_big_file();
        let image = |bytes: String| Image { exists: true, bytes: Some(bytes.into_bytes()), hash: None, stamp: String::new() };
        let mut before_snapshot = Snapshot::new();
        before_snapshot.insert("big.txt".into(), image(before));
        let mut after_snapshot = Snapshot::new();
        after_snapshot.insert("big.txt".into(), image(after));
        record(&base, "t", 1, before_snapshot, after_snapshot)?;
        let value = summary(&base, "t", 1)?;
        assert_eq!(value["files"][0]["coarse"], false);
        assert_eq!(value["files"][0]["added"], changed);
        assert_eq!(value["files"][0]["deleted"], changed);
        assert_eq!(value["added"], changed);
        assert_eq!(value["deleted"], changed);
        let _ = fs::remove_dir_all(&base);
        Ok(())
    }
    /// 2026-11-13: a project whose ignored `scratch/` and `runs/` folders held
    /// 38,617 of its 39,588 files exceeded the snapshot's file limit, so every
    /// shell command reported that its changes could not be recorded.
    #[test]
    fn a_shell_snapshot_leaves_out_what_git_ignores() -> Result<()> {
        let root = std::env::temp_dir().join(format!("grow-snapshot-ignore-{}", crate::server::agent_service::uuid_like()));
        fs::create_dir_all(root.join("scratch"))?;
        fs::write(root.join(".gitignore"), "/scratch/\n*.log\n")?;
        for relative in ["kept.txt", ".env", "scratch/output.txt", "build.log"] {
            fs::write(root.join(relative), b"x")?;
        }
        let names = |root: &Path| -> Result<Vec<String>> { Ok(capture(root, "run_program", &json!({}))?.into_keys().collect()) };
        // Outside a repository there is nothing git ignores.
        assert_eq!(names(&root)?, [".env", ".gitignore", "build.log", "kept.txt", "scratch/output.txt"]);
        fs::create_dir_all(root.join(".git"))?;
        invalidate_snapshot_cache(&root);
        assert_eq!(names(&root)?, [".env", ".gitignore", "kept.txt"], "hidden files stay tracked; ignored ones are left out");
        fs::remove_dir_all(&root)?;
        Ok(())
    }


    async fn tracked_edit(state: &AgentServiceState, task: &str, turn: usize, file: &str, old: &str, new: &str) -> Result<()> {
        let cancel = CancellationToken::new();
        let args = json!({"path":file,"edits":[{"old_text":old,"new_text":new}]});
        let _guard = mutation_guard(state.workspace.root(), "edit_file", &args, &cancel).await?;
        execute_tracked(state.clone(), state.workspace.clone(), task.into(), turn, "edit_file".into(), args, cancel).await?;
        Ok(())
    }

    #[tokio::test]
    async fn live_shell_and_tracked_tool_write_separate_files_and_undo_independently() -> Result<()> {
        let (root, mut state) = shell_state()?;
        state.permission_mode = crate::server::agent_service::PermissionMode::FullAccess;
        let conn = connection(&root)?;
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('editor','','fake','completed',0,0)", [])?;
        fs::write(root.join("a.txt"), "a before")?;
        fs::write(root.join("b.txt"), "b before")?;
        state.shell_matching_baselines("t", &root, |_, _| false)?;
        conn.execute("INSERT INTO agent_file_baselines(task_id,workspace,path,exists_at_baseline,bytes) VALUES ('t',?1,'b.txt',1,?2)",
            params![root.canonicalize()?.to_string_lossy(), b"b before".as_slice()])?;
        let cancel = CancellationToken::new();
        let cmd = if cfg!(windows) {
            "[Console]::ReadLine() | Out-Null; [IO.File]::WriteAllText((Join-Path $PWD a.txt), 'a after')"
        } else { "read -r line; printf 'a after' > a.txt" };
        use crate::tools::shell_process::execute;
        let started = execute(&state, &state.workspace, "t", 1, None, "exec_command",
            &json!({"cmd":cmd,"yield_time_ms":0,"stdin":true}), &cancel, false).await?;
        assert_eq!(started["running"], true, "{started}");
        tokio::time::timeout(std::time::Duration::from_secs(3), tracked_edit(&state, "editor", 1, "b.txt", "b before", "b after")).await??;
        assert_eq!(fs::read(root.join("b.txt"))?, b"b after");
        assert_eq!(fs::read(root.join("a.txt"))?, b"a before");
        assert!(ensure_no_shell(&root, &root).unwrap_err().to_string().contains("SHELL_BUSY"));
        let ended = execute(&state, &state.workspace, "t", 1, None, "write_stdin",
            &json!({"session_id":started["session_id"],"chars":"go\n"}), &cancel, false).await?;
        assert_eq!(ended["running"], false, "{ended}");
        assert_eq!(ended["exit_code"], 0, "{ended}");
        assert!(ended["capture_error"].is_null(), "{ended}");
        let rows = conn.prepare("SELECT task_id,source,path,after_hash,after_bytes FROM agent_change_ledger ORDER BY seq")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, Vec<u8>>(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(rows.len(), 2, "{rows:?}");
        for ((task, source, path, hash, bytes), (want_task, want_source, want_path, want_bytes)) in rows.iter().zip([
            ("editor", "tool", "b.txt", b"b after"), ("t", "shell", "a.txt", b"a after"),
        ]) {
            assert_eq!((task.as_str(), source.as_str(), path.as_str(), bytes.as_slice()), (want_task, want_source, want_path, want_bytes.as_slice()));
            assert_eq!(hash, &format!("{:x}", Sha256::digest(want_bytes)));
        }
        let baseline: Vec<u8> = conn.query_row("SELECT bytes FROM agent_file_baselines WHERE task_id='t' AND path='b.txt'", [], |row| row.get(0))?;
        assert_eq!(baseline, b"b before", "shell must not adopt another task's tool write");
        conn.execute("UPDATE agent_tasks SET status='completed' WHERE id='t'", [])?;
        assert_eq!(summary(&root, "t", 1)?["files"][0]["path"], "a.txt");
        assert_eq!(summary(&root, "editor", 1)?["files"][0]["path"], "b.txt");
        undo(&root, "t", 1)?;
        assert_eq!(fs::read(root.join("a.txt"))?, b"a before");
        assert_eq!(fs::read(root.join("b.txt"))?, b"b after");
        undo(&root, "editor", 1)?;
        assert_eq!(fs::read(root.join("b.txt"))?, b"b before");
        Ok(())
    }

    #[tokio::test]
    async fn shell_reconciles_against_latest_tool_hash_and_flags_same_path_residual() -> Result<()> {
        let (root, state) = shell_state()?;
        fs::write(root.join("shared.txt"), "start")?;
        let before = shell_begin(&state, &state.workspace, "t", 1).await?;
        tracked_edit(&state, "t", 1, "shared.txt", "start", "first").await?;
        tracked_edit(&state, "t", 1, "shared.txt", "first", "latest").await?;
        // Even a shell restoring the original bytes is a residual change after the tool.
        fs::write(root.join("shared.txt"), "start")?;
        let value = shell_finish(&state, &state.workspace, "t", 1, before, "mixed", &["@main".into()]).await?;
        assert_eq!(value["files"][0]["conflicted"], true, "{value}");
        assert_eq!(value["undo_available"], false, "{value}");
        let conn = connection(&root)?;
        let (before, after): (Vec<u8>, Vec<u8>) = conn.query_row("SELECT before_bytes,after_bytes FROM agent_change_ledger WHERE source='shell' AND interval_id='mixed'", [],
            |row| Ok((row.get(0)?, row.get(1)?)))?;
        assert_eq!(before, b"latest");
        assert_eq!(after, b"start");
        Ok(())
    }

    #[tokio::test]
    async fn copied_files_are_recorded_and_can_be_undone() -> Result<()> {
        let (root, mut state)=shell_state()?;
        state.permission_mode=crate::server::agent_service::PermissionMode::FullAccess;
        let conn=connection(&root)?;
        conn.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('copy-task','','fake','completed',0,0)",[])?;
        fs::write(root.join("copy-source.txt"),b"copy source")?;
        let args=json!({"from":"copy-source.txt","to":"copy-result.txt","recursive":false,"overwrite":false,"create_parent_dirs":true});
        let cancel=CancellationToken::new();
        let _guard=mutation_guard(&root,"copy_path",&args,&cancel).await?;
        execute_tracked(state.clone(),state.workspace.clone(),"copy-task".into(),1,"copy_path".into(),args,cancel).await?;
        drop(_guard);
        assert_eq!(fs::read(root.join("copy-result.txt"))?,b"copy source");
        assert!(summary(&root,"copy-task",1)?["undo_available"].as_bool().unwrap());
        conn.execute("UPDATE agent_tasks SET status='completed' WHERE id='t'",[])?;
        undo(&root,"copy-task",1)?;
        assert!(!root.join("copy-result.txt").exists());
        assert_eq!(fs::read(root.join("copy-source.txt"))?,b"copy source");
        let _=fs::remove_dir_all(&root);
        Ok(())
    }


    #[test]
    fn ledger_schema_migrates_legacy_workspace_primary_key_without_dropping_intervals() -> Result<()> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE agent_shell_change_intervals(workspace TEXT PRIMARY KEY,task_id TEXT NOT NULL,turn INTEGER NOT NULL,token TEXT NOT NULL);
            INSERT INTO agent_shell_change_intervals VALUES ('ws','old-task',1,'old-token');")?;
        let tx = conn.transaction()?;
        schema(&tx)?;
        schema(&tx)?;
        tx.execute("INSERT INTO agent_shell_change_intervals(workspace,task_id,turn,token,start_seq) VALUES ('ws','other',1,'other-token',42)", [])?;
        tx.commit()?;
        let original: i64 = conn.query_row("SELECT start_seq FROM agent_shell_change_intervals WHERE token='old-token'", [], |row| row.get(0))?;
        assert_eq!(original, 0);
        let count: usize = conn.query_row("SELECT COUNT(*) FROM agent_shell_change_intervals WHERE workspace='ws'", [], |row| row.get(0))?;
        assert_eq!(count, 2);
        Ok(())
    }
