//! Structured verification. Only this module constructs the runner argv.
use crate::{tools::process_sandbox::ExecutionBoundary, tools::Workspace};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub const TOOLS: &[&str] = &["verify_check", "verify_test"];
const PROGRAMS: &[&str] = &["cargo", "node", "go", "python", "npm"];
const RAW_LIMIT: usize = 8 * 1024 * 1024;
static SLOTS: Semaphore = Semaphore::const_new(2);

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

fn project(root: &Path, args: &Value, test: bool) -> Result<(PathBuf, &'static str)> {
    let raw = args["path"].as_str().unwrap_or(".");
    let cwd = crate::tools::file_edit::workspace_path(root, raw)?.canonicalize()?;
    ensure!(
        cwd.is_dir() && cwd.starts_with(root.canonicalize()?),
        "path must be a directory in the workspace"
    );
    let requested = args["ecosystem"].as_str().unwrap_or("auto");
    let node = if test { "node" } else { "typescript" };
    let ecosystem = match requested {
        "auto" if cwd.join("Cargo.toml").is_file() => "rust",
        "auto" if cwd.join("go.mod").is_file() => "go",
        "auto"
            if cwd
                .join(if test {
                    "package.json"
                } else {
                    "tsconfig.json"
                })
                .is_file() =>
        {
            node
        }
        "auto"
            if [
                "pyproject.toml",
                "pytest.ini",
                "setup.py",
                "requirements.txt",
            ]
            .iter()
            .any(|p| cwd.join(p).is_file()) =>
        {
            "python"
        }
        "rust" => "rust",
        "go" => "go",
        "python" => "python",
        "typescript" | "node" if requested == node => node,
        "auto" => anyhow::bail!(
            "cannot detect verification ecosystem at {}; pass ecosystem",
            cwd.display()
        ),
        _ => anyhow::bail!("unsupported ecosystem for this verification operation"),
    };
    Ok((cwd, ecosystem))
}

fn tsc(root: &Path, cwd: &Path) -> Result<PathBuf> {
    let boundary = root.canonicalize()?;
    cwd.ancestors().take_while(|p| p.starts_with(&boundary)).find_map(|p| {
        let file = p.join("node_modules/typescript/bin/tsc").canonicalize().ok()?;
        (file.starts_with(&boundary) && file.is_file())
            .then(|| crate::tools::file_edit::strip_verbatim_prefix(&file))
    }).context("local TypeScript is not installed inside the workspace; install the project dependencies first")
}

fn invocation(
    root: &Path,
    cwd: &Path,
    ecosystem: &str,
    test: bool,
    args: &Value,
) -> Result<(&'static str, Vec<String>)> {
    let filter = args["filter"].as_str().unwrap_or("");
    let target = args["target"].as_str();
    ensure!(
        filter.len() <= 512 && !filter.contains('\0'),
        "invalid test filter"
    );
    ensure!(
        target.is_none_or(|t| !t.is_empty()
            && t.len() <= 128
            && t.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))),
        "target must be a simple Cargo binary name"
    );
    ensure!(
        target.is_none() || ecosystem == "rust",
        "target is supported only for Rust"
    );
    let specs: (&str, &[&str]) = match (ecosystem, test) {
        ("rust", false) => ("cargo", &["check", "--message-format=json"]),
        ("rust", true) => (
            "cargo",
            &["test", "--message-format=json", "--", "--test-threads=2"],
        ),
        ("typescript", false) => ("node", &["--no-warnings"]),
        ("node", true) => ("node", &["--test"]),
        ("go", false) => ("go", &["vet", "./..."]),
        ("go", true) => ("go", &["test", "-json", "./..."]),
        ("python", false) => (
            "python",
            &[
                "-m",
                "compileall",
                "-q",
                "-x",
                r"(^|[\\/])(\.git|\.venv|venv|node_modules|target)([\\/]|$)",
                ".",
            ],
        ),
        ("python", true) => ("python", &["-m", "pytest", "-q", "--tb=short"]),
        _ => unreachable!(),
    };
    let (mut program, fixed) = specs;
    let mut argv: Vec<String> = fixed.iter().map(|s| (*s).to_owned()).collect();
    match (ecosystem, test) {
        ("rust", false) => {
            if args["include_tests"] == true {
                argv.push("--tests".into());
            }
            if let Some(t) = target {
                argv.push(format!("--bin={t}"));
            }
        }
        ("rust", true) if !filter.is_empty() => argv.push(format!("{filter}")),
        ("typescript", false) => {
            argv.push(
                tsc(root, cwd)?
                    .to_str()
                    .context("TypeScript path is not UTF-8")?
                    .into(),
            );
            argv.extend(["--noEmit", "--pretty", "false"].map(str::to_owned));
        }
        ("node", true) => {
            let manifest = cwd.join("package.json");
            let package = std::fs::read_to_string(manifest)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let script = package
                .as_ref()
                .and_then(|p| p.pointer("/scripts/test"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !script.is_empty() && !script.starts_with("echo ") && script != "node --test" {
                program = "npm";
                argv = vec!["test".into()];
                if !filter.is_empty() {
                    argv.extend(["--".into(), format!("--testNamePattern={filter}")]);
                }
            } else if !filter.is_empty() {
                argv.push(format!("--test-name-pattern={filter}"));
            }
        }
        ("go", true) if !filter.is_empty() => argv.insert(2, format!("-run={filter}")),
        ("python", true) if !filter.is_empty() => argv.push(format!("-k={filter}")),
        _ => {}
    }
    if ecosystem == "rust" && test {
        ensure!(
            !filter.starts_with('-'),
            "Rust filter cannot start with a dash"
        );
    }
    Ok((program, argv))
}

#[cfg(unix)]
fn disk_guard(cwd: &Path) -> Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(cwd.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs only initializes the output on a successful return.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot check filesystem free space");
    }
    let stat = unsafe { stat.assume_init() };
    ensure!(
        stat.f_bavail as u128 * stat.f_frsize as u128 >= 512 * 1024 * 1024,
        "verification requires at least 512 MiB free"
    );
    Ok(())
}

