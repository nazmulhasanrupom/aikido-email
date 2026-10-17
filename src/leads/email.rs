//! Normalize and check an email address. Pure: no network.

/// Domains where different people are not one company. A reply from one `gmail.com` user must not stop
/// the others. Settings can add more (`free_mail_domains_extra`).
pub const FREE_MAIL_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "yahoo.com",
    "ymail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "gmx.com",
    "gmx.net",
    "mail.com",
    "zoho.com",
    "yandex.com",
    "fastmail.com",
];

pub fn is_free_mail(domain: &str, extra: &[String]) -> bool {
    let d = domain.trim().to_lowercase();
    FREE_MAIL_DOMAINS.contains(&d.as_str()) || extra.iter().any(|x| x.trim().eq_ignore_ascii_case(&d))
}

/// The part after the last `@`, lower case. Empty if there is none.
pub fn domain_of(email: &str) -> String {
    email.rsplit_once('@').map(|(_, d)| d.trim().to_lowercase()).unwrap_or_default()
}

/// Trim and lower-case, then check the shape: one `@`, a local part up to 64 characters, a domain with
/// dots and valid labels, 254 characters in all. Returns the address, or the reason it is bad.
pub fn normalize(raw: &str) -> Result<String, &'static str> {
    let email = raw.trim().to_lowercase();
    if email.is_empty() {
        return Err("email is empty");
    }
    if email.len() > 254 {
        return Err("email is longer than 254 characters");
    }
    if email.matches('@').count() != 1 {
        return Err("email needs exactly one @");
    }
    let (local, domain) = email.split_once('@').unwrap_or_default();
    if local.is_empty() || local.len() > 64 {
        return Err("the part before @ must be 1 to 64 characters");
    }
    let local_ok = local.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-".contains(c));
    if !local_ok || local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return Err("the part before @ has a character that is not allowed");
    }
    if !domain_ok(domain) {
        return Err("the domain is not valid");
    }
    Ok(email)
}

fn domain_ok(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 || !domain.contains('.') {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    let label_ok = |l: &str| {
        !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if !labels.iter().all(|l| label_ok(l)) {
        return false;
    }
    // the last label (the TLD) has a letter in it: `a@b.123` and IP-like domains are refused
    labels.last().is_some_and(|tld| tld.len() >= 2 && tld.chars().any(|c| c.is_ascii_alphabetic()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_lowers() {
        assert_eq!(normalize("  John@Example.COM ").unwrap(), "john@example.com");
    }

    #[test]
    fn bad_shapes() {
        for bad in ["", "no-at", "a@@b.com", "a@b", "@b.com", "a@.com", "a@b..com", "a b@c.com", ".a@b.com", "a.@b.com", "a@-b.com", "a@b.123", "a@b.c", "ünï@b.com"] {
            assert!(normalize(bad).is_err(), "{bad} should be refused");
        }
        let long = format!("{}@b.com", "a".repeat(65));
        assert!(normalize(&long).is_err());
        let long = format!("a@{}.com", "b".repeat(250));
        assert!(normalize(&long).is_err());
    }

    #[test]
    fn good_shapes() {
        for good in ["a+tag@b.co", "first.last@sub.example.org", "o'neil@example.com", "x_y@xn--80ak6aa92e.com"] {
            assert!(normalize(good).is_ok(), "{good} should pass");
        }
    }

    #[test]
    fn free_mail() {
        assert!(is_free_mail("Gmail.com", &[]));
        assert!(!is_free_mail("acme.io", &[]));
        assert!(is_free_mail("acme.io", &["ACME.io".to_string()]));
        assert_eq!(domain_of("a@B.com"), "b.com");
    }
}
