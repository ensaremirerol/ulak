//! Encryption of stored GitHub tokens. One 32-byte key in `$ULAK_DIR/secrets.key` (0600).
//! If the key is lost the stored tokens are unusable and must be re-added.
use crate::CredentialError;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305,
};
use std::{io::Write, path::Path};
use ulak_proto::{CredentialId, GitToken};

pub struct Secrets(ChaCha20Poly1305);

fn key_err(path: &Path, m: impl std::fmt::Display) -> CredentialError {
    CredentialError::Key(format!("{}: {m}", path.display()))
}

impl Secrets {
    fn from_bytes(k: &[u8]) -> Result<Self, CredentialError> {
        ChaCha20Poly1305::new_from_slice(k)
            .map(Self)
            .map_err(|_| CredentialError::Key("key must be 32 bytes".into()))
    }

    /// A random key that lives only in memory (tests).
    pub fn ephemeral() -> Self {
        let mut k = [0u8; 32];
        rand::fill(&mut k[..]);
        Self::from_bytes(&k).expect("32 bytes")
    }

    /// Loads the key, creating it (0600, `create_new`) when missing. Refuses a symlink, a
    /// non-regular file, a mode with any group/other bit, or a wrong length.
    pub fn load_or_create(path: &Path) -> Result<Self, CredentialError> {
        #[cfg(unix)]
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let mut k = [0u8; 32];
        rand::fill(&mut k[..]);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        o.mode(0o600);
        match o.open(path) {
            Ok(mut f) => {
                f.write_all(&k)
                    .and_then(|()| f.sync_all())
                    .map_err(|e| key_err(path, e))?;
                return Self::from_bytes(&k);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(key_err(path, e)),
        }
        let m = std::fs::symlink_metadata(path).map_err(|e| key_err(path, e))?;
        if !m.file_type().is_file() {
            return Err(key_err(path, "not a regular file (symlinks are refused)"));
        }
        #[cfg(unix)]
        if m.mode() & 0o077 != 0 {
            return Err(key_err(
                path,
                format!("mode {:o} is too open; run chmod 600", m.mode() & 0o777),
            ));
        }
        Self::from_bytes(&std::fs::read(path).map_err(|e| key_err(path, e))?)
    }

    /// `(nonce, ciphertext)`; the credential id is bound as AAD so rows cannot be swapped.
    pub fn seal(
        &self,
        id: &CredentialId,
        token: &GitToken,
    ) -> Result<(Vec<u8>, Vec<u8>), CredentialError> {
        let mut nonce = [0u8; 12];
        rand::fill(&mut nonce[..]);
        let ct = self
            .0
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: token.expose().as_bytes(),
                    aad: id.as_str().as_bytes(),
                },
            )
            .map_err(|_| CredentialError::Decrypt)?;
        Ok((nonce.to_vec(), ct))
    }

    pub fn open(
        &self,
        id: &CredentialId,
        nonce: &[u8],
        ct: &[u8],
    ) -> Result<GitToken, CredentialError> {
        let nonce: [u8; 12] = nonce.try_into().map_err(|_| CredentialError::Decrypt)?;
        let plain = self
            .0
            .decrypt(
                (&nonce).into(),
                Payload {
                    msg: ct,
                    aad: id.as_str().as_bytes(),
                },
            )
            .map_err(|_| CredentialError::Decrypt)?;
        let s = String::from_utf8(plain).map_err(|_| CredentialError::Decrypt)?;
        GitToken::new(&s).map_err(|_| CredentialError::Decrypt)
    }
}
