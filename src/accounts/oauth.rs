//! The `state` value of the web OAuth flow. It is sealed (AES-256-GCM), so it is signed and
//! also hidden: it holds the PKCE verifier, which must not be readable in the browser URL.
//! No table is needed.
use super::model::Provider;
use crate::crypto::Crypto;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use chrono::Utc;
use serde::{Deserialize, Serialize};

const AAD: &[u8] = b"aikido-oauth-state-v1";
const MAX_AGE_SECS: i64 = 600;

#[derive(Serialize, Deserialize)]
pub struct StatePayload {
    pub provider: Provider,
    pub verifier: String,
    pub expires_at: i64,
}

pub fn seal(crypto: &Crypto, provider: Provider, verifier: &str) -> String {
    let payload = StatePayload { provider, verifier: verifier.to_string(), expires_at: Utc::now().timestamp() + MAX_AGE_SECS };
    let json = serde_json::to_vec(&payload).expect("payload serializes");
    let sealed = crypto.encrypt(&json, AAD).expect("encrypt works with a valid key");
    let mut out = sealed.nonce;
    out.extend(sealed.ciphertext);
    B64URL.encode(out)
}

pub fn open(crypto: &Crypto, state: &str, provider: Provider) -> Result<StatePayload, &'static str> {
    let raw = B64URL.decode(state).map_err(|_| "the state value is damaged")?;
    if raw.len() < 12 {
        return Err("the state value is damaged");
    }
    let (nonce, ciphertext) = raw.split_at(12);
    let plain = crypto
        .decrypt(ciphertext, nonce, crate::crypto::KEY_VERSION, AAD)
        .map_err(|_| "the state value is not valid")?;
    let payload: StatePayload = serde_json::from_slice(&plain).map_err(|_| "the state value is damaged")?;
    if payload.provider != provider {
        return Err("the state value is for another provider");
    }
    if payload.expires_at < Utc::now().timestamp() {
        return Err("the login took too long. Start again");
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_checks() {
        let crypto = Crypto::from_key_bytes(&[9u8; 32]).unwrap();
        let state = seal(&crypto, Provider::Gmail, "verifier-123");
        assert!(!state.contains("verifier-123"));
        assert_eq!(open(&crypto, &state, Provider::Gmail).unwrap().verifier, "verifier-123");
        assert!(open(&crypto, &state, Provider::Microsoft).is_err());
        assert!(open(&crypto, "garbage", Provider::Gmail).is_err());
        let other = Crypto::from_key_bytes(&[8u8; 32]).unwrap();
        assert!(open(&other, &state, Provider::Gmail).is_err());
    }
}
