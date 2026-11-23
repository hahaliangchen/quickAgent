use std::{
    fs::File,
    io,
    path::{Component, Path},
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode, header},
    response::Response,
};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

/// Walk descriptor-relative components, rejecting links at every step.
pub(crate) fn open_regular(root: &Path, relative: &Path) -> io::Result<File> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "not a regular workspace file");
    let mut parts = relative.components().peekable();
    if matches!(parts.peek(), Some(Component::CurDir)) {
        parts.next();
    }
    let mut dir = Dir::open_ambient_dir(root, ambient_authority())?;
    while let Some(Component::Normal(name)) = parts.next() {
        if parts.peek().is_some() {
            dir = dir.open_dir_nofollow(name)?;
            continue;
        }
        if !dir.symlink_metadata(name)?.is_file() {
            return Err(invalid());
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = dir.open_with(name, &options)?;
        if !file.metadata()?.is_file() {
            return Err(invalid());
        }
        return Ok(file.into_std());
    }
    Err(invalid())
}

/// Resolve absolute and file-URI paths lexically before opening. Canonicalizing
/// the requested file would erase the symlinks that the walk must reject.
pub(super) fn workspace_relative(root: &Path, input: &str) -> Option<std::path::PathBuf> {
    let path = if input.starts_with("file://") {
        // Parse only the authority, not a URL path: URL normalization removes
        // dot segments before we can reject them.
        let rest = input.strip_prefix("file://")?;
        let path = if rest.starts_with('/') {
            rest
        } else {
            rest.strip_prefix("localhost")?
                .strip_prefix('/')
                .map(|_| &rest["localhost".len()..])?
        };
        let mut bytes = Vec::with_capacity(path.len());
        let mut input = path.bytes();
        while let Some(byte) = input.next() {
            bytes.push(if byte == b'%' {
                let hi = (input.next()? as char).to_digit(16)?;
                let lo = (input.next()? as char).to_digit(16)?;
                (hi * 16 + lo) as u8
            } else {
                byte
            });
        }
        let path = String::from_utf8(bytes).ok()?;
        #[cfg(windows)]
        let path = if path.as_bytes().get(2) == Some(&b':') {
            path[1..].to_owned()
        } else {
            path
        };
        std::path::PathBuf::from(path)
    } else {
        input.into()
    };
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let relative = if path.is_absolute() {
        path.strip_prefix(root).ok()?
    } else {
        &path
    };
    // This directory is host state, not a conversation artifact.
    if relative.components().any(
        |c| matches!(c, Component::Normal(n) if n.eq_ignore_ascii_case(".codex-workspace-mcp")),
    ) {
        return None;
    }
    Some(relative.to_path_buf())
}

fn byte_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let (unit, value) = value.split_once('=')?;
    if !unit.eq_ignore_ascii_case("bytes") || total == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    let number = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    if start.is_empty() {
        let suffix = number(end)?;
        return (suffix > 0).then(|| (total.saturating_sub(suffix), total - 1));
    }
    let start = number(start)?;
    let end = if end.is_empty() {
        total - 1
    } else {
        number(end)?.min(total - 1)
    };
    (start <= end).then_some((start, end))
}

pub(crate) async fn respond(
    file: File,
    media: &str,
    inline: bool,
    method: Method,
    request: &HeaderMap,
) -> io::Result<Response> {
    let metadata = file.metadata()?;
    let total = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|t| t.as_nanos())
        .unwrap_or(0);
    let etag = format!("\"{total:x}-{modified:x}\"");
    let mut builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_TYPE, media)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'",
        )
        .header(
            header::CONTENT_DISPOSITION,
            if inline { "inline" } else { "attachment" },
        );
    let mut length = total;
    let mut start = 0;
    if let Some(range) = request.get(header::RANGE).filter(|_| {
        request
            .get(header::IF_RANGE)
            .is_none_or(|value| value.as_bytes() == etag.as_bytes())
    }) {
        let single = request.get_all(header::RANGE).iter().count() == 1;
        let parsed = if single {
            range
                .to_str()
                .ok()
                .and_then(|value| byte_range(value, total))
        } else {
            None
        };
        let Some((first, last)) = parsed else {
            return Ok(builder
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .header(header::CONTENT_LENGTH, 0)
                .body(Body::empty())
                .unwrap());
        };
        start = first;
        length = last - first + 1;
        builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {first}-{last}/{total}"),
        );
    }
    let body = if method == Method::HEAD || length == 0 {
        Body::empty()
    } else {
        let mut file = tokio::fs::File::from_std(file);
        if start > 0 {
            file.seek(io::SeekFrom::Start(start)).await?;
        }
        Body::from_stream(ReaderStream::new(file.take(length)))
    };
    Ok(builder
        .header(header::CONTENT_LENGTH, length)
        .body(body)
        .unwrap())
}
