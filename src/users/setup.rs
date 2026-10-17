//! The one-time setup code. On first boot (no user yet) the server prints a code and saves it to
//! `<data_dir>/setup_code.txt` (mode 600). The setup page asks for it, so a stranger cannot claim
//! a fresh server that is open to the internet. The code dies when the owner is made.
use super::repo::sha256;
use crate::config::Config;
use rand::Rng;
use rand::rngs::OsRng;
use sqlx::PgPool;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

/// No 0, 1, I, O: they look alike.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const LEN: usize = 12;

/// Upper case letters and digits only. Accepts `abcd-efgh-jkmn` and `ABCD EFGH JKMN`.
pub fn normalize_code(input: &str) -> String {
    input.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

pub fn hash_code(input: &str) -> Vec<u8> {
    sha256(&normalize_code(input))
}

/// `ABCD-EFGH-JKMN`
pub fn display(code: &str) -> String {
    code.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
}

fn path(cfg: &Config) -> std::path::PathBuf {
    cfg.data_dir.join("setup_code.txt")
}

fn generate() -> String {
    (0..LEN).map(|_| ALPHABET[OsRng.gen_range(0..ALPHABET.len())] as char).collect()
}

/// Make sure a setup code exists while there is no user. Returns the code (normalized) to show.
/// Returns `None` when the owner already exists (and removes the old file).
/// Safe to run on every start, and from two server copies at once.
pub async fn ensure_setup_code(pool: &PgPool, cfg: &Config) -> anyhow::Result<Option<String>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(7306002)").execute(&mut *tx).await?;
    let (users,): (i64,) = sqlx::query_as("SELECT count(*) FROM users").fetch_one(&mut *tx).await?;
    if users > 0 {
        let _ = std::fs::remove_file(path(cfg));
        return Ok(None);
    }
    let stored: Option<Vec<u8>> = sqlx::query_scalar("SELECT setup_code_hash FROM app_state WHERE id = 1").fetch_one(&mut *tx).await?;
    if let (Some(stored), Ok(text)) = (&stored, std::fs::read_to_string(path(cfg))) {
        let code = normalize_code(&text);
        if &hash_code(&code) == stored {
            tx.commit().await?;
            return Ok(Some(code));
        }
    }
    let code = generate();
    std::fs::create_dir_all(&cfg.data_dir)?;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path(cfg))?;
    writeln!(file, "{}", display(&code))?;
    sqlx::query("UPDATE app_state SET setup_code_hash = $1 WHERE id = 1").bind(hash_code(&code)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(code))
}

/// Print the code in a box.
pub fn announce(cfg: &Config, code: &str) {
    let lines = [
        "AIKIDO EMAIL: SETUP CODE".to_string(),
        String::new(),
        display(code),
        String::new(),
        "Open the app in your browser and type this code to make the owner account.".into(),
        format!("Also saved to {} (mode 600).", path(cfg).display()),
    ];
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let bar = format!("+{}+", "-".repeat(width + 2));
    println!("{bar}");
    for l in &lines {
        println!("| {l:<width$} |");
    }
    println!("{bar}");
}

pub fn remove_file(cfg: &Config) {
    let _ = std::fs::remove_file(path(cfg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_12_safe_characters() {
        let c = generate();
        assert_eq!(c.len(), 12);
        assert!(c.bytes().all(|b| ALPHABET.contains(&b)));
        assert_ne!(c, generate());
    }

    #[test]
    fn typed_codes_are_forgiving() {
        assert_eq!(normalize_code(" abcd-efgh jkmn\n"), "ABCDEFGHJKMN");
        assert_eq!(hash_code("abcd-efgh-jkmn"), hash_code("ABCDEFGHJKMN"));
        assert_eq!(display("ABCDEFGHJKMN"), "ABCD-EFGH-JKMN");
    }
}
