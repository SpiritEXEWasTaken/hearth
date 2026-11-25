//! Secret redaction for anything shown to read-only users, logs or the CLI.

/// True if an environment variable or setting name looks like it holds a secret.
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    const MARKERS: &[&str] = &[
        "PASS",
        "SECRET",
        "TOKEN",
        "APIKEY",
        "API_KEY",
        "PRIVATE",
        "CREDENTIAL",
        "_KEY",
        "SALT",
    ];
    MARKERS.iter().any(|m| k.contains(m)) || k == "KEY"
}

pub const REDACTED: &str = "***";

/// Redacts secret values in compose-style YAML text, line by line, for both
/// `KEY: value` (mapping) and `- KEY=value` (list) environment forms.
/// It is deliberately conservative: when in doubt, a value is hidden.
pub fn redact_compose_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&redact_line(line));
    }
    out
}

fn redact_line(line: &str) -> String {
    let (body, nl) = match line.strip_suffix('\n') {
        Some(b) => (b, "\n"),
        None => (line, ""),
    };
    let trimmed = body.trim_start();
    let indent = &body[..body.len() - trimmed.len()];

    // List form: "- KEY=value"
    if let Some(item) = trimmed.strip_prefix("- ") {
        let item_unquoted = item.trim_matches(|c| c == '"' || c == '\'');
        if let Some((k, _)) = item_unquoted.split_once('=')
            && is_secret_key(k.trim())
        {
            return format!("{indent}- {}={REDACTED}{nl}", k.trim());
        }
        return line.to_string();
    }
    // Mapping form: "KEY: value"
    if let Some((k, v)) = trimmed.split_once(':') {
        let key = k.trim().trim_matches(|c| c == '"' || c == '\'');
        if !v.trim().is_empty() && !key.contains(' ') && is_secret_key(key) {
            return format!("{indent}{}: {REDACTED}{nl}", k.trim());
        }
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_keys() {
        for k in [
            "DB_PASSWORD",
            "POSTGRES_PASSWORD",
            "api_key",
            "JWT_SECRET",
            "TOKEN",
            "MY_PRIVATE_KEY",
        ] {
            assert!(is_secret_key(k), "{k}");
        }
        for k in [
            "PUID",
            "TZ",
            "DB_USERNAME",
            "POSTGRES_DB",
            "port_map",
            "KEYBOARD_LAYOUT",
            "author",
        ] {
            assert!(!is_secret_key(k), "{k}");
        }
    }

    #[test]
    fn redacts_mapping_and_list_forms() {
        let input = "services:\n  db:\n    environment:\n      POSTGRES_PASSWORD: hunter2\n      POSTGRES_USER: postgres\n  app:\n    environment:\n      - DB_PASSWORD=hunter2\n      - \"API_KEY=abc\"\n      - TZ=UTC\n";
        let out = redact_compose_text(input);
        assert!(!out.contains("hunter2"), "{out}");
        assert!(!out.contains("abc"), "{out}");
        assert!(out.contains("POSTGRES_PASSWORD: ***"));
        assert!(out.contains("- DB_PASSWORD=***"));
        assert!(out.contains("POSTGRES_USER: postgres"));
        assert!(out.contains("- TZ=UTC"));
    }

    #[test]
    fn leaves_section_headers_alone() {
        let input = "x-secrets:\n  token_file: /run/x\n";
        // "token_file" holds a path, but we still hide it: conservative by design.
        let out = redact_compose_text(input);
        assert!(out.starts_with("x-secrets:\n"));
    }
}
