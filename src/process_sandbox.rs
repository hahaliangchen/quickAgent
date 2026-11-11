//! Linux process file-write confinement. Network and reads remain unrestricted.
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionBoundary {
    Host,
    ReadOnly,
    Workspace,
}

impl ExecutionBoundary {
    pub fn for_mode(mode: crate::agent_service::PermissionMode) -> Self {
        use crate::agent_service::PermissionMode;
        if !cfg!(target_os = "linux") {
            return Self::Host;
        }
        match mode {
            PermissionMode::ReadOnly => Self::ReadOnly,
            PermissionMode::WorkspaceWrite | PermissionMode::Ask | PermissionMode::AutoReview => Self::Workspace,
            PermissionMode::FullAccess => Self::Host,
        }
    }
}

/// Wrap the actual argv, never a command string. A missing or broken runner
/// must fail the operation: no fallback to the unrestricted command.
pub fn command(
    program: impl AsRef<OsStr>,
    root: &Path,
    cwd: &Path,
    boundary: ExecutionBoundary,
) -> Result<Command> {
    if boundary == ExecutionBoundary::Host {
        return Ok(Command::new(program));
    }
    ensure!(
        cfg!(target_os = "linux"),
        "SANDBOX_UNAVAILABLE: workspace sandbox is only implemented on Linux"
    );
    let root = root
        .canonicalize()
        .context("SANDBOX_UNAVAILABLE: workspace does not exist")?;
    ensure!(
        root.is_dir() && root != Path::new("/"),
        "SANDBOX_UNAVAILABLE: refusing to grant the filesystem root"
    );
    let cwd = cwd
        .canonicalize()
        .context("SANDBOX_UNAVAILABLE: working directory does not exist")?;
    ensure!(
        cwd.is_dir() && cwd.starts_with(&root),
        "SANDBOX_UNAVAILABLE: working directory is outside the workspace"
    );
    let runner = find_bwrap(&root).context("SANDBOX_UNAVAILABLE: install bubblewrap (bwrap) outside the workspace before using workspace execution")?;
    let mut cmd = Command::new(runner);
    cmd.args([
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--unshare-pid",
        "--proc",
        "/proc",
        "--die-with-parent",
    ]);
    if boundary == ExecutionBoundary::Workspace {
        // Keep a private writable /tmp without hiding workspaces nested under /tmp.
        cmd.args(["--tmpfs", "/tmp"]);
        if let Ok(relative) = root.strip_prefix("/tmp") {
            let mut parent = PathBuf::from("/tmp");
            if let Some(parts) = relative.parent() {
                for component in parts.components() {
                    parent.push(component.as_os_str());
                    cmd.arg("--dir").arg(&parent);
                }
            }
        }
        cmd.arg("--bind").arg(&root).arg(&root);
    }
    // ReadOnly keeps both the workspace and /tmp on the read-only root mount.
    // Mounted last so a workspace that contains the config directory cannot
    // expose them again.
    for secret in secret_masks() {
        cmd.arg("--ro-bind").arg("/dev/null").arg(secret);
    }
    cmd.arg("--chdir").arg(&cwd).arg("--").arg(program);
    cmd.env("TMPDIR", "/tmp")
        .env("TMP", "/tmp")
        .env("TEMP", "/tmp");
    Ok(cmd)
}

/// The secret files that exist, as the sandbox sees them: each one is
/// replaced by an empty read-only file, so a command reads nothing.
fn secret_masks() -> Vec<PathBuf> {
    let mut masks: Vec<PathBuf> = crate::ai_proxy::secret_files()
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_file())
        .collect();
    masks.dedup();
    masks
}

