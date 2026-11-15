//! Best-effort secret scrubbing before command output is shown to an LLM.

use std::sync::LazyLock;

use regex::Regex;

static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----", "[REDACTED PRIVATE KEY]"),
        (r"\bAKIA[0-9A-Z]{16}\b", "[REDACTED AWS KEY ID]"),
        (r"\bsk-ant-[A-Za-z0-9_\-]{10,}", "[REDACTED API KEY]"),
        (r"\bsk-[A-Za-z0-9_\-]{20,}", "[REDACTED API KEY]"),
        (r"\b(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}", "[REDACTED GITHUB TOKEN]"),
        (r"\bgithub_pat_[A-Za-z0-9_]{20,}", "[REDACTED GITHUB TOKEN]"),
        (r"\bglpat-[A-Za-z0-9_\-]{20,}", "[REDACTED GITLAB TOKEN]"),
        (r"\bxox[abprs]-[A-Za-z0-9\-]{10,}", "[REDACTED SLACK TOKEN]"),
        (r"(?i)\bbearer\s+[A-Za-z0-9._\-+/=]{16,}", "Bearer [REDACTED]"),
        (r"\b([a-z][a-z0-9+.\-]*://)[^:/\s@]+:[^@\s]+@", "${1}[REDACTED]@"),
        (
            r#"(?i)\b([A-Z0-9_]*(?:password|passwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|credentials?)[A-Z0-9_]*)(\s*[:=]\s*)("?)[^\s"',;]{3,}"#,
            "${1}${2}${3}[REDACTED]",
        ),
    ]
    .into_iter()
    .map(|(re, rep)| (Regex::new(re).expect("valid redaction regex"), rep))
    .collect()
});

pub fn redact(s: &str) -> String {
    let mut out = s.to_string();
    for (re, rep) in RULES.iter() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    out
}

/// Keep the head and tail of long output.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let head = max * 2 / 3;
    let tail = max - head;
    let mut h = head;
    while !s.is_char_boundary(h) {
        h -= 1;
    }
    let mut t = s.len() - tail;
    while !s.is_char_boundary(t) {
        t += 1;
    }
    format!("{}\n… [{} bytes elided] …\n{}", &s[..h], t - h, &s[t..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs() {
        let s = "DB_PASSWORD=hunter22\nurl=postgres://app:s3cret@db:5432/x\nAuthorization: Bearer abcdefghijklmnopqrstuv\nkey AKIAABCDEFGHIJKLMNOP";
        let r = redact(s);
        assert!(!r.contains("hunter22"), "{r}");
        assert!(!r.contains("s3cret"), "{r}");
        assert!(!r.contains("abcdefghijklmnopqrstuv"), "{r}");
        assert!(!r.contains("AKIAABCDEFGHIJKLMNOP"), "{r}");
        assert!(r.contains("DB_PASSWORD=[REDACTED]"), "{r}");
        assert_eq!(redact("nginx: worker process"), "nginx: worker process");
    }

    #[test]
    fn truncates_on_char_boundary() {
        let s = "é".repeat(5000);
        let t = truncate(&s, 1000);
        assert!(t.contains("elided"));
    }
}
