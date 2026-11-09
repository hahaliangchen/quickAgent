use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

type BuildResult<T> = Result<T, Box<dyn Error>>;

// Official upstream release checksums, pinned here rather than fetched alongside
// the executable. https://github.com/BurntSushi/ripgrep/releases/tag/15.1.0
const RG_VERSION: &str = "15.1.0";

fn release(target: &str) -> BuildResult<(&'static str, &'static str)> {
    Ok(match target {
        "x86_64-pc-windows-msvc" => ("x86_64-pc-windows-msvc.zip", "124510b94b6baa3380d051fdf4650eaa80a302c876d611e9dba0b2e18d87493a"),
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => ("x86_64-unknown-linux-musl.tar.gz", "1c9297be4a084eea7ecaedf93eb03d058d6faae29bbc57ecdaf5063921491599"),
        "aarch64-unknown-linux-gnu" => ("aarch64-unknown-linux-gnu.tar.gz", "2b661c6ef508e902f388e9098d9c4c5aca72c87b55922d94abdba830b4dc885e"),
        "x86_64-apple-darwin" => ("x86_64-apple-darwin.tar.gz", "64811cb24e77cac3057d6c40b63ac9becf9082eedd54ca411b475b755d334882"),
        "aarch64-apple-darwin" => ("aarch64-apple-darwin.tar.gz", "378e973289176ca0c6054054ee7f631a065874a352bf43f0fa60ef079b6ba715"),
        _ => return Err(format!("No verified ripgrep {RG_VERSION} archive configured for {target}; add an official release asset and its pinned SHA-256 to desktop/build.rs").into()),
    })
}

fn run(command: &mut Command, purpose: &str) -> BuildResult<()> {
    let status = command
        .status()
        .map_err(|error| format!("{purpose}: {error}"))?;
    if !status.success() {
        return Err(format!("{purpose}: {status}").into());
    }
    Ok(())
}

#[cfg(windows)]
fn powershell(script: &str) -> Command {
    let mut command = Command::new(
        PathBuf::from(env::var_os("SystemRoot").expect("SystemRoot is required"))
            .join("System32/WindowsPowerShell/v1.0/powershell.exe"),
    );
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    command
}

