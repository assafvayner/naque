//! Ephemeral display state for an in-flight turn (the pinned action line).
//!
//! Updated only by draining [`AgentEvent`]s; `apply` is a pure reducer so it is
//! unit-testable without a terminal.

use naque_llm::{AgentEvent, Usage};

/// State backing the pinned action line + spinner while a turn runs.
#[derive(Debug, Default)]
pub struct LiveState {
    pub running: bool,
    pub current_action: Option<String>,
    pub spinner_frame: usize,
    pub iteration: u32,
    pub max_iterations: u32,
    pub live_usage: Usage,
    /// Transcript scroll: rows scrolled up from the bottom (0 = following tail).
    pub scroll_offset: u16,
    pub follow: bool,
    /// New transcript rows arrived while paused (for the "↓ N new" hint).
    pub new_below: u16,
    /// True while a turn is paused waiting for the user to approve a query.
    pub awaiting_approval: bool,
}

impl LiveState {
    pub fn new(max_iterations: u32) -> Self {
        Self {
            follow: true,
            max_iterations,
            ..Default::default()
        }
    }

    /// Advance the spinner one frame (called on the UI tick).
    pub fn tick(&mut self) {
        if self.running {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
        }
    }

    /// Scroll the transcript up (toward older entries) by `lines` rows, pausing
    /// tail-follow. No content-length clamp: over-scrolling just shows blank
    /// space, which `render` absorbs via `saturating_sub`.
    pub fn scroll_up(&mut self, lines: u16) {
        self.follow = false;
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    /// Scroll the transcript down (toward newer entries) by `lines` rows. On
    /// reaching the tail (offset 0), resume following and clear the new-rows hint.
    pub fn scroll_down(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if self.scroll_offset == 0 {
            self.scroll_to_latest();
        }
    }

    /// Jump straight to the latest entry and resume following.
    pub fn scroll_to_latest(&mut self) {
        self.scroll_offset = 0;
        self.follow = true;
        self.new_below = 0;
    }

    /// Fold one agent event into the live state.
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnStarted => {
                self.running = true;
                self.current_action = Some("thinking".into());
                self.iteration = 0;
                self.awaiting_approval = false;
            },
            AgentEvent::LlmCallStarted { iteration } => {
                self.iteration = *iteration;
                self.current_action = Some("thinking".into());
                self.awaiting_approval = false;
            },
            AgentEvent::ToolCallStarted { name, .. } => {
                self.current_action = Some(name.clone());
            },
            AgentEvent::ToolCallFinished { .. } => {},
            AgentEvent::TextDelta(_) => {},
            AgentEvent::UsageUpdated(u) => {
                self.live_usage = u.clone();
            },
            AgentEvent::TurnFinished { .. } | AgentEvent::Cancelled => {
                self.running = false;
                self.current_action = None;
                self.awaiting_approval = false;
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_lifecycle_sets_and_clears_running() {
        let mut s = LiveState::new(12);
        assert!(!s.running);
        s.apply(&AgentEvent::TurnStarted);
        assert!(s.running);
        s.apply(&AgentEvent::LlmCallStarted { iteration: 1 });
        assert_eq!(s.iteration, 1);
        s.apply(&AgentEvent::ToolCallStarted {
            name: "run_query".into(),
            detail: Some("SELECT 1".into()),
        });
        assert_eq!(s.current_action.as_deref(), Some("run_query"));
        s.apply(&AgentEvent::UsageUpdated(Usage {
            input_tokens: 10,
            output_tokens: 5,
        }));
        assert_eq!(s.live_usage.input_tokens + s.live_usage.output_tokens, 15);
        s.apply(&AgentEvent::TurnFinished {
            iterations: 2,
            hit_iteration_cap: false,
        });
        assert!(!s.running);
        assert!(s.current_action.is_none());
    }

    #[test]
    fn tick_advances_only_while_running() {
        let mut s = LiveState::new(12);
        s.tick();
        assert_eq!(s.spinner_frame, 0); // not running
        s.apply(&AgentEvent::TurnStarted);
        s.tick();
        assert_eq!(s.spinner_frame, 1);
    }

    #[test]
    fn cancelled_clears_running() {
        let mut s = LiveState::new(12);
        s.apply(&AgentEvent::TurnStarted);
        s.apply(&AgentEvent::Cancelled);
        assert!(!s.running);
    }

    #[test]
    fn scroll_up_pauses_follow_and_raises_offset() {
        let mut s = LiveState::new(12);
        assert!(s.follow);
        s.scroll_up(5);
        assert!(!s.follow);
        assert_eq!(s.scroll_offset, 5);
        s.scroll_up(3);
        assert_eq!(s.scroll_offset, 8);
    }

    #[test]
    fn scroll_down_saturates_and_restores_follow_at_tail() {
        let mut s = LiveState::new(12);
        s.scroll_up(5);
        s.new_below = 4;
        s.scroll_down(2);
        assert_eq!(s.scroll_offset, 3);
        assert!(!s.follow, "still scrolled up, must not follow");
        assert_eq!(s.new_below, 4, "hint stays until the tail is reached");
        s.scroll_down(10); // saturates at 0
        assert_eq!(s.scroll_offset, 0);
        assert!(s.follow);
        assert_eq!(s.new_below, 0);
    }

    #[test]
    fn scroll_to_latest_resets_to_tail() {
        let mut s = LiveState::new(12);
        s.scroll_up(7);
        s.new_below = 3;
        s.scroll_to_latest();
        assert_eq!(s.scroll_offset, 0);
        assert!(s.follow);
        assert_eq!(s.new_below, 0);
    }
}
