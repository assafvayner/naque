//! Agent execution events and the observer that receives them.
//!
//! `run_turn` reports its progress as a stream of [`AgentEvent`]s to an
//! [`AgentObserver`]. The binary forwards them to the TUI; tests record them.

use crate::Usage;

/// One observable step in an agent turn, in emission order.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// The turn has begun (emitted once, before the first provider call).
    TurnStarted,
    /// A provider round-trip is starting. `iteration` is 1-based.
    LlmCallStarted { iteration: u32 },
    /// A fragment of model-generated text (streamed; may arrive many times).
    TextDelta(String),
    /// The agent is about to execute a tool call.
    ToolCallStarted { name: String, sql: Option<String> },
    /// A tool call finished. `summary` is a one-line digest of the result.
    ToolCallFinished {
        name: String,
        summary: String,
        is_error: bool,
    },
    /// Cumulative token usage so far this turn.
    UsageUpdated(Usage),
    /// The turn finished normally (final answer produced or iteration cap hit).
    TurnFinished { iterations: u32, hit_iteration_cap: bool },
    /// The turn was cancelled before completing.
    Cancelled,
}

/// Receives [`AgentEvent`]s as a turn progresses.
pub trait AgentObserver: Send {
    fn on_event(&mut self, event: AgentEvent);
}

/// No-op observer for non-UI callers (tests, the `drive` harness).
impl AgentObserver for () {
    fn on_event(&mut self, _event: AgentEvent) {}
}

/// Test observer that records every event in order.
#[derive(Default)]
pub struct RecordingObserver {
    pub events: Vec<AgentEvent>,
}

impl AgentObserver for RecordingObserver {
    fn on_event(&mut self, event: AgentEvent) {
        self.events.push(event);
    }
}

/// One-line digest of a tool result string for the collapsed step display.
///
/// Recognizes the shapes produced by the executor's `format_result_text`:
/// `"N row(s) affected"`, an empty `"(0 rows)"`/`"(no rows)"`, or a
/// header/separator/rows table (counted as `lines - 2`). Errors and anything
/// else fall back to a trimmed, length-capped first line.
pub fn summarize_tool_result(result: &str, is_error: bool) -> String {
    const MAX: usize = 80;

    let cap = |s: &str| -> String {
        if s.chars().count() > MAX {
            let mut t: String = s.chars().take(MAX - 1).collect();
            t.push('…');
            t
        } else {
            s.to_string()
        }
    };

    if is_error {
        return cap(result.lines().next().unwrap_or("").trim());
    }
    let trimmed = result.trim_end();
    if let Some(first) = trimmed.lines().next()
        && first.contains("row(s) affected")
    {
        return cap(first.trim());
    }
    if trimmed.ends_with("(0 rows)") || trimmed == "(no rows)" {
        return "0 rows".to_string();
    }
    // Table shape: header + separator + N data rows.
    let line_count = trimmed.lines().count();
    if line_count >= 2 {
        return format!("{} rows", line_count - 2);
    }
    cap(trimmed.lines().next().unwrap_or("").trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_observer_collects_in_order() {
        let mut obs = RecordingObserver::default();
        obs.on_event(AgentEvent::TurnStarted);
        obs.on_event(AgentEvent::TextDelta("hi".into()));
        assert_eq!(obs.events, vec![AgentEvent::TurnStarted, AgentEvent::TextDelta("hi".into())]);
    }

    #[test]
    fn noop_observer_compiles_and_ignores() {
        let mut unit = ();
        unit.on_event(AgentEvent::Cancelled); // must not panic
    }

    #[test]
    fn summarize_rows_affected() {
        assert_eq!(summarize_tool_result("3 row(s) affected", false), "3 row(s) affected");
    }

    #[test]
    fn summarize_select_counts_data_rows() {
        // header + separator + 2 data rows  ->  "2 rows"
        let s = "id | name\n---------\n1 | a\n2 | b\n";
        assert_eq!(summarize_tool_result(s, false), "2 rows");
    }

    #[test]
    fn summarize_zero_rows() {
        assert_eq!(summarize_tool_result("id\n--\n(0 rows)\n", false), "0 rows");
    }

    #[test]
    fn summarize_error_passes_message_trimmed() {
        let s = "error: no such table: foo";
        assert_eq!(summarize_tool_result(s, true), "error: no such table: foo");
    }

    #[test]
    fn summarize_caps_long_lines() {
        let long = "x".repeat(200);
        let out = summarize_tool_result(&long, true);
        assert!(out.chars().count() <= 80, "summary must be capped: {}", out.len());
        assert!(out.ends_with('…'));
    }
}
