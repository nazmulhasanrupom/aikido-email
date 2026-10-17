//! The rule mapper (no AI). It was the "mapping guess" of the CSV import; the import and the later
//! database path share it. A header is matched by its squashed form (no case, spaces or punctuation).
use super::proposal::MapTarget;
use super::samples::{SampleSet, is_email};
use crate::leads::fields::FieldDefs;
use std::collections::HashSet;

/// A column name without case, spaces and punctuation: `"E-mail Address"` becomes `emailaddress`.
pub fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

const GUESSES: &[(&str, &[&str])] = &[
    ("email", &["email", "emailaddress", "emailaddr", "mail", "workemail", "businessemail", "emailid", "eml", "eaddress"]),
    ("first_name", &["firstname", "first", "givenname", "fname", "forename"]),
    ("last_name", &["lastname", "last", "surname", "familyname", "lname"]),
    ("company_name", &["company", "companyname", "organization", "organisation", "org", "employer", "business", "businessname"]),
    ("job_title", &["jobtitle", "title", "position", "role", "jobrole", "jobposition", "designation"]),
    ("phone", &["phone", "phonenumber", "mobile", "mobilephone", "tel", "telephone", "cell"]),
    ("website", &["website", "web", "companywebsite", "homepage", "site", "websiteurl"]),
    ("linkedin_url", &["linkedin", "linkedinurl", "linkedinprofile", "linkedinlink"]),
    ("location", &["location", "city", "region", "place"]),
    ("timezone", &["timezone", "tz"]),
    ("notes", &["notes", "note", "comment", "comments"]),
    ("tags", &["tags", "tag", "labels"]),
];

/// The target string of a header (`email`, `company_name`, `custom:<key>`), if the name is known.
pub fn lookup(header: &str, defs: &FieldDefs) -> Option<String> {
    let key = squash(header);
    GUESSES
        .iter()
        .find(|(_, names)| names.contains(&key.as_str()))
        .map(|(t, _)| t.to_string())
        .or_else(|| defs.by_key.keys().find(|k| squash(k) == key).map(|k| format!("custom:{k}")))
}

/// Guess where each column goes. A header that matches a custom field key becomes `custom:<key>`. Others are
/// `ignore`. When two columns fit one target, the first wins and the other is ignored.
pub fn guess_mapping(headers: &[String], defs: &FieldDefs) -> Vec<(String, String)> {
    let mut used: HashSet<String> = HashSet::new();
    headers
        .iter()
        .map(|h| {
            let target = lookup(h, defs).filter(|t| !used.contains(t));
            match target {
                Some(t) => {
                    used.insert(t.clone());
                    (h.clone(), t)
                }
                None => (h.clone(), "ignore".into()),
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct RuleHit {
    pub target: MapTarget,
    pub reason: &'static str,
}

/// 4 of 5 samples (at least 3 values) look like an email address.
fn looks_like_emails(samples: &[String]) -> bool {
    samples.len() >= 3 && samples.iter().filter(|s| is_email(s)).count() * 5 >= samples.len() * 4
}

/// One entry per column: a hit, or `None` when the rules could not decide. Takes the real samples.
pub fn map_columns(set: &SampleSet, defs: &FieldDefs) -> Vec<Option<RuleHit>> {
    let mut used: HashSet<String> = HashSet::new();
    set.columns
        .iter()
        .map(|c| {
            if let Some(t) = lookup(&c.header, defs).filter(|t| !used.contains(t)) {
                used.insert(t.clone());
                return Some(RuleHit { target: MapTarget::from_string(&t), reason: "header alias" });
            }
            if !used.contains("email") && looks_like_emails(&c.samples) {
                used.insert("email".into());
                return Some(RuleHit { target: MapTarget::from_string("email"), reason: "values look like emails" });
            }
            None
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_headers_map_by_alias_and_by_value() {
        let defs = FieldDefs::default();
        let g = guess_mapping(&["E-mail Addr".into(), "Org".into(), "Job Role".into(), "Other".into()], &defs);
        assert_eq!(g[0].1, "email");
        assert_eq!(g[1].1, "company_name");
        assert_eq!(g[2].1, "job_title");
        assert_eq!(g[3].1, "ignore");
        let set = SampleSet {
            columns: vec![crate::leads::mapping::samples::ColumnSample { index: 0, header: "Contact".into(), samples: vec!["a@x.io".into(), "b@y.io".into(), "c@z.io".into(), "d@w.io".into(), "n/a".into()], empty_count: 0 }],
            rows_scanned: 5,
        };
        let hits = map_columns(&set, &defs);
        assert_eq!(hits[0].as_ref().unwrap().target, MapTarget::from_string("email"));
    }
}