fn sha256(path: &Path) -> BuildResult<String> {
    #[cfg(windows)]
    let output = powershell("$ErrorActionPreference='Stop'; $stream=[IO.File]::OpenRead($env:GROW_RG_HASH_FILE); $sha=[Security.Cryptography.SHA256]::Create(); try { [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-','').ToLowerInvariant() } finally { $sha.Dispose(); $stream.Dispose() }")
        .env("GROW_RG_HASH_FILE", path).output()?;
    #[cfg(not(windows))]
    let output = if cfg!(target_os = "macos") {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()?
    } else {
        Command::new("sha256sum").arg(path).output()?
    };
    if !output.status.success() {
        return Err(format!(
            "Could not compute SHA-256 for bundled ripgrep: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let digest = String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid SHA-256 output while verifying ripgrep".into());
    }
    Ok(digest)
}

fn copy_if_changed(source: &Path, destination: &Path) -> BuildResult<()> {
    if fs::read(destination).ok() != Some(fs::read(source)?) {
        fs::copy(source, destination)?;
    }
    Ok(())
}

/// Runs in the existing prepare.mjs -> cargo tauri build pipeline, before
/// tauri-build validates/copies externalBin. Never executes an unverified rg.
fn prepare_rg(target: &str) -> BuildResult<()> {
    let (asset, expected) = release(target)?;
    let filename = format!("ripgrep-{RG_VERSION}-{asset}");
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is missing")?);
    let archive = if let Some(path) = env::var_os("GROW_RG_ARCHIVE") {
        // Offline builders may supply the official archive; the same pin applies.
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(
                "GROW_RG_ARCHIVE must be an absolute path to the official release archive".into(),
            );
        }
        println!("cargo:rerun-if-changed={}", path.display());
        path
    } else {
        let path = output.join(&filename);
        if !path.is_file() {
            let url = format!(
                "https://github.com/BurntSushi/ripgrep/releases/download/{RG_VERSION}/{filename}"
            );
            println!("cargo:warning=Preparing verified ripgrep {RG_VERSION} for {target}");
            #[cfg(windows)]
            run(powershell("$ErrorActionPreference='Stop'; [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -UseBasicParsing -Uri $env:GROW_RG_URL -OutFile $env:GROW_RG_DOWNLOAD")
                .env("GROW_RG_URL", &url).env("GROW_RG_DOWNLOAD", &path), "Download official ripgrep (or set GROW_RG_ARCHIVE for an offline build)")?;
            #[cfg(not(windows))]
            run(
                Command::new("curl")
                    .args([
                        "--fail",
                        "--location",
                        "--proto",
                        "=https",
                        "--proto-redir",
                        "=https",
                        "--tlsv1.2",
                        "--retry",
                        "2",
                        "--output",
                    ])
                    .arg(&path)
                    .arg(url),
                "Download official ripgrep: curl is required (or set GROW_RG_ARCHIVE)",
            )?;
        }
        path
    };
    let actual = sha256(&archive)?;
    if actual != expected {
        return Err(format!("ripgrep archive SHA-256 mismatch: {} (expected {expected}, got {actual}). Refusing to extract or bundle it; replace this archive with the official {filename}", archive.display()).into());
    }
    let unpacked = output.join("verified-ripgrep");
    fs::create_dir_all(&unpacked)?;
    if asset.ends_with(".zip") {
        #[cfg(windows)]
        run(powershell("$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.IO.Compression.FileSystem; $zip=[IO.Compression.ZipFile]::OpenRead($env:GROW_RG_ZIP); try { foreach ($entry in $zip.Entries) { if ($entry.Name -eq 'rg.exe' -or $entry.Name -eq 'COPYING' -or $entry.Name -eq 'LICENSE-MIT' -or $entry.Name -eq 'UNLICENSE') { $destination=[IO.Path]::Combine($env:GROW_RG_UNPACK,$entry.FullName); [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) > $null; [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,$destination,$true) } } } finally { $zip.Dispose() }")
            .env("GROW_RG_ZIP", &archive).env("GROW_RG_UNPACK", &unpacked), "Extract verified ripgrep archive")?;
        #[cfg(not(windows))]
        run(
            Command::new("unzip")
                .arg("-o")
                .arg(&archive)
                .arg("-d")
                .arg(&unpacked),
            "Extract verified ripgrep: unzip is required",
        )?;
    } else {
        run(
            Command::new("tar")
                .arg("-xzf")
                .arg(&archive)
                .arg("-C")
                .arg(&unpacked),
            "Extract verified ripgrep: tar is required",
        )?;
    }
    let folder = filename
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".zip");
    let source = unpacked.join(folder);
    let ext = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let binaries =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is missing")?)
            .join("binaries");
    fs::create_dir_all(&binaries)?;
    copy_if_changed(
        &source.join(format!("rg{ext}")),
        &binaries.join(format!("rg-{target}{ext}")),
    )?;
    // Unsuffixed development alias: PATH must resolve `rg`, while Tauri's
    // externalBin source uses the target suffix and installs it as rg[.exe].
    copy_if_changed(
        &source.join(format!("rg{ext}")),
        &binaries.join(format!("rg{ext}")),
    )?;
    copy_if_changed(&source.join("COPYING"), &binaries.join("ripgrep-COPYING"))?;
    copy_if_changed(
        &source.join("LICENSE-MIT"),
        &binaries.join("ripgrep-LICENSE-MIT"),
    )?;
    copy_if_changed(
        &source.join("UNLICENSE"),
        &binaries.join("ripgrep-UNLICENSE"),
    )?;
    Ok(())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GROW_RG_ARCHIVE");
    let target = env::var("TARGET").expect("TARGET is required");
    println!("cargo:rustc-env=GROW_BUILD_TARGET={target}");
    prepare_rg(&target)
        .unwrap_or_else(|error| panic!("Could not prepare bundled ripgrep: {error}"));
    tauri_build::build()
}
