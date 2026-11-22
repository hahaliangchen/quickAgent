//! Register a local image file as a task-owned visual artifact.
//!
//! A screenshot written by a test, a script or a Playwright run already exists
//! on disk; routing it through `browser.open("file://...")` + `browser.screenshot`
//! only to give `vision.analyze_images` something to read costs steps and adds
//! the browser's own chrome to the pixels. This registers the file itself.
//!
//! Reads stay confined to the workspace root and the system temporary
//! directory, and only a real PNG/JPEG/WEBP/GIF header is accepted, so a session
//! cannot turn this into a reader for arbitrary local files.

use crate::server::agent_service::AgentServiceState;
use crate::media::visual_artifacts::{self, VisualContext};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Formats accepted from disk. The bytes decide, never the file name.
const FORMATS: [image::ImageFormat; 4] = [
    image::ImageFormat::Png,
    image::ImageFormat::Jpeg,
    image::ImageFormat::WebP,
    image::ImageFormat::Gif,
];


/// Every directory a registered image may come from, resolved so that `..` and
/// symbolic links are judged by where they really point.
fn allowed_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots = vec![workspace.to_path_buf()];
    for candidate in [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
    ] {
        if let Ok(resolved) = candidate.canonicalize()
            && !roots.contains(&resolved)
        {
            roots.push(resolved);
        }
    }
    roots
}

fn within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Where a request points on disk, refusing anything outside the allowed roots.
fn resolve(workspace: &Path, raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    ensure!(!raw.is_empty(), "path must not be empty");
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        workspace.join(raw)
    };
    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("image file does not exist: {raw}"))?;
    // Canonical first, then compare: a symlink inside the workspace that points
    // outside it must not become a read of that outside file.
    ensure!(
        within(&resolved, &allowed_roots(workspace)),
        "image path must stay inside the workspace or the system temp directory: {raw}"
    );
    ensure!(
        resolved.is_file(),
        "image path is not a regular file: {}",
        resolved.display()
    );
    Ok(resolved)
}

fn format_of(bytes: &[u8], resolved: &Path) -> Result<image::ImageFormat> {
    image::guess_format(bytes)
        .ok()
        .filter(|format| FORMATS.contains(format))
        .with_context(|| {
            format!(
                "not a supported image file (PNG/JPEG/WEBP/GIF by content): {}",
                resolved.display()
            )
        })
}

/// Register one local image file for this task.
///
/// @param state - the service state owning the workspace and the settings.
/// @param context - visual identity of this call; its task owns the artifact.
/// @param args - `{ path }`.
/// @returns the artifact id with its saved metadata, the resolved path and the
///   pixel size of both the source file and the registered copy.
pub async fn execute(
    state: &AgentServiceState,
    context: &VisualContext,
    args: &Value,
) -> Result<Value> {
    let raw = args["path"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .context("path is required")?;
    let workspace = state.workspace.root().to_path_buf();
    let request = raw.to_owned();
    let (resolved, bytes) = tokio::task::spawn_blocking(move || -> Result<(PathBuf, Vec<u8>)> {
        let resolved = resolve(&workspace, &request)?;
        let size = std::fs::metadata(&resolved)?.len();
        ensure!(
            size <= visual_artifacts::MAX_ORIGINAL_BYTES as u64,
            "image file exceeds the {} MiB original budget",
            visual_artifacts::MAX_ORIGINAL_BYTES / (1024 * 1024)
        );
        let bytes = std::fs::read(&resolved)?;
        Ok((resolved, bytes))
    })
    .await??;
    let format = format_of(&bytes, &resolved)?;
    let (png, normalization) = crate::media::attachments::vision_copy(&bytes)?;
    let artifact = visual_artifacts::save(
        state.workspace.root(),
        context,
        json!({
            "source": "vision.register_image",
            "requested_path": raw,
            "path": resolved.display().to_string(),
            "source_format": format.extensions_str().first().copied().unwrap_or(""),
            "source_byte_size": bytes.len(),
            "normalization": normalization,
        }),
        &png,
    )?;
    Ok(json!({
        "artifact_id": artifact["artifact_id"],
        "visual_artifact": artifact,
        "path": resolved.display().to_string(),
        "width": artifact["width"],
        "height": artifact["height"],
        "source_width": normalization["source_width"],
        "source_height": normalization["source_height"],
        "normalization": normalization,
        "next": "Call await vision.analyze_images({ artifact_ids: [artifact_id], question }) inside run_js to inspect it.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(width, height)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn sandbox(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("{prefix}-{}", crate::server::agent_service::uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_relative_path_resolves_below_the_workspace_root() {
        let workspace = sandbox("register-image-relative");
        std::fs::write(workspace.join("shot.png"), png(4, 3)).unwrap();
        let resolved = resolve(&workspace, "shot.png").unwrap();
        assert_eq!(resolved, workspace.join("shot.png").canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn an_absolute_temp_path_is_allowed_and_a_directory_is_refused() {
        let workspace = sandbox("register-image-absolute");
        let file = std::env::temp_dir()
            .join(format!("register-image-{}.png", crate::server::agent_service::uuid_like()));
        std::fs::write(&file, png(3, 2)).unwrap();
        assert!(resolve(&workspace, &file.display().to_string()).is_ok());
        assert!(
            resolve(&workspace, "..").is_err(),
            "the directory itself is not an image file"
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn a_resolved_path_outside_every_root_is_refused() {
        // The workspace and the temp dir are the only roots; a link that lands
        // in either one is fine, anything else is not.
        let roots = [PathBuf::from("/workspace"), PathBuf::from("/tmp")];
        assert!(within(Path::new("/workspace/src/shot.png"), &roots));
        assert!(within(Path::new("/tmp/shot.png"), &roots));
        assert!(!within(Path::new("/etc/shot.png"), &roots));
        assert!(!within(Path::new("/home/user/.ssh/shot.png"), &roots));
    }

    #[test]
    fn only_a_real_image_header_is_accepted() {
        let workspace = sandbox("register-image-format");
        assert_eq!(
            format_of(&png(2, 2), &workspace).unwrap(),
            image::ImageFormat::Png
        );
        let text = workspace.join("notes.png");
        std::fs::write(&text, b"this is not an image").unwrap();
        let error = format_of(&std::fs::read(&text).unwrap(), &text).unwrap_err();
        assert!(
            format!("{error}").contains("not a supported image file"),
            "{error}"
        );
        // A PDF is a real file but not a picture.
        assert!(format_of(b"%PDF-1.7\n", &workspace).is_err());
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
