//! Credential-hygiene helpers: detect plaintext passwords in project-local
//! config, and strip a password out of a connection URL.

/// Returns a warning string if `toml_text` (the contents of a project-local
/// `naque.toml`) contains a plaintext password — either an inline `password =`
/// key, or a password embedded in a connection `url`. `path` names the file in
/// the message. `None` when no plaintext credential is present.
pub fn project_local_password_warning(path: &str, toml_text: &str) -> Option<String> {
    let has_inline = toml_text.lines().map(str::trim_start).any(|l| {
        let key = l.split('=').next().map(str::trim).unwrap_or("");
        key == "password"
    });
    let has_url_password = toml_text
        .lines()
        .filter(|l| l.trim_start().starts_with("url"))
        .any(url_has_password);
    if has_inline || has_url_password {
        Some(format!("plaintext credential in {path} — do not commit it; add it to .gitignore"))
    } else {
        None
    }
}

/// Strip the password from a connection URL's authority (`user:pass@` → `user@`).
/// Returns the redacted URL and whether a password was present.
pub fn strip_url_password(url: &str) -> (String, bool) {
    // Authority is between "://" and the first '/' after it.
    let Some(scheme_end) = url.find("://") else {
        return (url.to_string(), false);
    };
    let after = scheme_end + 3;
    let auth_end = url[after..].find('/').map(|i| after + i).unwrap_or(url.len());
    let authority = &url[after..auth_end];
    let Some(at) = authority.rfind('@') else {
        return (url.to_string(), false);
    };
    let userinfo = &authority[..at];
    match userinfo.find(':') {
        Some(colon) => {
            let user = &userinfo[..colon];
            let redacted = format!("{}{}@{}", &url[..after], user, &authority[at + 1..]);
            (format!("{redacted}{}", &url[auth_end..]), true)
        },
        None => (url.to_string(), false),
    }
}

fn url_has_password(line: &str) -> bool {
    // crude: a quoted URL value with `user:pass@`.
    line.split('"').any(|seg| strip_url_password(seg).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_inline_password() {
        let w = project_local_password_warning("./naque.toml", "host = \"h\"\npassword = \"s\"\n");
        assert!(w.unwrap().contains("./naque.toml"));
    }

    #[test]
    fn flags_url_password() {
        let w = project_local_password_warning("./naque.toml", "url = \"postgres://u:p@h/db\"\n");
        assert!(w.is_some());
    }

    #[test]
    fn no_warning_for_reference_only() {
        let text = "host = \"h\"\npassword_env = \"PW\"\n";
        assert!(project_local_password_warning("./naque.toml", text).is_none());
    }

    #[test]
    fn strips_password_keeping_user() {
        let (red, had) = strip_url_password("postgres://user:secret@host:5432/db");
        assert_eq!(red, "postgres://user@host:5432/db");
        assert!(had);
    }

    #[test]
    fn strip_noop_when_no_password() {
        let (red, had) = strip_url_password("postgres://user@host/db");
        assert_eq!(red, "postgres://user@host/db");
        assert!(!had);
    }

    #[test]
    fn strip_noop_for_sqlite() {
        let (red, had) = strip_url_password("sqlite:///tmp/x.db");
        assert_eq!(red, "sqlite:///tmp/x.db");
        assert!(!had);
    }
}
