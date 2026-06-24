//! Dependency-free domain types shared across naque crates.

pub mod classify;
pub mod permission;

pub use classify::{CatastrophicReason, ClassifyResult, StatementClass, StatementKind};
pub use permission::{ParsePermissionModeError, PermissionMode};
