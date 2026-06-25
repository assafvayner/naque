//! Secrets abstraction: environment variables and keyring lookups.

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
        let entry = keyring::Entry::new("naque", account).ok()?;
        entry.get_password().ok()
    }
}
