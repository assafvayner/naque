//! Dependency-free domain types shared across naque crates.

pub mod classify;
pub mod gate;
pub mod permission;

pub use classify::{CatastrophicReason, ClassifyResult, StatementClass, StatementKind};
pub use gate::{GateDecision, QueryKind, gate_decision};
pub use permission::{ParsePermissionModeError, PermissionMode};
