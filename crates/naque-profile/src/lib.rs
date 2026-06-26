//! Config, profile, and store types for naque.
//!
//! This crate owns the `~/.naque/` layout, TOML (de)serialization for all
//! three naque config files, and the `Store` handle for the central store.
//!
//! # File layout
//!
//! | File | Purpose |
//! |---|---|
//! | `~/.naque/config.toml` | Global defaults + `default_profile` |
//! | `~/.naque/profiles.toml` | Named connection profiles |
//! | `./naque.toml` | Project-local `project` override |
//!
//! All three files share the `NaqueFile` parse type; absent fields are `None`.

mod config;
mod credscan;
mod discovery;
mod error;
mod file;
mod profile;
mod profile_store;
mod resolve;
mod secrets;
mod store;

pub use config::NaqueConfig;
pub use credscan::{project_local_password_warning, strip_url_password};
pub use discovery::find_naque_toml;
pub use error::ConfigError;
pub use file::NaqueFile;
pub use profile::{ConnectionSpec, ProfileBody, ProfileEngine};
pub use profile_store::Profile;
pub use resolve::{Overrides, Resolved, detect_provider, resolve};
pub use secrets::{Secrets, SystemSecrets};
pub use store::Store;
