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
    Workspace,
}

impl ExecutionBoundary {
    pub fn for_mode(mode: crate::agent_service::PermissionMode) -> Self {
        if cfg!(target_os = "linux")
            && !matches!(mode, crate::agent_service::PermissionMode::FullAccess)
        {
            Self::Workspace
        } else {
            Self::Host
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
    cmd.arg("--bind")
        .arg(&root)
        .arg(&root)
        .arg("--chdir")
        .arg(&cwd)
        .arg("--")
        .arg(program);
    cmd.env("TMPDIR", "/tmp")
        .env("TMP", "/tmp")
        .env("TEMP", "/tmp");
    Ok(cmd)
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
        assert!(
            format!(
                "{:#}",
                command(
                    "node",
                    Path::new("/"),
                    Path::new("/"),
                    ExecutionBoundary::Workspace
                )
                .unwrap_err()
            )
            .contains("filesystem root")
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn real_bwrap_preserves_workspace_and_blocks_external_writes_from_children() {
        use std::process::Stdio;
        let runner = match find_bwrap(Path::new("/nonexistent-workspace")) {
            Some(path) => path,
            None => {
                eprintln!("skipped: bwrap is not installed");
                return;
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
