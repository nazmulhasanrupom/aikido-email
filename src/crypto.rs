//! AES-256-GCM encryption for secrets.
//! Every encrypt uses a new random 12-byte nonce. The AAD (extra data bound to the
//! ciphertext) is the account id, so a secret copied to another row fails to decrypt.
use crate::config::Config;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use zeroize::Zeroizing;

/// Version of the master key. Stored with every secret so the key can be rotated later.
pub const KEY_VERSION: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("AIKIDO_ENCRYPTION_KEY is not valid base64")]
    BadKeyEncoding,
    #[error("the encryption key must be 32 bytes. Make one with: openssl rand -base64 32")]
    BadKeyLength,
    #[error("cannot read or write the key file: {0}")]
    KeyFile(String),
    #[error("encrypt failed")]
    Encrypt,
    #[error("cannot decrypt (wrong key, wrong account, or damaged data)")]
    Decrypt,
    #[error("unknown key version {0}")]
    UnknownKeyVersion(i32),
}

pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_version: i32,
}

pub struct Crypto {
    cipher: Aes256Gcm,
}

impl Crypto {
    pub fn from_key_bytes(key: &[u8]) -> Result<Self, CryptoError> {
        if key.len() != 32 {
            return Err(CryptoError::BadKeyLength);
        }
        Ok(Self { cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key)) })
    }

    fn from_base64(text: &str) -> Result<Self, CryptoError> {
        let raw = Zeroizing::new(B64.decode(text.trim()).map_err(|_| CryptoError::BadKeyEncoding)?);
        Self::from_key_bytes(&raw)
    }

    /// Key from `AIKIDO_ENCRYPTION_KEY`; else from `<data_dir>/encryption.key`;
    /// else make a new random key and save it there (mode 600).
    pub fn load_or_create(cfg: &Config) -> Result<Self, CryptoError> {
        if let Some(k) = &cfg.encryption_key {
            return Self::from_base64(k);
        }
        let path = cfg.data_dir.join("encryption.key");
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Self::from_base64(&text);
        }
        let io = |e: std::io::Error| CryptoError::KeyFile(e.to_string());
        std::fs::create_dir_all(&cfg.data_dir).map_err(io)?;
        let key = Zeroizing::new(Aes256Gcm::generate_key(&mut OsRng).to_vec());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(io)?;
        file.write_all(B64.encode(&*key).as_bytes()).map_err(io)?;
        eprintln!(
            "\n!!! NEW ENCRYPTION KEY MADE: {}\n!!! Back up this file. If you lose it, every connected account must be connected again.\n",
            path.display()
        );
        Self::from_key_bytes(&key)
    }

    pub fn encrypt(&self, plain: &[u8], aad: &[u8]) -> Result<Sealed, CryptoError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, Payload { msg: plain, aad })
            .map_err(|_| CryptoError::Encrypt)?;
        Ok(Sealed { ciphertext, nonce: nonce.to_vec(), key_version: KEY_VERSION })
    }

    pub fn decrypt(
        &self,
        ciphertext: &[u8],
        nonce: &[u8],
        key_version: i32,
        aad: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        if key_version != KEY_VERSION {
            return Err(CryptoError::UnknownKeyVersion(key_version));
        }
        if nonce.len() != 12 {
            return Err(CryptoError::Decrypt);
        }
        self.cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ciphertext, aad })
            .map(Zeroizing::new)
            .map_err(|_| CryptoError::Decrypt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto() -> Crypto {
        Crypto::from_key_bytes(&[7u8; 32]).unwrap()
    }

    #[test]
    fn round_trip() {
        let c = crypto();
        let sealed = c.encrypt(b"secret", b"account-1").unwrap();
        assert_ne!(sealed.ciphertext, b"secret");
        let plain = c.decrypt(&sealed.ciphertext, &sealed.nonce, sealed.key_version, b"account-1").unwrap();
        assert_eq!(&*plain, b"secret");
    }

    #[test]
    fn wrong_aad_fails() {
        let c = crypto();
        let sealed = c.encrypt(b"secret", b"account-1").unwrap();
        assert!(c.decrypt(&sealed.ciphertext, &sealed.nonce, sealed.key_version, b"account-2").is_err());
    }

    #[test]
    fn tampered_data_fails() {
        let c = crypto();
        let mut sealed = c.encrypt(b"secret", b"a").unwrap();
        sealed.ciphertext[0] ^= 1;
        assert!(c.decrypt(&sealed.ciphertext, &sealed.nonce, sealed.key_version, b"a").is_err());
    }

    #[test]
    fn nonce_is_new_each_time() {
        let c = crypto();
        let a = c.encrypt(b"x", b"a").unwrap();
        let b = c.encrypt(b"x", b"a").unwrap();
        assert_ne!(a.nonce, b.nonce);
    }

    #[test]
    fn bad_key_length() {
        assert!(Crypto::from_key_bytes(&[1u8; 16]).is_err());
    }
}
