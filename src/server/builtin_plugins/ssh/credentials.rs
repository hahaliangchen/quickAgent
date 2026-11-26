use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

// Deliberately not Debug or Serialize. Only the local auth layer should expose it.
pub(super) struct Secret(String);

impl Secret {
    pub(super) fn new(value: String) -> Result<Self> {
        ensure!(!value.is_empty(), "SSH credential must not be empty");
        Ok(Self(value))
    }

    pub(super) fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Password,
    Passphrase,
}

#[derive(Deserialize, Serialize)]
struct Entry {
    kind: Kind,
    value: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(transparent)]
pub(super) struct Credentials(BTreeMap<String, Entry>);

impl Credentials {
    pub(super) fn get(&self, id: &str, kind: Kind) -> Option<Secret> {
        self.0
            .get(id)
            .filter(|entry| entry.kind == kind)
            .map(|entry| Secret(entry.value.clone()))
    }

    pub(super) fn has(&self, id: &str, kind: Kind) -> bool {
        self.0.get(id).is_some_and(|entry| entry.kind == kind)
    }

    pub(super) fn set(&mut self, id: &str, kind: Kind, value: Secret) {
        self.0.insert(
            id.to_owned(),
            Entry {
                kind,
                value: value.0,
            },
        );
    }

    pub(super) fn remove(&mut self, id: &str) -> bool {
        self.0.remove(id).is_some()
    }

    #[cfg(unix)]
    pub(super) fn read(path: &Path) -> Result<Self> {
        use std::{
            fs::OpenOptions,
            io::{ErrorKind, Read},
            os::unix::fs::{MetadataExt, OpenOptionsExt},
        };

        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err.into()),
        };
        let metadata = file.metadata()?;
        // SAFETY: geteuid takes no arguments and does not dereference memory.
        let uid = unsafe { libc::geteuid() };
        ensure!(
            metadata.is_file()
                && metadata.uid() == uid
                && metadata.nlink() == 1
                && metadata.mode() & 0o7777 == 0o600,
            "SSH credential file must be a regular file owned by the current user with mode 0600"
        );
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "SSH credential file is too large"
        );
        // JSON parser errors may quote input and must not cross this boundary.
        let result: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid SSH credential file"))?;
        ensure!(
            result.0.values().all(|entry| !entry.value.is_empty()),
            "invalid SSH credential file"
        );
        Ok(result)
    }

    #[cfg(unix)]
    pub(super) fn write(&self, path: &Path) -> Result<()> {
        use std::{fs::Permissions, io::Write, os::unix::fs::PermissionsExt};

        let bytes = serde_json::to_vec(self)
            .map_err(|_| anyhow::anyhow!("cannot encode SSH credentials"))?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "SSH credential file is too large"
        );
        let parent = path
            .parent()
            .context("SSH credential file has no parent directory")?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.as_file()
            .set_permissions(Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|err| err.error)?;
        Ok(())
    }

    #[cfg(windows)]
    pub(super) fn read(path: &Path) -> Result<Self> {
        use std::{
            fs::OpenOptions,
            io::{ErrorKind, Read},
            os::windows::fs::OpenOptionsExt,
        };
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err.into()),
        };
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "SSH credential file must be a regular non-symlink file"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let file_meta = file.metadata()?;
        ensure!(
            file_meta.is_file() && !file_meta.file_type().is_symlink(),
            "SSH credential file must be a regular non-symlink file"
        );
        let mut raw = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut raw)?;
        ensure!(
            raw.len() as u64 <= MAX_BYTES,
            "SSH credential file is too large"
        );
        let Some(cipher) = raw.strip_prefix(DPAPI_MAGIC) else {
            anyhow::bail!("invalid SSH credential file");
        };
        let mut plaintext = dpapi_unprotect(cipher)
            .map_err(|_| anyhow::anyhow!("invalid SSH credential file"))?;
        let parsed: Result<Self, _> = serde_json::from_slice(&plaintext);
        plaintext.fill(0);
        let result = parsed.map_err(|_| anyhow::anyhow!("invalid SSH credential file"))?;
        ensure!(
            result.0.values().all(|entry| !entry.value.is_empty()),
            "invalid SSH credential file"
        );
        Ok(result)
    }

    #[cfg(windows)]
    pub(super) fn write(&self, path: &Path) -> Result<()> {
        use std::io::Write;

        if let Ok(meta) = std::fs::symlink_metadata(path) {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "SSH credential file must be a regular non-symlink file"
            );
        }
        let mut bytes = serde_json::to_vec(self)
            .map_err(|_| anyhow::anyhow!("cannot encode SSH credentials"))?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "SSH credential file is too large"
        );
        let cipher = dpapi_protect(&bytes);
        bytes.fill(0);
        let cipher = cipher?;
        let parent = path
            .parent()
            .context("SSH credential file has no parent directory")?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(DPAPI_MAGIC)?;
        file.write_all(&cipher)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|err| err.error)?;
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    pub(super) fn read(path: &Path) -> Result<Self> {
        match std::fs::symlink_metadata(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
            Ok(_) => {
                anyhow::bail!("private SSH credential storage is not available on this platform")
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    pub(super) fn write(&self, _path: &Path) -> Result<()> {
        anyhow::bail!("private SSH credential storage is not available on this platform")
    }
}

#[cfg(any(unix, windows))]
use anyhow::Context;
#[cfg(any(unix, windows))]
const MAX_BYTES: u64 = 4 * 1024 * 1024;
#[cfg(windows)]
const DPAPI_MAGIC: &[u8] = b"QA_SSH_DPAPI_V1\n";

#[cfg(windows)]
fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData},
    };

    let data_in = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(plaintext.len()).context("SSH credentials too large")?,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut data_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `data_in` points to valid immutable bytes for the duration of the call;
    // `CryptProtectData` allocates `data_out.pbData` with LocalAlloc, which we free below.
    let ok = unsafe {
        CryptProtectData(
            &data_in,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut data_out,
        )
    };
    ensure!(ok != 0 && !data_out.pbData.is_null(), "DPAPI encryption failed");
    // SAFETY: `data_out.pbData` is valid for `data_out.cbData` bytes until `LocalFree`.
    let out = unsafe {
        let slice = std::slice::from_raw_parts(data_out.pbData, data_out.cbData as usize);
        let vec = slice.to_vec();
        LocalFree(data_out.pbData as _);
        vec
    };
    Ok(out)
}

#[cfg(windows)]
fn dpapi_unprotect(ciphertext: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };

    ensure!(!ciphertext.is_empty(), "empty ciphertext");
    let data_in = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(ciphertext.len()).context("SSH credentials too large")?,
        pbData: ciphertext.as_ptr() as *mut u8,
    };
    let mut data_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `data_in` points to valid immutable bytes; output buffer is freed via `LocalFree`.
    let ok = unsafe {
        CryptUnprotectData(
            &data_in,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut data_out,
        )
    };
    ensure!(ok != 0 && !data_out.pbData.is_null(), "DPAPI decryption failed");
    // SAFETY: `data_out.pbData` is valid for `data_out.cbData` bytes until `LocalFree`.
    let out = unsafe {
        let slice = std::slice::from_raw_parts_mut(data_out.pbData, data_out.cbData as usize);
        let vec = slice.to_vec();
        slice.fill(0);
        LocalFree(data_out.pbData as _);
        vec
    };
    Ok(out)
}
