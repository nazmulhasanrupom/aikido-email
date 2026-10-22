//! Settings that a person types in the app (Settings → Environment): the Google and Microsoft keys.
//! They are saved in the `app_settings` table (secrets are sealed). A process-wide copy (the overlay)
//! is read by the OAuth code, so a save works at once, with no restart.
//!
//! The env var always wins: a key set in `.env` (or by Docker or Coolify) is read-only in the app.
pub mod routes;
pub mod store;

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// One setting that the app can save.
pub struct Item {
    pub key: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub secret: bool,
    pub default: Option<&'static str>,
}

pub const GOOGLE: &str = "google";
pub const MICROSOFT: &str = "microsoft";

pub const CATALOG: &[Item] = &[
    Item { key: "GOOGLE_CLIENT_ID", group: GOOGLE, label: "Client ID", help: "Ends in .apps.googleusercontent.com", secret: false, default: None },
    Item { key: "GOOGLE_CLIENT_SECRET", group: GOOGLE, label: "Client secret", help: "From the same OAuth client in Google Cloud.", secret: true, default: None },
    Item { key: "MS_CLIENT_ID", group: MICROSOFT, label: "Application (client) ID", help: "From the app registration in Microsoft Entra.", secret: false, default: None },
    Item { key: "MS_CLIENT_SECRET", group: MICROSOFT, label: "Client secret value", help: "Copy the Value of the secret, not its ID.", secret: true, default: None },
    Item { key: "MS_TENANT", group: MICROSOFT, label: "Tenant", help: "`common` for any account, or your tenant ID.", secret: false, default: Some("common") },
];

pub fn item(key: &str) -> Option<&'static Item> {
    CATALOG.iter().find(|i| i.key == key)
}

static OVERLAY: LazyLock<RwLock<HashMap<String, String>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Replace the saved values (after a load from the database).
pub fn set_overlay(values: HashMap<String, String>) {
    if let Ok(mut o) = OVERLAY.write() {
        *o = values;
    }
}

/// The value of a setting. The env var wins when it is set and not empty. Else the value saved in the app.
pub fn get(name: &str) -> Option<String> {
    pick(std::env::var(name).ok().as_deref(), overlay_value(name).as_deref())
}

/// Where the value of a setting comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Env,
    App,
    Unset,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::App => "app",
            Source::Unset => "unset",
        }
    }
}

pub fn source(name: &str) -> Source {
    source_of(std::env::var(name).ok().as_deref(), overlay_value(name).as_deref())
}

fn overlay_value(name: &str) -> Option<String> {
    OVERLAY.read().ok()?.get(name).cloned()
}

fn filled(v: Option<&str>) -> Option<&str> {
    v.filter(|v| !v.trim().is_empty())
}

/// The env value wins over the saved one. An empty value counts as not set.
fn pick(env: Option<&str>, saved: Option<&str>) -> Option<String> {
    filled(env).or(filled(saved)).map(str::to_string)
}

fn source_of(env: Option<&str>, saved: Option<&str>) -> Source {
    if filled(env).is_some() {
        Source::Env
    } else if filled(saved).is_some() {
        Source::App
    } else {
        Source::Unset
    }
}

/// Why a value cannot be saved. `None` when it is fine.
pub fn validate(key: &str, value: &str) -> Option<&'static str> {
    let v = value.trim();
    if v.is_empty() {
        return Some("The value is empty. Clear the field to remove the setting.");
    }
    if v.len() > 512 || v.chars().any(char::is_control) {
        return Some("The value is too long or has hidden characters.");
    }
    match key {
        "MS_TENANT" if !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') => {
            Some("The tenant can have letters, digits, dots and dashes only.")
        }
        _ => None,
    }
}

/// `abcd` for a secret that ends in `abcd`. A short secret shows nothing.
pub fn last4(secret: &str) -> Option<String> {
    let chars: Vec<char> = secret.trim().chars().collect();
    (chars.len() >= 8).then(|| chars[chars.len() - 4..].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_wins_over_the_saved_value() {
        assert_eq!(pick(Some("from-env"), Some("from-app")).as_deref(), Some("from-env"));
        assert_eq!(source_of(Some("from-env"), Some("from-app")), Source::Env);
    }

    #[test]
    fn an_empty_env_value_counts_as_not_set() {
        assert_eq!(pick(Some("  "), Some("from-app")).as_deref(), Some("from-app"));
        assert_eq!(source_of(Some(""), Some("from-app")), Source::App);
        assert_eq!(pick(Some(""), None), None);
        assert_eq!(source_of(None, None), Source::Unset);
    }

    #[test]
    fn the_catalog_is_valid() {
        let mut keys: Vec<_> = CATALOG.iter().map(|i| i.key).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), CATALOG.len(), "keys are unique");
        assert!(CATALOG.iter().all(|i| i.group == GOOGLE || i.group == MICROSOFT));
        assert!(item("GOOGLE_CLIENT_SECRET").unwrap().secret);
        assert!(!item("GOOGLE_CLIENT_ID").unwrap().secret);
        assert!(item("DATABASE_URL").is_none());
    }

    #[test]
    fn values_are_checked() {
        assert!(validate("GOOGLE_CLIENT_ID", "abc.apps.googleusercontent.com").is_none());
        assert!(validate("GOOGLE_CLIENT_ID", "   ").is_some());
        assert!(validate("MS_TENANT", "contoso.onmicrosoft.com").is_none());
        assert!(validate("MS_TENANT", "common").is_none());
        assert!(validate("MS_TENANT", "bad tenant/..").is_some());
        assert!(validate("GOOGLE_CLIENT_SECRET", "a\nb").is_some());
    }

    #[test]
    fn a_secret_shows_only_its_last_four_characters() {
        assert_eq!(last4("GOCSPX-abcdefgh1234").as_deref(), Some("1234"));
        assert_eq!(last4("short"), None);
    }
}
