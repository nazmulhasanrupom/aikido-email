//! `SampleSet`: for each column, the header, up to N real values and the count of empty cells seen.
//! The mappers take a `SampleSet` and never read a CSV. Later a database table makes the same input.
use crate::leads::import::{cell, csv_reader, headers_of};

pub const MAX_SCAN_ROWS: usize = 500;
pub const MAX_VALUE_CHARS: usize = 100;
/// More columns than this: the rest gets the rules only.
pub const MAX_AI_COLUMNS: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSample {
    pub index: usize,
    pub header: String,
    pub samples: Vec<String>,
    pub empty_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SampleSet {
    pub columns: Vec<ColumnSample>,
    /// How many rows were read to find the samples (at most 500).
    pub rows_scanned: usize,
}

impl SampleSet {
    /// Per column: the first `per_column` rows where the column is not empty. Reads at most 500 rows,
    /// and stops early when every column has enough.
    pub fn from_csv(data: &[u8], per_column: usize) -> Result<Self, String> {
        let mut reader = csv_reader(data);
        let headers = headers_of(&mut reader)?;
        let mut columns: Vec<ColumnSample> =
            headers.into_iter().enumerate().map(|(index, header)| ColumnSample { index, header, samples: Vec::new(), empty_count: 0 }).collect();
        let mut rows_scanned = 0;
        for rec in reader.byte_records().take(MAX_SCAN_ROWS) {
            let Ok(rec) = rec else { break };
            rows_scanned += 1;
            for c in columns.iter_mut() {
                let v = cell(&rec, c.index);
                if v.is_empty() {
                    c.empty_count += 1;
                } else if c.samples.len() < per_column {
                    c.samples.push(v.chars().take(MAX_VALUE_CHARS).collect());
                }
            }
            if columns.iter().all(|c| c.samples.len() >= per_column) {
                break;
            }
        }
        Ok(Self { columns, rows_scanned })
    }

    /// The same set with emails and phone numbers masked.
    pub fn masked(&self) -> Self {
        let mut s = self.clone();
        for c in s.columns.iter_mut() {
            for v in c.samples.iter_mut() {
                *v = mask_value(v);
            }
        }
        s
    }
}

pub fn is_email(t: &str) -> bool {
    let t = t.trim_matches(|c: char| ",;:()<>\"'".contains(c));
    match t.split_once('@') {
        Some((local, domain)) => !local.is_empty() && !domain.contains('@') && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && !t.contains(char::is_whitespace),
        None => false,
    }
}

fn is_phone(v: &str) -> bool {
    let v = v.trim();
    let digits = v.chars().filter(char::is_ascii_digit).count();
    if !v.chars().all(|c| c.is_ascii_digit() || " +-()./".contains(c)) || !(7..=15).contains(&digits) {
        return false;
    }
    if chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok() {
        return false;
    }
    v.starts_with('+') || v.starts_with('(') || digits >= 10
}

fn tail4(tok: &str) -> String {
    let n = tok.chars().count();
    if n <= 4 { tok.to_string() } else { format!("***{}", tok.chars().skip(n - 4).collect::<String>()) }
}

/// `john.doe@acme.com` becomes `j***@acme.com`. `+1 415 555 0101` becomes `+1 *** *** 0101`.
/// Other text is returned as it is.
pub fn mask_value(v: &str) -> String {
    if is_phone(v) {
        let toks: Vec<&str> = v.split_whitespace().collect();
        return match toks.as_slice() {
            [one] => format!("{}{}", if one.starts_with('+') { "+" } else { "" }, tail4(one)),
            [first, middle @ .., last] => {
                let head = if first.starts_with('+') { (*first).to_string() } else { "***".to_string() };
                let mid: Vec<&str> = middle.iter().map(|_| "***").collect();
                let mut parts = vec![head];
                parts.extend(mid.into_iter().map(String::from));
                parts.push(tail4(last));
                parts.join(" ")
            }
            [] => String::new(),
        };
    }
    if !v.contains('@') {
        return v.to_string();
    }
    v.split_whitespace()
        .map(|t| match (is_email(t), t.split_once('@')) {
            (true, Some((local, domain))) => {
                let lead: String = local.trim_start_matches(|c: char| ",;:()<>\"'".contains(c)).chars().take(1).collect();
                format!("{lead}***@{domain}")
            }
            _ => t.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_and_phones_are_masked_and_other_text_is_not() {
        assert_eq!(mask_value("john.doe@acme.com"), "j***@acme.com");
        assert_eq!(mask_value("+1 415 555 0101"), "+1 *** *** 0101");
        assert_eq!(mask_value("(415) 555-0101"), "*** ***0101");
        assert_eq!(mask_value("+14155550101"), "+***0101");
        assert_eq!(mask_value("write to ann@x.io please"), "write to a***@x.io please");
        assert_eq!(mask_value("2024-01-15"), "2024-01-15");
        assert_eq!(mask_value("Acme Inc"), "Acme Inc");
        assert_eq!(mask_value("SaaS"), "SaaS");
    }

    #[test]
    fn a_sparse_column_still_gets_real_values_and_long_values_are_cut() {
        let long = "x".repeat(150);
        let mut csv = String::from("a,b\n");
        for i in 0..8 {
            csv.push_str(&format!("{i},{}\n", if i < 6 { String::new() } else { long.clone() }));
        }
        let s = SampleSet::from_csv(csv.as_bytes(), 5).unwrap();
        assert_eq!(s.columns[0].samples.len(), 5);
        assert_eq!(s.columns[1].samples.len(), 2);
        assert_eq!(s.columns[1].samples[0].chars().count(), MAX_VALUE_CHARS);
        assert_eq!(s.columns[1].empty_count, 6);
    }
}
