//! Pure path, URL, base64, YAML and read-only environment operations for the
//! flow runtime; no process or filesystem access.
use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

const INPUT_LIMIT: usize = 64 * 1024;
const OUTPUT_LIMIT: usize = 256 * 1024;
/// base64 carries file-sized text, larger than a path or URL.
const BASE64_LIMIT: usize = 4 * 1024 * 1024;

pub(super) fn arity(name: &str) -> Option<(usize, usize)> {
    Some(match name {
        "path.join" | "path.resolve" => (1, 32),
        "path.relative" => (2, 2),
        "path.basename" | "url.parse" => (1, 2),
        "path.normalize" | "path.dirname" | "path.extname" => (1, 1),
        "os.homedir" | "os.platform" | "os.tmpdir" => (0, 0),
        "process.env" | "atob" | "btoa" => (1, 1),
        "TextEncoder.encode" => (0, 1),
        "Buffer.byteLength" => (1, 2),
        "yaml.parse" | "yaml.load" => (1, 2),
        "yaml.stringify" | "yaml.dump" => (1, 3),
        _ => return None,
    })
}

/// Environment variables whose names look like credentials stay unreadable:
/// a flow's values reach model context and saved history.
fn credential_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "COOKIE", "SESSION", "PRIVATE"]
        .iter()
        .any(|part| upper.contains(part))
}

/// Node's os.platform() names.
fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// atob: forgiving base64 to a binary string, one char per byte, as in Node.
fn atob(input: &str) -> Result<String> {
    let mut compact: String = input.chars().filter(|c| !matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r')).collect();
    if compact.len() % 4 == 0 {
        let padding = if compact.ends_with("==") { 2 } else { usize::from(compact.ends_with('=')) };
        compact.truncate(compact.len() - padding);
    }
    let invalid = || anyhow::anyhow!("InvalidCharacterError: The string to be decoded is not correctly encoded.");
    if compact.len() % 4 == 1 || !compact.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
        return Err(invalid());
    }
    let bytes = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    )
    .decode(compact.as_bytes())
    .map_err(|_| invalid())?;
    Ok(bytes.into_iter().map(char::from).collect())
}

