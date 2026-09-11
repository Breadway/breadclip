//! Best-effort content heuristics for never persisting obvious secrets.
//!
//! The password-manager path (`CLIPBOARD_STATE=sensitive` / the
//! `x-kde-passwordManagerHint` MIME type) only catches copies an app
//! deliberately flagged. These rules catch copies that *look* like secrets
//! even when the app didn't flag them: one-time codes, credit card
//! numbers, private keys, credential lines. They are deliberately
//! conservative (a false "skip" is cheaper than a leaked password) and they
//! are a convenience, not a security boundary: the 0600 file permissions
//! and 0700 data dir are the real protection.
//!
//! The `[ignore]` section of `config.toml` layers user overrides on top:
//! extra regex `patterns` to treat as sensitive, and built-in rule names to
//! `disable` when a heuristic trips on something the user genuinely wants
//! kept. An absent section leaves the built-in behaviour exactly as before.

use breadclip_core::config::IgnoreConfig;
use regex::Regex;
use std::collections::HashSet;

/// The built-in heuristics, by the name used in `[ignore] disable`.
pub const BUILTIN_RULES: [&str; 5] = [
    "private_key",
    "credential_line",
    "otp",
    "card_number",
    "api_token",
];

/// The built-in heuristics plus any user overrides from `[ignore]`.
/// `Ruleset::default()` is the built-in heuristic with nothing changed.
#[derive(Default)]
pub struct Ruleset {
    extra: Vec<Regex>,
    disabled: HashSet<String>,
}

impl Ruleset {
    /// Compile the user's `[ignore]` overrides. An invalid regex or an
    /// unknown rule name is logged to stderr and skipped rather than
    /// failing the capture.
    pub fn from_config(cfg: &IgnoreConfig) -> Self {
        let mut extra = Vec::new();
        for pat in &cfg.patterns {
            match Regex::new(pat) {
                Ok(re) => extra.push(re),
                Err(e) => {
                    eprintln!("breadclipd: ignoring invalid [ignore] pattern {pat:?}: {e}")
                }
            }
        }
        let mut disabled = HashSet::new();
        for name in &cfg.disable {
            if BUILTIN_RULES.contains(&name.as_str()) {
                disabled.insert(name.clone());
            } else {
                eprintln!(
                    "breadclipd: unknown [ignore] disable rule {name:?} (valid: {})",
                    BUILTIN_RULES.join(", ")
                );
            }
        }
        Self { extra, disabled }
    }

    fn on(&self, rule: &str) -> bool {
        !self.disabled.contains(rule)
    }

    /// Would persisting this text be a bad idea? Skipped copies never reach
    /// the database (or the event bus), like the password-manager path.
    pub fn is_sensitive(&self, text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        (self.on("private_key") && looks_like_private_key(trimmed))
            || (self.on("credential_line") && looks_like_credential_line(trimmed))
            || (self.on("otp") && looks_like_otp(trimmed))
            || (self.on("card_number") && contains_card_number(trimmed))
            || (self.on("api_token") && contains_api_token(trimmed))
            || self.extra.iter().any(|re| re.is_match(trimmed))
    }
}

/// The built-in heuristic with no user overrides.
#[cfg(test)]
pub fn is_sensitive(text: &str) -> bool {
    Ruleset::default().is_sensitive(text)
}

/// PEM/OpenSSH private key blocks (and PGP blocks, which are just as
/// sensitive).
fn looks_like_private_key(text: &str) -> bool {
    text.contains("-----BEGIN") || text.contains("PRIVATE KEY")
}

/// A line that labels a credential directly: `password: hunter2`,
/// `passwd = hunter2`, etc.
fn looks_like_credential_line(text: &str) -> bool {
    let first = text.lines().next().unwrap_or("").trim().to_ascii_lowercase();
    const PREFIXES: [&str; 8] = [
        "password:",
        "password =",
        "password=",
        "passwd:",
        "passwd =",
        "passwd=",
        "pw:",
        "pass:",
    ];
    PREFIXES.iter().any(|p| first.starts_with(p))
}

/// A one-time code: a short copy that mentions a code keyword and contains
/// a 6–8 digit run. A *bare* 6-digit number is deliberately not flagged:
/// too many legitimate numbers have that shape.
fn looks_like_otp(text: &str) -> bool {
    if text.len() > 60 {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    const LABELS: [&str; 8] = [
        "code", "otp", "verification", "verif", "2fa", "passcode", "one-time", "one time",
    ];
    if !LABELS.iter().any(|k| lower.contains(k)) {
        return false;
    }
    has_digit_run(text, 6, 8)
}

/// Credit card numbers: 13–19 digit runs (allowing spaces/dashes between
/// groups) that pass the Luhn check.
fn contains_card_number(text: &str) -> bool {
    let mut run = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            run.push(c);
        } else if (c == ' ' || c == '-') && !run.is_empty() {
            run.push(' '); // keep one run across group separators
        } else {
            if card_like(&run) {
                return true;
            }
            run.clear();
        }
    }
    card_like(&run)
}