/// Commands that name a secret file are refused before they start. The
/// sandbox masks the files anyway; this covers execution outside it (full
/// access, other platforms) and tells the AI why instead of handing it an
/// empty file. A name alone is not enough: `.env` is common in projects, so
/// the file's own directory must be named too.
pub fn check_secret_reference(command: &str) -> Result<()> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let normalized = command.replace('\\', "/");
    for secret in crate::ai_proxy::secret_files() {
        let (Some(dir), Some(name)) = (secret.parent(), secret.file_name().and_then(OsStr::to_str)) else { continue };
        if !normalized.contains(name) {
            continue;
        }
        let mut spellings = vec![dir.to_string_lossy().replace('\\', "/")];
        if let Ok(canonical) = dir.canonicalize() {
            spellings.push(canonical.to_string_lossy().replace('\\', "/"));
        }
        if let Some(relative) = home.as_deref().and_then(|home| dir.strip_prefix(home).ok()) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if !relative.is_empty() {
                spellings.push(relative);
            }
        }
        if spellings.iter().any(|dir| !dir.is_empty() && normalized.contains(dir.as_str())) {
            anyhow::bail!(
                "SECRET_FILE: the command names {}, which holds API keys; commands never read it. The host applies credentials itself through its libraries (web, model calls). Model settings are in providers.yml and agent-generation.yml beside it",
                secret.display()
            );
        }
    }
    Ok(())
}

