//! TUI components for naque: input grammar router, color theme, and widgets.

pub mod activity;
pub mod approval;
pub mod input;
pub mod input_line;
pub mod picker;
pub mod result_table;
pub mod status_bar;
pub mod theme;

pub use activity::{ActivityLine, SPINNER_FRAMES};
pub use approval::{ApprovalChoice, ApprovalPrompt};
pub use input::{Input, route_input};
pub use input_line::{InputLine, InputView};
pub use picker::{Picker, PickerOption, PickerOutcome};
pub use result_table::ResultTable;
pub use status_bar::StatusBar;
pub use theme::Theme;