#[cfg(windows)]
fn disk_guard(cwd: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = cwd.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("cannot check filesystem free space");
    }
    ensure!(
        free >= 512 * 1024 * 1024,
        "verification requires at least 512 MiB free"
    );
    Ok(())
}

pub async fn execute(
    workspace: &Workspace,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
    boundary: ExecutionBoundary,
) -> Result<Value> {
    ensure!(
        boundary != ExecutionBoundary::ReadOnly,
        "verification is not allowed in a read-only session"
    );
    let testing = name == "verify_test";
    let (cwd, ecosystem) = project(workspace.root(), args, testing)?;
    let (program, argv) = invocation(workspace.root(), &cwd, ecosystem, testing, args)?;
    let limit_key = if testing {
        "max_failures"
    } else {
        "max_diagnostics"
    };
    let limit = args[limit_key]
        .as_u64()
        .unwrap_or(if testing { 20 } else { 50 })
        .clamp(1, 200) as usize;
    let _slot = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("verification cancelled before launch"),
        slot = SLOTS.acquire() => slot?,
    };
    disk_guard(&cwd)?;
    let command = std::iter::once(program)
        .chain(argv.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let raw = crate::tools::program_execution::execute_allowlisted(workspace, &json!({
        "program":program,"args":argv,"project_path":cwd,"timeout_seconds":args["timeout_secs"].as_u64().unwrap_or(120),"max_output_bytes":RAW_LIMIT
    }), cancel, boundary, PROGRAMS, RAW_LIMIT).await?;
    let mut value = if testing {
        tests(ecosystem, &raw, limit)
    } else {
        checks(ecosystem, &raw, limit)
    };
    value["ecosystem"] = json!(ecosystem);
    value["command"] = json!(command);
    value["ok"] = raw["process_success"].clone();
    value["duration_ms"] = raw["elapsed_ms"].clone();
    value["outcome"] = raw["outcome"].clone();
    value["truncated"] = json!(
        value["truncated"] == true
            || raw["stdout_truncated"] == true
            || raw["stderr_truncated"] == true
            || raw["stdout_read_error"].is_string()
            || raw["stderr_read_error"].is_string()
    );
    if raw["error_message"].is_string() {
        value["execution_error"] = raw["error_message"].clone();
    }
    value["__verify_raw"] = raw;
    Ok(value)
}

fn diagnostic(
    file: Value,
    line: Value,
    col: Value,
    severity: &str,
    code: Value,
    message: &str,
) -> Value {
    let file = match file {
        Value::String(path) => Value::String(path.replace('\\', "/")),
        other => other,
    };
    json!({"file":file,"line":line,"col":col,"severity":severity,"code":code,"message":message.chars().take(1000).collect::<String>()})
}