/// btoa: a binary string (every char at most U+00FF) to base64.
fn btoa(input: &str) -> Result<String> {
    let bytes = input
        .chars()
        .map(|c| u8::try_from(u32::from(c)).map_err(|_| anyhow::anyhow!("InvalidCharacterError: Invalid character")))
        .collect::<Result<Vec<_>>>()?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

// Lexical normalization only: does not touch the filesystem or follow symlinks.
fn normalize(input: &str, preserve_trailing: bool) -> String {
    let mut prefix = PathBuf::new();
    let mut parts: Vec<String> = Vec::new();
    let mut rooted = false;
    for component in Path::new(input).components() {
        match component {
            Component::Prefix(p) => prefix.push(p.as_os_str()),
            Component::RootDir => {
                prefix.push(std::path::MAIN_SEPARATOR.to_string());
                rooted = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|p| p != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push("..".into());
                }
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
        }
    }
    for part in parts {
        prefix.push(part);
    }
    let mut result = prefix.to_string_lossy().into_owned();
    if result.is_empty() {
        result.push('.');
    }
    if preserve_trailing
        && !input.is_empty()
        && input.chars().last().is_some_and(separator)
        && !result.chars().last().is_some_and(separator)
    {
        result.push(std::path::MAIN_SEPARATOR);
    }
    result
}

fn basename(input: &str) -> &str {
    let trimmed = input.trim_end_matches(separator);
    match trimmed.rfind(separator) {
        Some(index) => &trimmed[index + 1..],
        None => trimmed,
    }
}

#[cfg(windows)]
fn dirname(input: &str) -> String {
    if input.is_empty() {
        return ".".into();
    }
    let path = Path::new(input);
    if path.parent().is_none() && path.has_root() {
        return normalize(input, false);
    }
    let parent = path.parent().unwrap_or(Path::new(""));
    let text = parent.to_string_lossy();
    if text.is_empty() {
        ".".into()
    } else {
        text.into_owned()
    }
}

#[cfg(not(windows))]
fn dirname(input: &str) -> String {
    if input.is_empty() {
        return ".".into();
    }
    let root = input.starts_with(separator);
    let mut seen_non_separator = false;
    let mut end = None;
    for (index, ch) in input.char_indices().rev() {
        if index == 0 {
            break;
        }
        if separator(ch) {
            if seen_non_separator {
                end = Some(index);
                break;
            }
        } else {
            seen_non_separator = true;
        }
    }
    match end {
        None => {
            if root {
                std::path::MAIN_SEPARATOR.to_string()
            } else {
                ".".into()
            }
        }
        Some(1) if root && !cfg!(windows) => "//".into(),
        Some(index) => input[..index].into(),
    }
}

fn extension(input: &str) -> &str {
    let base = basename(input);
    if base == "." || base == ".." {
        return "";
    }
    match base.rfind('.') {
        Some(index) if index > 0 => &base[index..],
        _ => "",
    }
}

fn url_parse(input: &str, base: Option<&str>) -> Result<Value> {
    let parsed = match base {
        Some(base) => url::Url::parse(base).and_then(|base| base.join(input)),
        None => url::Url::parse(input),
    }
    .map_err(|error| anyhow::anyhow!("URL parse error: {error}"))?;
    let search = parsed
        .query()
        .filter(|value| !value.is_empty())
        .map(|value| format!("?{value}"))
        .unwrap_or_default();
    let hash = parsed
        .fragment()
        .filter(|value| !value.is_empty())
        .map(|value| format!("#{value}"))
        .unwrap_or_default();
    let hostname = parsed.host_str().unwrap_or("");
    let hostname = if hostname.contains(':') && !hostname.starts_with('[') {
        format!("[{hostname}]")
    } else {
        hostname.to_owned()
    };
    Ok(
        json!({"href": parsed.as_str(), "origin": parsed.origin().ascii_serialization(),
        "protocol": format!("{}:", parsed.scheme()), "hostname": hostname,
        "port": parsed.port().map(|v|v.to_string()).unwrap_or_default(), "pathname": parsed.path(),
        "search": search, "hash": hash}),
    )
}

/// YAML in and out, as the `yaml` package's parse/stringify and js-yaml's
/// load/dump do it: one document, mappings become objects with string keys,
/// merge keys (`<<`) applied. Options are accepted and ignored.
fn yaml(name: &str, args: &[Value]) -> Result<Option<Value>> {
    match name {
        "yaml.parse" | "yaml.load" => {
            let text = args[0].as_str().with_context(|| format!("TypeError: {name} takes a YAML string"))?;
            ensure!(text.len() <= BASE64_LIMIT, "{name} input exceeds {} KiB", BASE64_LIMIT / 1024);
            let mut value: serde_yaml::Value = serde_yaml::from_str(text).map_err(|error| anyhow::anyhow!("YAMLParseError: {error}"))?;
            value.apply_merge().map_err(|error| anyhow::anyhow!("YAMLParseError: {error}"))?;
            Ok(Some(yaml_to_json(value)))
        }
        "yaml.stringify" | "yaml.dump" => {
            let text = serde_yaml::to_string(&args[0])?;
            ensure!(text.len() <= BASE64_LIMIT, "{name} output exceeds {} KiB", BASE64_LIMIT / 1024);
            Ok(Some(json!(text)))
        }
        _ => Ok(None),
    }
}

fn yaml_to_json(value: serde_yaml::Value) -> Value {
    use serde_yaml::Value as Yaml;
    match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(b) => json!(b),
        Yaml::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!(i)
            } else if let Some(u) = n.as_u64() {
                json!(u)
            } else {
                n.as_f64().and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number)
            }
        }
        Yaml::String(s) => json!(s),
        Yaml::Sequence(items) => Value::Array(items.into_iter().map(yaml_to_json).collect()),
        Yaml::Mapping(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let key = match yaml_to_json(key) {
                        Value::String(s) => s,
                        Value::Null => "null".to_owned(),
                        other => other.to_string(),
                    };
                    (key, yaml_to_json(value))
                })
                .collect(),
        ),
        Yaml::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

/// UTF-8 sizes and bytes, so code never counts them in a per-character loop
/// that exhausts its instruction budget.
fn text_encoding(name: &str, args: &[Value]) -> Result<Option<Value>> {
    const ENCODE_LIMIT: usize = 256 * 1024;
    Ok(Some(match name {
        // Like the web API, encode() and encode(undefined) give no bytes.
        "TextEncoder.encode" => {
            let text = match args.first() {
                None | Some(Value::Null) => "",
                Some(Value::String(text)) => text.as_str(),
                Some(_) => bail!("TypeError: TextEncoder.encode takes a string"),
            };
            ensure!(
                text.len() <= ENCODE_LIMIT,
                "RangeError: TextEncoder.encode input exceeds 256 KiB; use Buffer.byteLength(text) for a size"
            );
            json!(text.as_bytes())
        }
        "Buffer.byteLength" => {
            let text = args[0].as_str().context("TypeError: Buffer.byteLength takes a string")?;
            match args.get(1) {
                None | Some(Value::Null) => {}
                Some(Value::String(encoding)) if matches!(encoding.to_ascii_lowercase().as_str(), "utf8" | "utf-8") => {}
                Some(other) => bail!("Buffer.byteLength supports only the utf8 encoding here, not {other}"),
            }
            json!(text.len())
        }
        _ => return Ok(None),
    }))
}

