//! TUI components for naque: input grammar router, color theme, and widgets.

pub mod approval;
pub mod input;
pub mod picker;
pub mod result_table;
pub mod status_bar;
pub mod theme;

pub use approval::{ApprovalChoice, ApprovalPrompt};
pub use input::{route_input, Input};
pub use picker::{Picker, PickerOption, PickerOutcome};
pub use result_table::ResultTable;
pub use status_bar::StatusBar;
pub use theme::Theme;
