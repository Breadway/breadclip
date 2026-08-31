//! Best-effort content heuristics for never persisting obvious secrets.
//!
//! The password-manager path (`CLIPBOARD_STATE=sensitive` / the
//! `x-kde-passwordManagerHint` MIME type) only catches copies an app
//! deliberately flagged. These rules catch copies that *look* like secrets
//! even when the app didn't flag them — one-time codes, credit card
//! numbers, private keys, credential lines. They are deliberately
//! conservative (a false "skip" is cheaper than a leaked password) and they
//! are a convenience, not a security boundary: the 0600 file permissions
//! and 0700 data dir are the real protection.

/// Would persisting this text be a bad idea? Skipped copies never reach the
/// database (or the event bus), like the password-manager path.
pub fn is_sensitive(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    looks_like_private_key(trimmed)
        || looks_like_credential_line(trimmed)
        || looks_like_otp(trimmed)
        || contains_card_number(trimmed)
        || contains_api_token(trimmed)
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
/// a 6–8 digit run. A *bare* 6-digit number is deliberately not flagged —
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
}
