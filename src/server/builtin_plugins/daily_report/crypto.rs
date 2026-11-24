//! RuoYi-Vue-Plus login envelope: the request body is AES-256-ECB/PKCS7 and the
//! AES key travels RSA-encrypted in `encrypt-key`, so a password never crosses
//! the wire in readable form. Mirrors `report_vue/src/utils/crypto.ts`.
use aes::{
    Aes256,
    cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rsa::{
    Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey, pkcs1::DecodeRsaPrivateKey,
    pkcs8::{DecodePrivateKey, DecodePublicKey},
};

/// AES block size; ECB encrypts each block on its own.
const BLOCK: usize = 16;
/// AES-256 key length in bytes, and the length of the string RuoYi generates.
const KEY_LEN: usize = 32;
/// The characters a generated key may use, as the browser build does.
const KEY_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// `VITE_APP_RSA_PUBLIC_KEY` of the team server; an account may override it.
pub(super) const DEFAULT_RSA_PUBLIC_KEY: &str =
    "MFwwDQYJKoZIhvcNAQEBBQADSwAwSAJBAKoR8mX0rGKLqzcWmOzbfj64K8ZIgOdHnzkXSOVOZbFu/TJhZ7rFAN+eaGkl3C4buccQd/EjEsj9ir7ijT7h96MCAwEAAQ==";

/// A fresh 32-character alphanumeric key, unused once the request is over.
pub(super) fn random_key() -> Result<String> {
    let mut raw = [0u8; KEY_LEN];
    getrandom::fill(&mut raw).context("the login envelope needs system randomness")?;
    Ok(raw
        .iter()
        .map(|byte| KEY_ALPHABET[*byte as usize % KEY_ALPHABET.len()] as char)
        .collect())
}

fn cipher(key: &str) -> Result<Aes256> {
    ensure!(
        key.len() == KEY_LEN,
        "an AES-256 login key is exactly 32 bytes"
    );
    Aes256::new_from_slice(key.as_bytes()).map_err(|_| anyhow::anyhow!("invalid AES-256 key"))
}

/// AES-256-ECB with PKCS7 padding, Base64 encoded: the body RuoYi decrypts.
pub(super) fn encrypt(text: &str, key: &str) -> Result<String> {
    let cipher = cipher(key)?;
    let mut buffer = text.as_bytes().to_vec();
    let pad = BLOCK - buffer.len() % BLOCK;
    buffer.extend(std::iter::repeat_n(pad as u8, pad));
    for chunk in buffer.chunks_mut(BLOCK) {
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    Ok(STANDARD.encode(&buffer))
}

/// Undo `encrypt`. The server pads as well, so both sides must agree.
pub(super) fn decrypt(encoded: &str, key: &str) -> Result<String> {
    let cipher = cipher(key)?;
    let mut buffer = STANDARD
        .decode(encoded.trim())
        .context("the encrypted body is not Base64")?;
    ensure!(
        !buffer.is_empty() && buffer.len().is_multiple_of(BLOCK),
        "the encrypted body is not a whole number of AES blocks"
    );
    for chunk in buffer.chunks_mut(BLOCK) {
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    let pad = buffer.last().map(|byte| *byte as usize).unwrap_or_default();
    ensure!(
        (1..=BLOCK).contains(&pad) && pad <= buffer.len(),
        "invalid PKCS7 padding"
    );
    ensure!(
        buffer[buffer.len() - pad..]
            .iter()
            .all(|byte| *byte as usize == pad),
        "invalid PKCS7 padding"
    );
    buffer.truncate(buffer.len() - pad);
    String::from_utf8(buffer).context("the decrypted body is not UTF-8")
}

fn public_key(encoded: &str) -> Result<RsaPublicKey> {
    let der = STANDARD
        .decode(encoded.trim())
        .context("the RSA public key is not Base64")?;
    RsaPublicKey::from_public_key_der(&der)
        .context("the RSA public key is not SubjectPublicKeyInfo")
}

/// RSA PKCS1v15 over `Base64(aes_key)`, matching RuoYi-Vue-Plus's
/// `encrypt(encryptBase64(aesKey))` and the Java backend's
/// `decryptByBase64(decryptByRsa(encryptKey, privateKey))`.
pub(super) fn wrap_key(public_key_base64: &str, key: &str) -> Result<String> {
    let public = public_key(public_key_base64)?;
    let key_b64 = STANDARD.encode(key.as_bytes());
    let mut rng = rand::rng();
    let wrapped = public
        .encrypt(&mut rng, Pkcs1v15Encrypt, key_b64.as_bytes())
        .context("the server RSA public key could not wrap the login key")?;
    Ok(STANDARD.encode(wrapped))
}

/// Recover a key the server wrapped for us; used when a response is encrypted.
pub(super) fn unwrap_key(private_key_pem: &str, wrapped_base64: &str) -> Result<String> {
    let private = RsaPrivateKey::from_pkcs8_pem(private_key_pem.trim())
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(private_key_pem.trim()))
        .context("the configured response private key is not a PEM RSA key")?;
    let wrapped = STANDARD
        .decode(wrapped_base64.trim())
        .context("encrypt-key is not Base64")?;
    let plain = private
        .decrypt(Pkcs1v15Encrypt, &wrapped)
        .context("encrypt-key could not be unwrapped")?;
    let key_b64 = String::from_utf8(plain).context("the unwrapped key is not UTF-8")?;
    let decoded = STANDARD
        .decode(key_b64.trim())
        .context("the unwrapped key is not Base64")?;
    String::from_utf8(decoded).context("the decoded AES key is not UTF-8")
}