pub(super) fn call(name: &str, args: Vec<Value>) -> Result<Value> {
    let (min, max) = arity(name).with_context(||format!("{name} is unavailable in the flow runtime. {}", crate::agent_program::NOT_AVAILABLE_HINT))?;
    ensure!(
        (min..=max).contains(&args.len()),
        "invalid built-in argument count"
    );
    if let Some(value) = yaml(name, &args)? {
        return Ok(value);
    }
    if let Some(value) = text_encoding(name, &args)? {
        return Ok(value);
    }
    let strings = args
        .iter()
        .map(|v| v.as_str().context("built-in arguments must be strings"))
        .collect::<Result<Vec<_>>>()?;
    let (input_limit, output_limit) = if matches!(name, "atob" | "btoa") {
        (BASE64_LIMIT, BASE64_LIMIT * 2)
    } else {
        (INPUT_LIMIT, OUTPUT_LIMIT)
    };
    ensure!(
        serde_json::to_vec(&json!({"name":name,"args":args}))?.len() <= input_limit,
        "built-in input exceeds {} KiB",
        input_limit / 1024
    );
    let value = match name {
        "atob" => json!(atob(strings[0])?),
        "btoa" => json!(btoa(strings[0])?),
        "os.platform" => json!(platform()),
        "os.homedir" => {
            let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
            json!(std::env::var(var).context("os.homedir: the home directory is not set")?)
        }
        "os.tmpdir" => {
            // Node drops a trailing separator unless the path is a root.
            let dir = std::env::temp_dir().to_string_lossy().into_owned();
            let trimmed = dir.trim_end_matches(separator);
            json!(if trimmed.is_empty() { dir.as_str() } else { trimmed })
        }
        "process.env" => {
            ensure!(
                !credential_name(strings[0]),
                "process.env.{} looks like a credential and is not readable from flow code; pass what is needed explicitly or use a host library that handles the credential",
                strings[0]
            );
            std::env::var(strings[0]).map(Value::String).unwrap_or(Value::Null)
        }
        "path.normalize" => json!(normalize(strings[0], true)),
        "path.join" => {
            let joined = strings
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(&std::path::MAIN_SEPARATOR.to_string());
            json!(normalize(&joined, true))
        }
        "path.resolve" => {
            let index = strings
                .iter()
                .rposition(|part| Path::new(part).is_absolute())
                .context("path.resolve requires an absolute path argument")?;
            let joined = strings[index..].join(&std::path::MAIN_SEPARATOR.to_string());
            json!(normalize(&joined, false))
        }
        "path.relative" => {
            ensure!(
                strings.iter().all(|part| Path::new(part).is_absolute()),
                "path.relative requires absolute paths"
            );
            let a = normalize(strings[0], false);
            let b = normalize(strings[1], false);
            let left: Vec<_> = Path::new(&a).components().collect();
            let right: Vec<_> = Path::new(&b).components().collect();
            let mut common = 0;
            while common < left.len()
                && common < right.len()
                && if cfg!(windows) {
                    left[common]
                        .as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&right[common].as_os_str().to_string_lossy())
                } else {
                    left[common] == right[common]
                }
            {
                common += 1;
            }
            if common == 0 {
                json!(b)
            } else {
                let mut result = PathBuf::new();
                for _ in common..left.len() {
                    result.push("..");
                }
                for part in &right[common..] {
                    result.push(part.as_os_str());
                }
                json!(result.to_string_lossy().to_string())
            }
        }
        "path.dirname" => json!(dirname(strings[0])),
        "path.basename" => {
            let base = basename(strings[0]);
            json!(if strings.len() == 2 && !strings[1].is_empty() {
                base.strip_suffix(strings[1]).unwrap_or(base)
            } else {
                base
            })
        }
        "path.extname" => json!(extension(strings[0])),
        "url.parse" => url_parse(strings[0], strings.get(1).copied())?,
        _ => bail!("{name} is unavailable in the flow runtime"),
    };
    ensure!(
        serde_json::to_vec(&value)?.len() <= output_limit,
        "built-in output exceeds {} KiB",
        output_limit / 1024
    );
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_arguments_and_large_inputs_without_external_runtime() {
        assert!(
            call("path.resolve", vec![json!("relative")])
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        assert!(call("path.join", vec![json!(17)]).is_err());
        assert!(call("path.join", vec![json!("x".repeat(INPUT_LIMIT))]).is_err());
        assert!(call("url.parse", vec![json!("relative")]).is_err());
        // Node's atob rejects these too.
        for bad in ["a", "YQ===", "Y=Q=", "YQ=", "a*b="] {
            assert!(call("atob", vec![json!(bad)]).is_err(), "atob should reject {bad:?}");
        }
        assert!(call("btoa", vec![json!("中")]).is_err());
        for name in ["SUBAPI_API_KEY", "GITHUB_TOKEN", "aws_secret_access_key", "DB_PASSWORD"] {
            assert!(call("process.env", vec![json!(name)]).unwrap_err().to_string().contains("credential"), "{name}");
        }
        assert_eq!(call("process.env", vec![json!("GROW_SURELY_UNSET_VARIABLE")]).unwrap(), Value::Null);
    }

    #[cfg(unix)]
    #[test]
    fn common_operations_match_node_when_available() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut cases = Vec::new();
        for value in [
            "",
            ".",
            "..",
            ".foo",
            "..foo",
            "...",
            "foo.",
            "foo..",
            "/",
            "//",
            "//a",
            "/a//b/",
            "a/../",
            "a/./b",
            "中文/../文件",
            "a/b/../c",
        ] {
            for name in [
                "path.normalize",
                "path.dirname",
                "path.basename",
                "path.extname",
            ] {
                cases.push(json!({"name":name,"args":[value]}));
            }
        }
        for (name, args) in [
            ("path.join", json!(["a", "/b", "..", "c/"])),
            ("path.join", json!(["", ""])),
            ("path.resolve", json!(["/project", "a", "../b/"])),
            ("path.relative", json!(["/a/b", "/a/c"])),
            ("path.relative", json!(["/a", "/a"])),
            ("path.basename", json!(["/a/file.txt", ".txt"])),
            (
                "url.parse",
                json!(["../a%20b.css?x=1#top", "https://example.com/docs/pages/"]),
            ),
            ("url.parse", json!(["https://EXAMPLE.com:443/a?x=1#z"])),
            ("url.parse", json!(["file:///tmp/a"])),
            ("url.parse", json!(["https://[::1]:8080/a?x=%20#part"])),
            ("url.parse", json!(["https://example.com/中文?x=1#文"])),
            ("url.parse", json!(["https://example.com/a?"])),
            ("url.parse", json!(["https://example.com/a#"])),
            ("url.parse", json!(["mailto:user@example.com"])),
            ("atob", json!(["aGk="])),
            ("atob", json!([" aG\tk=\n"])),
            ("atob", json!(["YQ"])),
            ("atob", json!(["YR=="])),
            ("atob", json!(["/+8="])),
            ("atob", json!([""])),
            ("btoa", json!(["ab\u{ff}"])),
            ("btoa", json!([""])),
            ("os.platform", json!([])),
            ("os.homedir", json!([])),
            ("os.tmpdir", json!([])),
        ] {
            cases.push(json!({"name":name,"args":args}));
        }
        let source = r#"const p=require('node:path'),os=require('node:os');let b='';process.stdin.on('data',c=>b+=c);process.stdin.on('end',()=>{const v=JSON.parse(b);console.log(JSON.stringify(v.map(x=>{if(x.name==='url.parse'){const u=x.args.length===2?new URL(x.args[0],x.args[1]):new URL(x.args[0]);return {href:u.href,origin:u.origin,protocol:u.protocol,hostname:u.hostname,port:u.port,pathname:u.pathname,search:u.search,hash:u.hash}}if(x.name==='atob'||x.name==='btoa')return globalThis[x.name](...x.args);if(x.name.startsWith('os.'))return os[x.name.slice(3)]();return p[x.name.slice(5)](...x.args)})))})"#;
        let Ok(mut child) = Command::new("node")
            .arg("-e")
            .arg(source)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        else {
            return;
        };
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_string(&cases).unwrap().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        for (case, expected) in cases.iter().zip(expected) {
            let actual = call(
                case["name"].as_str().unwrap(),
                case["args"].as_array().unwrap().clone(),
            )
            .unwrap();
            assert_eq!(actual, expected, "case {case}");
        }
    }
}