fn checks(ecosystem: &str, raw: &Value, limit: usize) -> Value {
    static LOCATION: OnceLock<regex::Regex> = OnceLock::new();
    static TS: OnceLock<regex::Regex> = OnceLock::new();
    static PY: OnceLock<regex::Regex> = OnceLock::new();
    let loc =
        LOCATION.get_or_init(|| regex::Regex::new(r"^(.+?):(\d+):(?:(\d+):)?\s*(\S.*)$").unwrap());
    let ts = TS.get_or_init(|| {
        regex::Regex::new(r"^(?:(.+)\((\d+),(\d+)\): )?(error|warning) (TS\d+): (.+)$").unwrap()
    });
    let py = PY.get_or_init(|| regex::Regex::new(r#"File "([^"]+)", line (\d+)"#).unwrap());
    let mut items = Vec::new();
    let mut errors = 0;
    let mut warnings = 0;
    let mut py_file = None;
    for line in streams(raw) {
        let item = match ecosystem {
            "rust" => {
                let Ok(row) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if row["reason"] != "compiler-message" {
                    continue;
                }
                let m = &row["message"];
                let severity = m["level"].as_str().unwrap_or("");
                if !matches!(severity, "error" | "warning") {
                    continue;
                }
                let span = m["spans"]
                    .as_array()
                    .and_then(|s| {
                        s.iter()
                            .find(|p| p["is_primary"] == true)
                            .or_else(|| s.first())
                    })
                    .unwrap_or(&Value::Null);
                diagnostic(
                    span["file_name"].clone(),
                    span["line_start"].clone(),
                    span["column_start"].clone(),
                    severity,
                    m["code"]["code"].clone(),
                    m["message"].as_str().unwrap_or(""),
                )
            }
            "typescript" => {
                let Some(c) = ts.captures(line) else { continue };
                diagnostic(
                    json!(c.get(1).map(|m| m.as_str())),
                    num(c.get(2)),
                    num(c.get(3)),
                    &c[4],
                    json!(&c[5]),
                    &c[6],
                )
            }
            "go" => {
                let Some(c) = loc.captures(line.trim_start_matches("vet: ")) else {
                    continue;
                };
                diagnostic(
                    json!(&c[1]),
                    num(c.get(2)),
                    num(c.get(3)),
                    "error",
                    Value::Null,
                    &c[4],
                )
            }
            "python" => {
                if let Some(c) = py.captures(line) {
                    py_file = Some((c[1].to_owned(), num(c.get(2))));
                    continue;
                }
                let Some((code, msg)) = line.trim().split_once(':') else {
                    continue;
                };
                if !["SyntaxError", "IndentationError", "TabError"].contains(&code) {
                    continue;
                }
                let (file, row) = py_file
                    .take()
                    .map_or((Value::Null, Value::Null), |(p, n)| (json!(p), n));
                diagnostic(file, row, Value::Null, "error", json!(code), msg.trim())
            }
            _ => continue,
        };
        if item["severity"] == "warning" {
            warnings += 1;
        } else {
            errors += 1;
        }
        if items.len() < limit {
            items.push(item);
        }
    }
    json!({"error_count":errors,"warning_count":warnings,"truncated":errors+warnings > items.len(),"diagnostics":items})
}

fn num(value: Option<regex::Match<'_>>) -> Value {
    json!(value.and_then(|s| s.as_str().parse::<u64>().ok()))
}
fn streams(raw: &Value) -> impl Iterator<Item = &str> {
    ["stdout", "stderr"]
        .into_iter()
        .flat_map(|key| raw[key].as_str().unwrap_or("").lines())
}

fn tests(ecosystem: &str, raw: &Value, limit: usize) -> Value {
    static COUNTS: OnceLock<regex::Regex> = OnceLock::new();
    static TAP: OnceLock<regex::Regex> = OnceLock::new();
    static RUST_LOCATION: OnceLock<regex::Regex> = OnceLock::new();
    let counts = COUNTS.get_or_init(|| {
        regex::Regex::new(r"(\d+)\s+(passed|failed|skipped|ignored|xfailed|xpassed|pending|todo)\b")
            .unwrap()
    });
    let tap = TAP.get_or_init(|| {
        regex::Regex::new(r"^(?:ℹ |#\s*)(pass|fail|skipped|skip|todo)\s*[: ]\s*(\d+)$").unwrap()
    });
    let rust_location = RUST_LOCATION.get_or_init(|| {
        regex::Regex::new(r"^thread '([^']+)'(?: \([^)]*\))? panicked at (.+):(\d+):\d+:$").unwrap()
    });
    let mut tally = [0u64; 3];
    let mut failures = Vec::<Value>::new();
    let mut failure_count = 0usize;
    let mut next_detail = None;
    let mut go_details = std::collections::BTreeMap::<(String, String), String>::new();
    let mut go_terminal = std::collections::BTreeMap::<(String, String), String>::new();
    for raw_line in streams(raw) {
        let line = raw_line.trim();
        if ecosystem == "go" {
            let Ok(row) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let test = row["Test"].as_str().unwrap_or("");
            let pkg = row["Package"].as_str().unwrap_or("");
            if test.is_empty() {
                continue;
            }
            let key = (pkg.to_owned(), test.to_owned());
            match row["Action"].as_str().unwrap_or("") {
                action @ ("pass" | "fail" | "skip") => {
                    go_terminal.insert(key, action.to_owned());
                }
                "output" if go_details.len() < 4096 || go_details.contains_key(&key) => {
                    let s = row["Output"].as_str().unwrap_or("").trim();
                    let detail = go_details.entry(key).or_default();
                    if !s.starts_with("===") && !s.starts_with("---") && detail.len() < 1000 {
                        detail.push_str(&s.chars().take(700).collect::<String>());
                        detail.push('\n');
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Some(i) = next_detail.take() {
            let item: &mut Value = &mut failures[i];
            if !line.is_empty() {
                item["message"] = json!(line.chars().take(1000).collect::<String>());
            }
        }
        if ecosystem == "rust"
            && let Some(c) = rust_location.captures(line)
        {
            if let Some(i) = failures.iter().position(|f| f["name"] == &c[1]) {
                failures[i]["file"] = json!(c[2].replace('\\', "/"));
                failures[i]["line"] = num(c.get(3));
                next_detail = Some(i);
            }
        }
        let summary = (ecosystem == "rust" && line.starts_with("test result:"))
            || (ecosystem == "python" && line.contains(" in ") && line.ends_with('s'))
            || (ecosystem == "node"
                && ["Tests:", "Test Files"].iter().any(|p| line.starts_with(p))
                && !line.starts_with("Test Files"));
        if summary {
            for c in counts.captures_iter(line) {
                let i = match &c[2] {
                    "passed" => 0,
                    "failed" | "xpassed" => 1,
                    _ => 2,
                };
                tally[i] += c[1].parse::<u64>().unwrap_or(0);
            }
        }
        if ecosystem == "node"
            && let Some(c) = tap.captures(line)
        {
            let i = match &c[1] {
                "pass" => 0,
                "fail" => 1,
                _ => 2,
            };
            tally[i] += c[2].parse::<u64>().unwrap_or(0);
        }
        let fail = match ecosystem {
            "rust" => line
                .strip_prefix("test ")
                .and_then(|s| s.strip_suffix(" ... FAILED"))
                .map(|s| (s, "test failed")),
            "node" => line
                .strip_prefix("not ok ")
                .and_then(|s| s.split_once(" - "))
                .map(|(_, name)| (name, "test failed")),
            "python" => line
                .strip_prefix("FAILED ")
                .or_else(|| line.strip_prefix("ERROR "))
                .map(|s| s.split_once(" - ").unwrap_or((s, "test failed"))),
            _ => None,
        };
        if let Some((name, msg)) = fail {
            failure_count += 1;
            if failures.len() < limit {
                failures.push(
                    json!({"name":name,"message":msg.chars().take(1000).collect::<String>()}),
                );
            }
        }
    }
    for (key, status) in go_terminal {
        let i = match status.as_str() {
            "pass" => 0,
            "fail" => 1,
            _ => 2,
        };
        tally[i] += 1;
        if i == 1 {
            failure_count += 1;
            if failures.len() < limit {
                let message = go_details
                    .remove(&key)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "test failed".into());
                failures
                    .push(json!({"name":format!("{}::{}",key.0,key.1),"message":message.trim()}));
            }
        }
    }
    tally[1] = tally[1].max(failure_count as u64);
    json!({"passed":tally[0],"failed":tally[1],"skipped":tally[2],"truncated":tally[1] > limit as u64,"failures":failures})
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"verify_check","description":"Run the ecosystem compiler or static check using fixed argv; return structured diagnostics and a scoped raw output asset in chat.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"ecosystem":{"enum":["auto","rust","typescript","go","python"]},"target":{"type":"string"},"include_tests":{"type":"boolean"},"max_diagnostics":{"type":"integer","minimum":1,"maximum":200,"default":50}}}}),
        json!({"name":"verify_test","description":"Run the ecosystem test runner with bounded duration, return counters, failure details and a scoped raw output asset in chat.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"ecosystem":{"enum":["auto","rust","node","go","python"]},"filter":{"type":"string"},"timeout_secs":{"type":"integer","minimum":1,"maximum":600,"default":120},"max_failures":{"type":"integer","minimum":1,"maximum":200,"default":20}}}}),
    ]
}

#[cfg(test)]
mod tests_fixture {
    use super::*;
    use std::fs;

    struct Temp(PathBuf);
    impl Temp {
        fn new(parent: &Path) -> Self {
            let p = parent.join(format!("verify-{}", crate::server::agent_service::uuid_like()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn rust_fixture_has_exact_compiler_span_and_failing_test() {
        let dir = Temp::new(&std::env::temp_dir());
        fs::create_dir(dir.0.join("src")).unwrap();
        fs::write(dir.0.join("Cargo.toml"), "[package]\nname=\"verify_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[workspace]\n").unwrap();
        let file = dir.0.join("src/lib.rs");
        fs::write(&file, "pub fn bad() { let _: u32 = \"wrong\"; }\n").unwrap();
        let root = Workspace::new(&dir.0).unwrap();
        let cancel = CancellationToken::new();
        let check = execute(
            &root,
            "verify_check",
            &json!({}),
            &cancel,
            ExecutionBoundary::Host,
        )
        .await
        .unwrap();
        assert_eq!(check["ok"], false);
        let diagnostic = &check["diagnostics"][0];
        assert_eq!(diagnostic["code"], "E0308", "{check}");
        assert_eq!(diagnostic["file"], "src/lib.rs");
        assert_eq!(diagnostic["line"], 1);
        assert_eq!(diagnostic["col"], 29);
        fs::write(
            &file,
            "#[test]\nfn passes() {}\n#[test]\nfn fails() { panic!(\"fixture failed\") }\n",
        )
        .unwrap();
        let result = execute(
            &root,
            "verify_test",
            &json!({}),
            &cancel,
            ExecutionBoundary::Host,
        )
        .await
        .unwrap();
        assert_eq!(result["passed"], 1, "{result}");
        assert_eq!(result["failed"], 1, "{result}");
        assert_eq!(result["failures"][0]["name"], "fails");
        assert_eq!(result["failures"][0]["line"], 4);
        assert_eq!(result["failures"][0]["file"], "src/lib.rs");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn typescript_fixture_uses_installed_compiler_and_reports_exact_diagnostic() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let deps = repo.join("frontend/node_modules");
        if !deps.join("typescript/bin/tsc").is_file() {
            return;
        }
        let dir = Temp::new(&repo.join("target"));
        std::os::unix::fs::symlink(deps, dir.0.join("node_modules")).unwrap();
        fs::write(
            dir.0.join("tsconfig.json"),
            r#"{"compilerOptions":{"strict":true,"types":[]},"files":["bad.ts"]}"#,
        )
        .unwrap();
        fs::write(dir.0.join("bad.ts"), "const count: number = 'wrong';\n").unwrap();
        let workspace = Workspace::new(repo).unwrap();
        let check = execute(
            &workspace,
            "verify_check",
            &json!({"path":dir.0}),
            &CancellationToken::new(),
            ExecutionBoundary::Host,
        )
        .await
        .unwrap();
        assert_eq!(check["ok"], false);
        assert_eq!(
            check["diagnostics"][0],
            json!({"file":"bad.ts","line":1,"col":7,"code":"TS2322","severity":"error","message":"Type 'string' is not assignable to type 'number'."}),
            "{check}"
        );
    }

    #[test]
    fn summaries_keep_total_counts_above_preview_limit() {
        let node = tests(
            "node",
            &json!({"stdout":"not ok 1 - first\nnot ok 2 - second\n# pass 3\n# fail 2\n# skipped 1"}),
            1,
        );
        assert_eq!(node["passed"], 3);
        assert_eq!(node["skipped"], 1);
        assert_eq!(node["failed"], 2);
        assert_eq!(node["truncated"], true);
        assert_eq!(node["failures"].as_array().unwrap().len(), 1);
        let go = checks(
            "go",
            &json!({"stderr":"./a.go:3:5: missing\n./b.go:4:2: missing"}),
            1,
        );
        assert_eq!(go["error_count"], 2);
        assert_eq!(go["diagnostics"][0]["col"], 5);
        assert_eq!(go["truncated"], true);
        assert!(
            invocation(
                Path::new("."),
                Path::new("."),
                "rust",
                true,
                &json!({"filter":"--help"})
            )
            .is_err()
        );
    }
}
