//! Secrets abstraction: environment variables and keyring lookups.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// In-process cache of keyring reads. macOS prompts on **every**
/// `Entry::get_password` call (unless the user clicked "Always Allow"), so the
/// connect flow — which probes the keyring several times during provider
/// resolution and again when building the provider — would re-prompt on each
/// call. Caching the first successful read per account makes the prompt fire at
/// most once per account per session. Writes/deletes keep the cache in sync.
fn keyring_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Source of secret values: environment variables and system keyring.
///
/// Abstracting over a trait allows tests to inject a fake implementation
/// without touching the real environment or system keyring.
pub trait Secrets {
    /// Look up an environment variable by name.
    fn env(&self, var: &str) -> Option<String>;

    /// Look up a keyring entry by account name (service is always `"naque"`).
    fn keyring(&self, account: &str) -> Option<String>;
}

/// Production implementation: `std::env::var` + the `keyring` crate.
pub struct SystemSecrets;

impl Secrets for SystemSecrets {
    fn env(&self, var: &str) -> Option<String> {
        std::env::var(var).ok()
    }

    fn keyring(&self, account: &str) -> Option<String> {
        if let Some(v) = keyring_cache().lock().unwrap().get(account).cloned() {
            return Some(v);
        }
        let entry = keyring::Entry::new("naque", account).ok()?;
        entry.get_password().ok().inspect(|v| {
            keyring_cache().lock().unwrap().insert(account.to_string(), v.clone());
        })
    }
}

/// The environment variables that carry each provider's API key, in fallback
/// order. A GUI launch has none of these set, so the keyring is the source of
/// truth there; the TUI continues to honor env vars (they win when present).
pub fn provider_env_vars(provider: &str) -> &'static [&'static str] {
    match provider {
        "claude" | "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "gemini" | "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "hf" | "huggingface" => &["HF_TOKEN"],
        _ => &[],
    }
}

/// The keyring account name used to store a provider's API key (service is
/// always `"naque"`). Returns `None` for unknown providers.
pub fn provider_keyring_account(provider: &str) -> Option<&'static str> {
    match provider {
        "claude" | "anthropic" => Some("anthropic-api-key"),
        "openai" => Some("openai-api-key"),
        "gemini" | "google" => Some("gemini-api-key"),
        "hf" | "huggingface" => Some("hf-token"),
        _ => None,
    }
}

/// Resolve a provider's API key: env var first (TUI / shell launch), then the
/// system keyring (GUI launch, where the shell environment is absent). Returns
/// `None` when neither source has a key for `provider`.
pub fn resolve_api_key(provider: &str, secrets: &dyn Secrets) -> Option<String> {
    for var in provider_env_vars(provider) {
        if let Some(v) = secrets.env(var) {
            return Some(v);
        }
    }
    let account = provider_keyring_account(provider)?;
    secrets.keyring(account)
}

/// Write a provider's API key to the system keyring (service `"naque"`).
pub fn set_api_key(provider: &str, key: &str) -> keyring::Result<()> {
    let account = provider_keyring_account(provider).ok_or(keyring::Error::NoEntry)?;
    let entry = keyring::Entry::new("naque", account)?;
    entry.set_password(key)?;
    keyring_cache().lock().unwrap().insert(account.to_string(), key.to_string());
    Ok(())
}

/// Delete a provider's API key from the system keyring. A missing entry is
/// treated as success so the caller can clear unconditionally.
pub fn clear_api_key(provider: &str) -> keyring::Result<()> {
    let account = provider_keyring_account(provider).ok_or(keyring::Error::NoEntry)?;
    let entry = keyring::Entry::new("naque", account)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {
            keyring_cache().lock().unwrap().remove(account);
            Ok(())
        },
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Real-keychain round-trip + a read of the actual saved token. Run with
    /// `cargo test -p naque-profile live_keyring -- --ignored --nocapture`.
    /// Ignored by default so it never touches CI's keychain.
    #[test]
    #[ignore]
    fn live_keyring_round_trip_and_read() {
        let secrets = SystemSecrets;
        // 1. Round-trip under a scratch account — does the keyring work at all?
        let entry = keyring::Entry::new("naque", "diag-scratch").unwrap();
        let _ = entry.set_password("sentinel");
        let got = secrets.keyring("diag-scratch");
        eprintln!("[diag] scratch round-trip: {got:?}");
        let _ = entry.delete_credential();
        assert_eq!(got.as_deref(), Some("sentinel"));

        // 2. Read the real saved HF token (read-only — does NOT modify it).
        let hf = secrets.keyring("hf-token");
        eprintln!("[diag] hf-token present: {}", hf.is_some());
        if let Some(v) = &hf {
            eprintln!("[diag] hf-token length: {}", v.len());
        }
    }
}
