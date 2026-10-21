//! Open tracking. Count email opens without hurting deliverability: the tracker is served from the sender's own
//! domain (a CNAME to this server, with its own HTTPS certificate) and is a normal visible image.
//!
//! * Type 1, `text_image`: a word in the email (usually the name in the signature) becomes an image of the same text.
//! * Type 2, `signature_image`: the first image that is not in a link is served from the tracking domain.
//!
//! Parts: `domains` (the CNAME check loop), `acme` and `tls` (certificates and the HTTPS listener), `server` (the
//! public image routes), `html`, `style` and `render` (find, read and draw the text), `assets` (stored images),
//! `send` (the hook in the send pipeline), `settings`, `routes` and `stats` (the API).
use crate::crypto::Crypto;
use zeroize::Zeroizing;

pub mod acme;
pub mod assets;
pub mod dns;
pub mod domains;
pub mod html;
pub mod proxy;
pub mod render;
pub mod routes;
pub mod send;
pub mod server;
pub mod settings;
pub mod stats;
pub mod style;
pub mod tls;

/// The DNS of a domain that is not connected is checked this often ...
pub const DNS_CHECK_MINS: i64 = 5;
/// ... for this long. Then it stops and the domain shows "not found" until the user clicks "Check now".
pub const CHECK_WINDOW_HOURS: i64 = 72;
/// A connected domain is checked once a day.
pub const CONNECTED_CHECK_HOURS: i64 = 24;
/// A certificate is renewed this many days before it ends.
pub const RENEW_BEFORE_DAYS: i64 = 30;

/// Encrypt a secret for a database column: key version (4 bytes), nonce (12 bytes), ciphertext.
pub fn seal(crypto: &Crypto, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
    let s = crypto.encrypt(plain, aad).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(16 + s.ciphertext.len());
    out.extend_from_slice(&s.key_version.to_be_bytes());
    out.extend_from_slice(&s.nonce);
    out.extend_from_slice(&s.ciphertext);
    Ok(out)
}

pub fn open(crypto: &Crypto, data: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    if data.len() < 16 {
        return Err("the saved value is damaged".into());
    }
    let version = i32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    crypto.decrypt(&data[16..], &data[4..16], version, aad).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_values_open_only_with_the_same_aad() {
        let c = Crypto::from_key_bytes(&[9u8; 32]).unwrap();
        let sealed = seal(&c, b"secret pem", b"row-1").unwrap();
        assert!(!sealed.windows(10).any(|w| w == b"secret pem"));
        assert_eq!(&*open(&c, &sealed, b"row-1").unwrap(), b"secret pem");
        assert!(open(&c, &sealed, b"row-2").is_err());
        assert!(open(&c, &sealed[..10], b"row-1").is_err());
    }
}
