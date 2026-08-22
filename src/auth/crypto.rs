//! Secret material: the at-rest encryption key, AEAD wrapping of refresh tokens,
//! random identifiers, and PKCE.
//!
//! Nothing in this module implements `Debug` or `Display` for key or token material.
//! That is deliberate — NFR-4 requires that no secret can reach a log line, and the
//! cheapest way to guarantee that is to make it unprintable.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use sha2::{Digest, Sha256};

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

/// The AEAD key used to wrap refresh tokens before they touch the database (D18/D19).
pub struct SecretKey {
    cipher: ChaCha20Poly1305,
}

impl SecretKey {
    /// Load the key from `path`, generating it on first use.
    ///
    /// Refuses to proceed if the file is readable by group or other. A key that anyone
    /// on the box can read provides no protection over storing the token in plaintext,
    /// so continuing would be security theatre rather than security.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading key file {}", path.display()))?;
            enforce_owner_only_permissions(path)?;
            let bytes: [u8; KEY_BYTES] = bytes.as_slice().try_into().map_err(|_| {
                anyhow!(
                    "key file {} is {} bytes, expected {}. Delete it to have mdshelf \
                     generate a fresh key (this invalidates all existing sessions).",
                    path.display(),
                    bytes.len(),
                    KEY_BYTES
                )
            })?;
            return Ok(Self::from_bytes(&bytes));
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating key directory {}", parent.display()))?;
        }
        let mut bytes = [0u8; KEY_BYTES];
        rand::fill(&mut bytes[..]);
        write_owner_only(path, &bytes)
            .with_context(|| format!("writing key file {}", path.display()))?;
        Ok(Self::from_bytes(&bytes))
    }

    fn from_bytes(bytes: &[u8; KEY_BYTES]) -> Self {
        let key = Key::from(*bytes);
        Self {
            cipher: ChaCha20Poly1305::new(&key),
        }
    }

    /// Encrypt `plaintext`, returning `nonce || ciphertext`.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_BYTES];
        rand::fill(&mut nonce_bytes[..]);
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| anyhow!("AEAD encryption failed"))?;
        let mut out = Vec::with_capacity(NONCE_BYTES + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt a `nonce || ciphertext` blob produced by [`SecretKey::encrypt`].
    ///
    /// The error is intentionally opaque: a caller that fails to decrypt a stored
    /// refresh token invalidates that session (US-6) rather than reporting detail that
    /// would describe the stored bytes.
    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() <= NONCE_BYTES {
            bail!("ciphertext is too short to contain a nonce");
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_BYTES);
        let nonce_array: [u8; NONCE_BYTES] = nonce_bytes
            .try_into()
            .map_err(|_| anyhow!("ciphertext has a malformed nonce"))?;
        let nonce = Nonce::from(nonce_array);
        self.cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| anyhow!("AEAD decryption failed"))
    }
}

/// Default key location, `~/.mdshelf/secret.key` (D19).
pub fn default_key_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set; pass [auth] key_file explicitly"))?;
    Ok(home.join(".mdshelf/secret.key"))
}

#[cfg(unix)]
fn enforce_owner_only_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .with_context(|| format!("reading permissions of {}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        bail!(
            "key file {} has mode {:o}; it must not be readable by group or other.\n  \
             Fix with: chmod 600 {}",
            path.display(),
            mode & 0o777,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_owner_only_permissions(_path: &Path) -> Result<()> {
    // Windows ACLs are not mode bits; the file inherits the user profile's protection.
    Ok(())
}

#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

/// A cryptographically random URL-safe identifier, used for session ids, OAuth `state`,
/// `nonce`, and PKCE verifiers.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    URL_SAFE_NO_PAD.encode(&buf)
}

/// The S256 PKCE challenge for a verifier: `base64url(sha256(verifier))`.
pub fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Stable short digest used to key per-viewer caches by ACL signature (D12).
pub fn signature_digest(parts: &[String]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SecretKey {
        SecretKey::from_bytes(&[7u8; KEY_BYTES])
    }

    #[test]
    fn round_trips_ciphertext() {
        let key = test_key();
        let blob = key.encrypt(b"refresh-token-value").unwrap();
        assert_eq!(key.decrypt(&blob).unwrap(), b"refresh-token-value");
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let key = test_key();
        let blob = key.encrypt(b"refresh-token-value").unwrap();
        assert!(
            !blob
                .windows(b"refresh-token-value".len())
                .any(|w| w == b"refresh-token-value")
        );
    }

    #[test]
    fn rejects_tampered_ciphertext() {
        let key = test_key();
        let mut blob = key.encrypt(b"refresh-token-value").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(key.decrypt(&blob).is_err());
    }

    #[test]
    fn rejects_truncated_ciphertext() {
        let key = test_key();
        assert!(key.decrypt(&[0u8; 4]).is_err());
    }

    #[test]
    fn nonce_differs_between_encryptions() {
        let key = test_key();
        let a = key.encrypt(b"same").unwrap();
        let b = key.encrypt(b"same").unwrap();
        assert_ne!(a, b, "reusing a nonce with the same key would be fatal");
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn random_tokens_are_unique() {
        assert_ne!(random_token(32), random_token(32));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_world_readable_key_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("mdshelf-key-{}", random_token(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret.key");
        std::fs::write(&path, [0u8; KEY_BYTES]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        // `SecretKey` intentionally has no `Debug`, so `unwrap_err` is unavailable here.
        let err = match SecretKey::load_or_create(&path) {
            Ok(_) => panic!("a group-readable key file must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(err.contains("must not be readable"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn generates_key_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("mdshelf-key-{}", random_token(8)));
        let path = dir.join("secret.key");

        SecretKey::load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "generated key must be 0600");

        // A second load must reuse the same key rather than regenerating it.
        let first = std::fs::read(&path).unwrap();
        SecretKey::load_or_create(&path).unwrap();
        assert_eq!(first, std::fs::read(&path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