fn find_bwrap(root: &Path) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("bwrap"))
        .filter_map(|path| path.canonicalize().ok())
        .find(|path| path.is_file() && !path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_naming_a_secret_file_are_refused() {
        let secret = crate::ai_proxy::secret_files().remove(0);
        let path = secret.to_string_lossy().into_owned();
        let dir = secret.parent().unwrap().to_string_lossy().into_owned();
        let name = secret.file_name().unwrap().to_string_lossy().into_owned();
        for command in [format!("cat {path}"), format!("cd '{dir}' && head -5 {name}")] {
            let error = check_secret_reference(&command).unwrap_err().to_string();
            assert!(error.starts_with("SECRET_FILE"), "{command}: {error}");
        }
        if let Some(relative) = std::env::var_os("HOME").map(PathBuf::from).and_then(|home| secret.strip_prefix(home).ok().map(Path::to_path_buf)) {
            let command = format!("base64 ~/{}", relative.display());
            assert!(check_secret_reference(&command).is_err(), "{command}");
        }
        // The file name alone, or a project's own .env, is someone else's file.
        for command in [format!("cat {name}"), "cat .env".to_owned(), "ls ~/.config".to_owned()] {
            assert!(check_secret_reference(&command).is_ok(), "{command}");
        }
    }

    #[test]
    fn permission_modes_select_execution_boundary() {
        use crate::agent_service::PermissionMode;
        for (mode, linux_boundary) in [
            (PermissionMode::ReadOnly, ExecutionBoundary::ReadOnly),
            (PermissionMode::WorkspaceWrite, ExecutionBoundary::Workspace),
            (PermissionMode::Ask, ExecutionBoundary::Workspace),
            (PermissionMode::FullAccess, ExecutionBoundary::Host),
        ] {
            assert_eq!(
                ExecutionBoundary::for_mode(mode),
                if cfg!(target_os = "linux") {
                    linux_boundary
                } else {
                    ExecutionBoundary::Host
                },
                "{mode:?}"
            );
        }
    }

    #[test]
    fn host_execution_keeps_argv_and_workspace_root_is_not_granted() {
        assert!(
            command(
                "node",
                Path::new("."),
                Path::new("."),
                ExecutionBoundary::Host
            )
            .is_ok()
        );
        #[cfg(target_os = "linux")]
        for boundary in [ExecutionBoundary::ReadOnly, ExecutionBoundary::Workspace] {
            assert!(
                format!(
                    "{:#}",
                    command("node", Path::new("/"), Path::new("/"), boundary).unwrap_err()
                )
                .contains("filesystem root")
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readonly_keeps_root_readonly_and_preserves_process_isolation() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        if find_bwrap(&root).is_none() {
            eprintln!("skipped: bwrap is not installed outside the workspace");
            return;
        }
        let cmd = command("/bin/sh", &root, &root, ExecutionBoundary::ReadOnly).unwrap();
        let args: Vec<_> = cmd.as_std().get_args().collect();
        let mut expected: Vec<&OsStr> = [
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--unshare-pid",
            "--proc",
            "/proc",
            "--die-with-parent",
            "--chdir",
        ]
        .into_iter()
        .map(OsStr::new)
        .collect();
        expected.pop();
        let masks = secret_masks();
        for mask in &masks {
            expected.extend([OsStr::new("--ro-bind"), OsStr::new("/dev/null"), mask.as_os_str()]);
        }
        expected.push(OsStr::new("--chdir"));
        expected.extend([root.as_os_str(), OsStr::new("--"), OsStr::new("/bin/sh")]);
        assert_eq!(args, expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readonly_rejects_cwd_outside_workspace() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let error = command(
            "/bin/sh",
            &root,
            Path::new("/"),
            ExecutionBoundary::ReadOnly,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("working directory is outside the workspace"));
    }

    #[cfg(target_os = "linux")]
    fn bwrap_available() -> bool {
        use std::process::Stdio;
        let runner = match find_bwrap(Path::new("/nonexistent-workspace")) {
            Some(path) => path,
            None => {
                eprintln!("skipped: bwrap is not installed");
                return false;
            }
        };
        let probe = std::process::Command::new(runner)
            .args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-pid",
                "--proc",
                "/proc",
                "--die-with-parent",
                "--",
                "/bin/true",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !probe.is_ok_and(|result| result.success()) {
            eprintln!("skipped: bwrap cannot create its namespaces on this host");
            return false;
        }
        true
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn real_bwrap_readonly_allows_reads_and_blocks_workspace_tmp_and_child_writes() {
        if !bwrap_available() {
            return;
        }
        let base = std::env::temp_dir().join(format!(
            "grow-readonly-sandbox-test-{}-{}",
            std::process::id(),
            crate::agent_service::uuid_like()
        ));
        let work = base.join("work");
        let outside = base.join("outside");
        let mut tmp_name = base.file_name().unwrap().to_os_string();
        tmp_name.push(".tmp-write");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(work.join("existing"), "workspace").unwrap();
        std::fs::write(outside.join("existing"), "outside").unwrap();
        let output = command("/bin/sh", &work, &work, ExecutionBoundary::ReadOnly)
            .unwrap()
            .args([
                "-c",
                r#"
                set -eu
                test "$PWD" = "$1"
                test "$(cat existing)" = workspace
                test "$(cat "$2/existing")" = outside
                /bin/sh -c 'test "$(cat existing)" = workspace'
                for target in existing inside "$2/existing" "$2/escape" "/tmp/$3" "$TMPDIR/$3" "$TMP/$3" "$TEMP/$3"; do
                    if (printf bad > "$target") 2>/dev/null; then exit 42; fi
                    if /bin/sh -c 'printf child > "$1"' sh "$target" 2>/dev/null; then exit 43; fi
                done
                "#,
                "sh",
            ])
            .arg(work.canonicalize().unwrap())
            .arg(&outside)
            .arg(&tmp_name)
            .current_dir(&work)
            .output()
            .await
            .unwrap();
        let existing = std::fs::read_to_string(work.join("existing")).unwrap();
        let external = std::fs::read_to_string(outside.join("existing")).unwrap();
        let inside_created = work.join("inside").exists();
        let escape_created = outside.join("escape").exists();
        let tmp_created = Path::new("/tmp").join(&tmp_name).exists();
        std::fs::remove_dir_all(base).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(existing, "workspace");
        assert_eq!(external, "outside");
        assert!(!inside_created);
        assert!(!escape_created);
        assert!(!tmp_created);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn real_bwrap_preserves_workspace_and_blocks_external_writes_from_children() {
        if !bwrap_available() {
            return;
        }
        let base = std::env::temp_dir().join(format!(
            "grow-sandbox-test-{}-{}",
            std::process::id(),
            crate::agent_service::uuid_like()
        ));
        let work = base.join("work");
        let outside = base.join("outside");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let result = async {
            let mut cmd = command("/bin/sh", &work, &work, ExecutionBoundary::Workspace).unwrap();
            let output = cmd.args(["-c", "printf ok > inside; /bin/sh -c 'printf child > child'; if printf bad > \"$1/escape\" 2>/dev/null; then exit 42; fi; printf temp > /tmp/grow-temp-check", "sh"])
                .arg(&outside).current_dir(&work).output().await.unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(std::fs::read_to_string(work.join("inside")).unwrap(), "ok");
            assert_eq!(std::fs::read_to_string(work.join("child")).unwrap(), "child");
            assert!(!outside.join("escape").exists());
        }.await;
        std::fs::remove_dir_all(base).unwrap();
        result
    }
}
