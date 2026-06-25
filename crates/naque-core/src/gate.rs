//! Permission gate: decide how to handle a query given mode, classification,
//! and whether the always-on catastrophic guard is enabled.

use crate::{ClassifyResult, PermissionMode};

/// Whether a query is the agent's internal introspection or the primary query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    /// The agent's internal schema/catalog introspection.
    Introspection,
    /// The primary user-facing query.
    Primary,
}

/// What the permission gate decides for a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Run without prompting.
    AutoApprove,
    /// Ask the user to accept / edit / reject.
    Prompt,
    /// Always-on catastrophic guard: hard confirm even in wildcard.
    PromptCatastrophic,
}

/// Decide how to handle a query given the permission mode, its classification,
/// whether it's introspection vs primary, and whether the always-on
/// catastrophic guard is enabled.
pub fn gate_decision(
    mode: PermissionMode,
    class: &ClassifyResult,
    kind: QueryKind,
    catastrophic_guard: bool,
) -> GateDecision {
    if catastrophic_guard && class.any_catastrophic() {
        return GateDecision::PromptCatastrophic;
    }
    match mode {
        PermissionMode::Wildcard => GateDecision::AutoApprove,
        PermissionMode::Strict => GateDecision::Prompt,
        PermissionMode::Default => match kind {
            QueryKind::Introspection => GateDecision::AutoApprove,
            QueryKind::Primary => GateDecision::Prompt,
        },
        PermissionMode::ReadOnly => {
            if class.is_read_only() {
                GateDecision::AutoApprove
            } else {
                GateDecision::Prompt
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatastrophicReason, StatementClass, StatementKind};

    fn read_class() -> ClassifyResult {
        ClassifyResult {
            statements: vec![StatementClass {
                kind: StatementKind::Read,
                is_read_only: true,
                catastrophic: None,
                label: "read-only".into(),
            }],
        }
    }

    fn write_class() -> ClassifyResult {
        ClassifyResult {
            statements: vec![StatementClass {
                kind: StatementKind::Write,
                is_read_only: false,
                catastrophic: None,
                label: "WRITE".into(),
            }],
        }
    }

    fn catastrophic_class() -> ClassifyResult {
        ClassifyResult {
            statements: vec![StatementClass {
                kind: StatementKind::Ddl,
                is_read_only: false,
                catastrophic: Some(CatastrophicReason::DropObject),
                label: "DDL: DROP".into(),
            }],
        }
    }

    fn catastrophic_write_class() -> ClassifyResult {
        ClassifyResult {
            statements: vec![StatementClass {
                kind: StatementKind::Write,
                is_read_only: false,
                catastrophic: Some(CatastrophicReason::DeleteWithoutWhere),
                label: "DELETE without WHERE".into(),
            }],
        }
    }

    // --- Wildcard mode ---

    #[test]
    fn wildcard_read_primary_auto_approve() {
        assert_eq!(
            gate_decision(PermissionMode::Wildcard, &read_class(), QueryKind::Primary, false),
            GateDecision::AutoApprove
        );
    }

    #[test]
    fn wildcard_catastrophic_guard_on_prompt_catastrophic() {
        assert_eq!(
            gate_decision(PermissionMode::Wildcard, &catastrophic_class(), QueryKind::Primary, true),
            GateDecision::PromptCatastrophic
        );
    }

    #[test]
    fn wildcard_catastrophic_guard_off_auto_approve() {
        assert_eq!(
            gate_decision(PermissionMode::Wildcard, &catastrophic_class(), QueryKind::Primary, false),
            GateDecision::AutoApprove
        );
    }

    // --- Strict mode ---

    #[test]
    fn strict_read_introspection_prompt() {
        assert_eq!(
            gate_decision(PermissionMode::Strict, &read_class(), QueryKind::Introspection, false),
            GateDecision::Prompt
        );
    }

    #[test]
    fn strict_read_primary_prompt() {
        assert_eq!(
            gate_decision(PermissionMode::Strict, &read_class(), QueryKind::Primary, false),
            GateDecision::Prompt
        );
    }

    // --- Default mode ---

    #[test]
    fn default_introspection_auto_approve() {
        assert_eq!(
            gate_decision(PermissionMode::Default, &read_class(), QueryKind::Introspection, false),
            GateDecision::AutoApprove
        );
    }

    #[test]
    fn default_read_primary_prompt() {
        assert_eq!(
            gate_decision(PermissionMode::Default, &read_class(), QueryKind::Primary, false),
            GateDecision::Prompt
        );
    }

    #[test]
    fn default_write_primary_prompt() {
        assert_eq!(
            gate_decision(PermissionMode::Default, &write_class(), QueryKind::Primary, false),
            GateDecision::Prompt
        );
    }

    // --- ReadOnly mode ---

    #[test]
    fn readonly_read_primary_auto_approve() {
        assert_eq!(
            gate_decision(PermissionMode::ReadOnly, &read_class(), QueryKind::Primary, false),
            GateDecision::AutoApprove
        );
    }

    #[test]
    fn readonly_write_primary_prompt() {
        assert_eq!(
            gate_decision(PermissionMode::ReadOnly, &write_class(), QueryKind::Primary, false),
            GateDecision::Prompt
        );
    }

    #[test]
    fn readonly_write_catastrophic_guard_on_prompt_catastrophic() {
        assert_eq!(
            gate_decision(PermissionMode::ReadOnly, &catastrophic_write_class(), QueryKind::Primary, true),
            GateDecision::PromptCatastrophic
        );
    }

    // --- Catastrophic guard takes precedence over Wildcard ---

    #[test]
    fn catastrophic_guard_overrides_wildcard_auto_approve() {
        let class = catastrophic_class();
        assert!(class.any_catastrophic());
        assert_eq!(
            gate_decision(PermissionMode::Wildcard, &class, QueryKind::Primary, true),
            GateDecision::PromptCatastrophic,
            "catastrophic guard must override wildcard auto-approve"
        );
    }
}
