//! Password rules, Argon2id hashing and checking.
//! The hash runs in `spawn_blocking`, and a semaphore keeps at most 4 hashes in flight,
//! so a flood of logins cannot eat all the memory (each hash uses 64 MiB).
use crate::crypto::Crypto;
use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::rngs::OsRng;
use std::sync::{Arc, OnceLock};
use tokio::sync::Semaphore;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

pub const MIN_LEN: usize = 12;
pub const MAX_LEN: usize = 128;
const MEMORY_KIB: u32 = 64 * 1024;
const ITERATIONS: u32 = 3;
const PARALLELISM: u32 = 1;
const OUTPUT_LEN: usize = 32;
const MAX_PARALLEL: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub enum PolicyError {
    TooShort,
    TooLong,
    Weak,
    ContainsPersonalInfo,
}

impl PolicyError {
    pub fn code(&self) -> &'static str {
        match self {
            PolicyError::TooShort => "password_too_short",
            PolicyError::TooLong => "password_too_long",
            PolicyError::Weak => "password_too_weak",
            PolicyError::ContainsPersonalInfo => "password_has_personal_info",
        }
    }
    pub fn message(&self) -> String {
        match self {
            PolicyError::TooShort => format!("Use at least {MIN_LEN} characters."),
            PolicyError::TooLong => format!("Use at most {MAX_LEN} characters."),
            PolicyError::Weak => "This password is too easy to guess. Try a longer phrase of unrelated words.".into(),
            PolicyError::ContainsPersonalInfo => "Do not put your name or email in the password.".into(),
        }
    }
}

/// Unicode NFKC, so the same visible password always gives the same bytes.
pub fn normalize(password: &str) -> Zeroizing<String> {
    Zeroizing::new(password.nfkc().collect())
}

/// Length 12 to 128, not weak (zxcvbn score 3 or more), and no name or email inside.
/// No "needs a symbol" rules.
pub fn check_policy(password: &str, email: &str, name: &str) -> Result<(), PolicyError> {
    let pw = normalize(password);
    let len = pw.chars().count();
    if len < MIN_LEN {
        return Err(PolicyError::TooShort);
    }
    if len > MAX_LEN {
        return Err(PolicyError::TooLong);
    }
    let lower = pw.to_lowercase();
    let email = email.trim().to_lowercase();
    let local = email.split('@').next().unwrap_or("");
    let mut personal: Vec<String> = Vec::new();
    if email.len() >= 3 {
        personal.push(email.clone());
    }
    if local.chars().count() >= 3 {
        personal.push(local.to_string());
    }
    let name = name.trim().to_lowercase();
    if name.chars().count() >= 3 {
        personal.push(name.clone());
    }
    personal.extend(name.split_whitespace().filter(|p| p.chars().count() >= 4).map(str::to_string));
    if personal.iter().any(|p| lower.contains(p.as_str())) {
        return Err(PolicyError::ContainsPersonalInfo);
    }
    let inputs: Vec<&str> = personal.iter().map(String::as_str).collect();
    let score = zxcvbn::zxcvbn(&pw, &inputs).score();
    if (score as u8) < 3 {
        return Err(PolicyError::Weak);
    }
    Ok(())
}

/// Hashes and checks passwords. One per process, inside `AppState`.
pub struct Passwords {
    pepper: Zeroizing<[u8; 32]>,
    gate: Semaphore,
    /// A hash of a random password. Checked when the email is unknown, so the time is the same.
    /// Made on first use (a hash takes a moment).
    dummy: OnceLock<String>,
}

impl Passwords {
    pub fn new(crypto: &Crypto) -> Arc<Self> {
        let pepper = crypto.derive_key("aikido-password-pepper-v1");
        Arc::new(Self { pepper, gate: Semaphore::new(MAX_PARALLEL), dummy: OnceLock::new() })
    }

    fn dummy_hash(&self) -> &str {
        self.dummy.get_or_init(|| {
            let mut random = [0u8; 24];
            rand::RngCore::fill_bytes(&mut OsRng, &mut random);
            self.hash_blocking(&hex(&random)).expect("can hash a random password")
        })
    }

