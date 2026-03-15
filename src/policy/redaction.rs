use once_cell::sync::Lazy;
use regex::Regex;

static BEARER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)bearer\s+[A-Za-z0-9\._\-]+").expect("regex"));
static TOKEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[A-Za-z0-9_\-]{24,}").expect("regex"));
static PHONE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\+?[0-9][0-9\-\s]{7,}[0-9]").expect("regex"));

pub fn redact(input: &str) -> String {
    let step1 = BEARER_RE.replace_all(input, "Bearer [REDACTED]");
    let step2 = TOKEN_RE.replace_all(&step1, "[REDACTED_TOKEN]");
    let step3 = PHONE_RE.replace_all(&step2, "[REDACTED_PHONE]");
    step3.to_string()
}
