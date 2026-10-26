//! A small robots.txt reader for the Fetch web page node (RFC 9309).
//!
//! - The group that names our agent wins. Else the group for `*`. No group: everything is allowed.
//! - `Allow` and `Disallow` lines are patterns with `*` and a final `$`. The longest matching pattern wins; on a tie `Allow` wins.
//! - A missing file (4xx) allows everything. A server error (5xx) or no answer disallows everything.

/// The token of our user agent, as robots.txt files name it.
pub const AGENT: &str = "aikidoemail-automation";

#[derive(Debug, Clone, PartialEq)]
pub enum Robots {
    AllowAll,
    DisallowAll,
    Rules(Vec<Rule>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub allow: bool,
    pub pattern: String,
}

impl Robots {
    pub fn parse(text: &str) -> Robots {
        // groups: (agents, rules)
        let mut groups: Vec<(Vec<String>, Vec<Rule>)> = vec![];
        let mut in_agents = false;
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once(':') else { continue };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            match k.as_str() {
                "user-agent" => {
                    if !in_agents {
                        groups.push((vec![], vec![]));
                    }
                    in_agents = true;
                    if let Some(g) = groups.last_mut() {
                        g.0.push(v.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    in_agents = false;
                    if let Some(g) = groups.last_mut() {
                        // an empty Disallow allows everything: no rule
                        if !v.is_empty() {
                            g.1.push(Rule { allow: k == "allow", pattern: v.to_string() });
                        }
                    }
                }
                _ => in_agents = false,
            }
        }
        let specific: Vec<&(Vec<String>, Vec<Rule>)> = groups.iter().filter(|g| g.0.iter().any(|a| a != "*" && AGENT.contains(a.as_str()))).collect();
        let chosen: Vec<&(Vec<String>, Vec<Rule>)> = if specific.is_empty() { groups.iter().filter(|g| g.0.iter().any(|a| a == "*")).collect() } else { specific };
        let rules: Vec<Rule> = chosen.into_iter().flat_map(|g| g.1.clone()).collect();
        if rules.is_empty() { Robots::AllowAll } else { Robots::Rules(rules) }
    }

    /// From the answer for `/robots.txt`.
    pub fn from_response(status: u16, body: &str) -> Robots {
        match status {
            200..=299 => Robots::parse(body),
            500..=599 => Robots::DisallowAll,
            _ => Robots::AllowAll,
        }
    }

    /// May we fetch this path (with its query)?
    pub fn allows(&self, path_and_query: &str) -> bool {
        match self {
            Robots::AllowAll => true,
            Robots::DisallowAll => false,
            Robots::Rules(rules) => {
                if path_and_query == "/robots.txt" {
                    return true;
                }
                let mut best: Option<(usize, bool)> = None;
                for r in rules {
                    if matches(&r.pattern, path_and_query) {
                        let len = r.pattern.len();
                        if best.is_none_or(|(l, a)| len > l || (len == l && r.allow && !a)) {
                            best = Some((len, r.allow));
                        }
                    }
                }
                best.is_none_or(|(_, allow)| allow)
            }
        }
    }
}

/// `*` matches any run of characters. A final `$` pins the end. The pattern is a prefix otherwise.
fn matches(pattern: &str, path: &str) -> bool {
    let (pat, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pat.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !path.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if let Some(at) = path[pos..].find(part) {
            pos += at + part.len();
        } else {
            return false;
        }
    }
    if anchored {
        // the last part must end the path
        let last = parts.last().copied().unwrap_or("");
        if parts.len() == 1 { path == pat } else { path.ends_with(last) }
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_group_beats_the_star_group() {
        let r = Robots::parse("User-agent: *\nDisallow: /\n\nUser-agent: AikidoEmail-Automation\nDisallow: /private\n");
        assert!(r.allows("/about"));
        assert!(!r.allows("/private/x"));
        let r = Robots::parse("User-agent: *\nDisallow: /\n");
        assert!(!r.allows("/about"));
        assert!(r.allows("/robots.txt"));
    }

    #[test]
    fn allow_and_wildcards_and_the_longest_match() {
        let r = Robots::parse("User-agent: *\nDisallow: /search\nAllow: /search/public\nDisallow: /*.pdf$\nDisallow: /tmp*cache\n");
        assert!(!r.allows("/search?q=1"));
        assert!(r.allows("/search/public/a"));
        assert!(!r.allows("/files/a.pdf"));
        assert!(r.allows("/files/a.pdf?x=1"), "the `$` pins the end");
        assert!(!r.allows("/tmp/some/cache"));
        assert!(r.allows("/tmp/some"));
        assert!(r.allows("/index.html"));
    }

    #[test]
    fn missing_empty_and_broken_files() {
        assert_eq!(Robots::from_response(404, ""), Robots::AllowAll);
        assert_eq!(Robots::from_response(403, "x"), Robots::AllowAll);
        assert!(!Robots::from_response(503, "").allows("/"));
        assert!(Robots::from_response(200, "").allows("/anything"));
        assert!(Robots::from_response(200, "User-agent: *\nDisallow:\n").allows("/anything"));
        assert!(Robots::from_response(200, "garbage\n\n# comment only").allows("/x"));
        // several agent lines share one group
        let r = Robots::parse("User-agent: googlebot\nUser-agent: aikidoemail-automation\nDisallow: /no\n");
        assert!(!r.allows("/no"));
        // a comment after a rule is ignored
        assert!(!Robots::parse("User-agent: *\nDisallow: /a # why\n").allows("/a/b"));
    }
}