    fn argon(&self) -> Result<Argon2<'_>, argon2::Error> {
        let params = Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, Some(OUTPUT_LEN))?;
        Argon2::new_with_secret(&*self.pepper, Algorithm::Argon2id, Version::V0x13, params)
    }

    fn hash_blocking(&self, password: &str) -> anyhow::Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let pw = normalize(password);
        let hash = self
            .argon()
            .map_err(|e| anyhow::anyhow!("argon2 setup: {e}"))?
            .hash_password(pw.as_bytes(), &salt)
            .map_err(|e| anyhow::anyhow!("argon2 hash: {e}"))?;
        Ok(hash.to_string())
    }

    fn verify_blocking(&self, password: &str, phc: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(phc) else { return false };
        let Ok(argon) = self.argon() else { return false };
        let pw = normalize(password);
        // `verify_password` compares in constant time.
        argon.verify_password(pw.as_bytes(), &parsed).is_ok()
    }

    /// True when the stored hash was made with older parameters than the current ones.
    pub fn needs_rehash(phc: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(phc) else { return true };
        let Ok(params) = Params::try_from(&parsed) else { return true };
        params.m_cost() != MEMORY_KIB
            || params.t_cost() != ITERATIONS
            || params.p_cost() != PARALLELISM
            || parsed.algorithm.as_str() != "argon2id"
    }

    pub async fn hash(self: &Arc<Self>, password: &str) -> anyhow::Result<String> {
        let _permit = self.gate.acquire().await?;
        let me = self.clone();
        let password = Zeroizing::new(password.to_string());
        tokio::task::spawn_blocking(move || me.hash_blocking(&password)).await?
    }

    /// `phc = None` (unknown user) checks against the dummy hash and always returns false.
    pub async fn verify(self: &Arc<Self>, password: &str, phc: Option<&str>) -> bool {
        let Ok(_permit) = self.gate.acquire().await else { return false };
        let me = self.clone();
        let password = Zeroizing::new(password.to_string());
        let real = phc.is_some();
        let hash = phc.map(str::to_string);
        let ok = tokio::task::spawn_blocking(move || {
            let hash = hash.as_deref().unwrap_or_else(|| me.dummy_hash());
            me.verify_blocking(&password, hash)
        })
        .await
        .unwrap_or(false);
        ok && real
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_length_and_strength() {
        assert_eq!(check_policy("short", "a@b.com", "A"), Err(PolicyError::TooShort));
        assert_eq!(check_policy(&"x".repeat(129), "a@b.com", "A"), Err(PolicyError::TooLong));
        assert_eq!(check_policy("password1234", "a@b.com", "A"), Err(PolicyError::Weak));
        assert_eq!(check_policy("aaaaaaaaaaaaaaaa", "a@b.com", "A"), Err(PolicyError::Weak));
        assert!(check_policy("correct horse battery staple", "a@b.com", "A").is_ok());
    }

    #[test]
    fn policy_refuses_name_and_email() {
        assert_eq!(
            check_policy("nazmul-hasan-is-here-2026", "naz@example.com", "Nazmul Hasan"),
            Err(PolicyError::ContainsPersonalInfo)
        );
        assert_eq!(
            check_policy("my-naz@example.com-pass!", "naz@example.com", "Someone Else"),
            Err(PolicyError::ContainsPersonalInfo)
        );
    }

    #[test]
    fn nfkc_makes_look_alike_passwords_equal() {
        // the full-width letter A and the plain letter A
        assert_eq!(&*normalize("\u{FF21}bc"), "Abc");
    }

    #[tokio::test]
    async fn hash_and_verify() {
        let crypto = Crypto::from_key_bytes(&[1u8; 32]).unwrap();
        let pw = Passwords::new(&crypto);
        let h = pw.hash("correct horse battery staple").await.unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(!Passwords::needs_rehash(&h));
        assert!(pw.verify("correct horse battery staple", Some(&h)).await);
        assert!(!pw.verify("correct horse battery stapl", Some(&h)).await);
        assert!(!pw.verify("anything", None).await);
        // another master key: another pepper, so the same hash does not verify
        let other = Passwords::new(&Crypto::from_key_bytes(&[2u8; 32]).unwrap());
        assert!(!other.verify("correct horse battery staple", Some(&h)).await);
    }

    #[test]
    fn old_parameters_need_a_rehash() {
        let weak = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(19 * 1024, 2, 1, Some(32)).unwrap());
        let salt = SaltString::generate(&mut OsRng);
        let h = weak.hash_password(b"x", &salt).unwrap().to_string();
        assert!(Passwords::needs_rehash(&h));
    }
}
