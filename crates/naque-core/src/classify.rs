//! Result types describing how a SQL statement was classified.
//! These are produced by `naque-sql` and consumed by the permission gate.

/// Broad category of a single SQL statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    /// SELECT / WITH-SELECT / VALUES / SHOW / EXPLAIN (no analyze) / COPY ... TO.
    ///
    /// Usually read-only, but a row-locking SELECT (`FOR UPDATE` / `FOR SHARE`)
    /// is `Read` yet NOT read-only: it acquires locks for a later write, so the
    /// producer sets `is_read_only = false` in that case.
    Read,
    /// INSERT / UPDATE / DELETE / MERGE / COPY ... FROM.
    Write,
    /// CREATE / ALTER / DROP / TRUNCATE.
    Ddl,
    /// BEGIN / COMMIT / ROLLBACK / SAVEPOINT.
    ///
    /// Makes no data change itself, so the expected `is_read_only = true`.
    Transaction,
    /// SET / RESET — session settings, no data change.
    ///
    /// Makes no data change itself, so the expected `is_read_only = true`.
    Set,
    /// Could not be confidently classified (includes parse failures).
    Unknown,
}

/// Why a statement is considered catastrophic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatastrophicReason {
    DropObject,
    Truncate,
    DeleteWithoutWhere,
    UpdateWithoutWhere,
}

impl CatastrophicReason {
    pub fn human(&self) -> &'static str {
        match self {
            Self::DropObject => "DROP",
            Self::Truncate => "TRUNCATE",
            Self::DeleteWithoutWhere => "DELETE without WHERE",
            Self::UpdateWithoutWhere => "UPDATE without WHERE",
        }
    }
}

/// Classification of one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementClass {
    pub kind: StatementKind,
    /// The AUTHORITATIVE gate signal, intentionally independent of `kind`.
    ///
    /// Must NEVER be derived from `kind`: some `Read`-kind statements (e.g.
    /// `SELECT ... FOR UPDATE`) are NOT read-only because they lock rows for a
    /// later write. Producers must set this explicitly via the centralized
    /// constructors in `naque-sql`.
    pub is_read_only: bool,
    pub catastrophic: Option<CatastrophicReason>,
    /// Short label for the approval UI, e.g. "read-only", "WRITE", "DDL: DROP".
    pub label: String,
}

impl StatementClass {
    /// Fail-safe constructor: an unclassifiable statement is a non-read-only
    /// `Unknown` (so the gate treats it as a write).
    pub fn unknown(label: impl Into<String>) -> Self {
        Self {
            kind: StatementKind::Unknown,
            is_read_only: false,
            catastrophic: None,
            label: label.into(),
        }
    }

    pub fn is_catastrophic(&self) -> bool {
        self.catastrophic.is_some()
    }
}

/// Classification of a full input string, which may contain several statements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifyResult {
    pub statements: Vec<StatementClass>,
}

impl ClassifyResult {
    /// The whole batch is read-only only if every statement is read-only.
    /// An empty batch is **not** read-only (fail safe).
    pub fn is_read_only(&self) -> bool {
        !self.statements.is_empty() && self.statements.iter().all(|s| s.is_read_only)
    }

    /// True if any statement is catastrophic.
    pub fn any_catastrophic(&self) -> bool {
        self.statements.iter().any(|s| s.is_catastrophic())
    }

    /// First catastrophic reason found, if any (for the guard message).
    pub fn first_catastrophic(&self) -> Option<CatastrophicReason> {
        self.statements.iter().find_map(|s| s.catastrophic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_fail_safe() {
        let c = StatementClass::unknown("unparseable");
        assert_eq!(c.kind, StatementKind::Unknown);
        assert!(!c.is_read_only);
        assert!(!c.is_catastrophic());
    }

    #[test]
    fn empty_batch_is_not_read_only() {
        let r = ClassifyResult { statements: vec![] };
        assert!(!r.is_read_only());
        assert!(!r.any_catastrophic());
    }

    #[test]
    fn batch_read_only_requires_all_read_only() {
        let read = StatementClass {
            kind: StatementKind::Read,
            is_read_only: true,
            catastrophic: None,
            label: "read-only".into(),
        };
        let write = StatementClass {
            kind: StatementKind::Write,
            is_read_only: false,
            catastrophic: None,
            label: "WRITE".into(),
        };
        assert!(ClassifyResult {
            statements: vec![read.clone()]
        }
        .is_read_only());
        assert!(!ClassifyResult {
            statements: vec![read, write]
        }
        .is_read_only());
    }

    #[test]
    fn surfaces_first_catastrophic_reason() {
        let cat = StatementClass {
            kind: StatementKind::Ddl,
            is_read_only: false,
            catastrophic: Some(CatastrophicReason::Truncate),
            label: "DDL: TRUNCATE".into(),
        };
        let r = ClassifyResult { statements: vec![cat] };
        assert!(r.any_catastrophic());
        assert_eq!(r.first_catastrophic(), Some(CatastrophicReason::Truncate));
    }
}