fn card_like(run: &str) -> bool {
    let digits: String = run.chars().filter(|c| c.is_ascii_digit()).collect();
    (13..=19).contains(&digits.len()) && luhn_valid(&digits)
}

fn luhn_valid(digits: &str) -> bool {
    let mut sum: u32 = 0;
    let mut double = false;
    for b in digits.bytes().rev() {
        let mut d = (b - b'0') as u32;
        if double {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        double = !double;
    }
    sum.is_multiple_of(10)
}

/// Well-known API token prefixes (OpenAI, GitHub, Slack, AWS access keys).
fn contains_api_token(text: &str) -> bool {
    const PREFIXES: [&str; 7] = ["sk-", "sk-proj-", "ghp_", "gho_", "xoxb-", "xoxp-", "AKIA"];
    PREFIXES.iter().any(|p| text.contains(p))
}

fn has_digit_run(text: &str, min: usize, max: usize) -> bool {
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_digit() {
            run += 1;
        } else {
            if run >= min && run <= max {
                return true;
            }
            run = 0;
        }
    }
    run >= min && run <= max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_key_blocks_are_sensitive() {
        assert!(is_sensitive(
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA..."
        ));
        assert!(is_sensitive("-----BEGIN PGP MESSAGE-----"));
    }

    #[test]
    fn credential_labeled_lines_are_sensitive() {
        assert!(is_sensitive("password: hunter2"));
        assert!(is_sensitive("Password = correct horse battery staple"));
        assert!(is_sensitive("passwd=hunter2"));
    }

    #[test]
    fn labeled_one_time_codes_are_sensitive() {
        assert!(is_sensitive("Your verification code is 483920"));
        assert!(is_sensitive("483920 is your code"));
        assert!(is_sensitive("OTP: 12345678"));
    }

    #[test]
    fn bare_numbers_are_not_sensitive() {
        // A bare 6-digit number is too ambiguous to drop on purpose.
        assert!(!is_sensitive("483920"));
        assert!(!is_sensitive("The answer is 42"));
        // A long random number that happens to be 16 digits but fails Luhn.
        assert!(!is_sensitive("1234567890123456"));
    }

    #[test]
    fn luhn_valid_card_numbers_are_sensitive() {
        assert!(is_sensitive("4111 1111 1111 1111"));
        assert!(is_sensitive("card number: 4111-1111-1111-1111"));
    }

    #[test]
    fn api_token_prefixes_are_sensitive() {
        assert!(is_sensitive("sk-proj-abc123def456"));
        assert!(is_sensitive("ghp_1234567890abcdefghijklmnopqrstuvwxyz"));
        assert!(is_sensitive("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn normal_prose_is_not_sensitive() {
        assert!(!is_sensitive("just a normal sentence someone copied"));
        assert!(!is_sensitive("here is the plan for next week"));
        assert!(!is_sensitive("the password field in the form is empty"));
    }

    fn ruleset(patterns: &[&str], disable: &[&str]) -> Ruleset {
        Ruleset::from_config(&IgnoreConfig {
            patterns: patterns.iter().map(|s| s.to_string()).collect(),
            disable: disable.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn default_ruleset_matches_the_builtin_heuristic() {
        let rs = Ruleset::default();
        assert!(rs.is_sensitive("OTP: 12345678"));
        assert!(!rs.is_sensitive("just a normal sentence someone copied"));
    }

    #[test]
    fn extra_patterns_flag_otherwise_plain_text() {
        let rs = ruleset(&[r"^ACME-\d{4,}\b"], &[]);
        assert!(rs.is_sensitive("ACME-4821 is the internal ticket"));
        assert!(!rs.is_sensitive("ACME-42 is too short"));
        // built-ins still apply alongside the extra pattern
        assert!(rs.is_sensitive("password: hunter2"));
    }

    #[test]
    fn disabling_a_builtin_rule_relaxes_just_that_rule() {
        let rs = ruleset(&[], &["otp"]);
        assert!(!rs.is_sensitive("Your verification code is 483920"));
        // other built-ins untouched
        assert!(rs.is_sensitive("4111 1111 1111 1111"));
        assert!(rs.is_sensitive("-----BEGIN RSA PRIVATE KEY-----"));
    }

    #[test]
    fn invalid_pattern_is_skipped_not_fatal() {
        let rs = ruleset(&["valid-\\w+", "("], &[]);
        assert!(rs.is_sensitive("valid-token"));
        assert!(!rs.is_sensitive("plain text"));
    }

    #[test]
    fn unknown_disable_name_is_ignored() {
        let rs = ruleset(&[], &["not_a_rule"]);
        assert!(rs.is_sensitive("OTP: 12345678"));
    }
}
