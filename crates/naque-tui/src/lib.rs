//! TUI components for naque: input grammar router, color theme, and widgets.

pub mod activity;
pub mod approval;
pub mod bytes;
pub mod commands;
pub mod history;
pub mod input;
pub mod input_line;
pub mod markdown;
pub mod picker;
pub mod result_table;
pub mod status_bar;
pub mod suggest;
pub mod theme;

pub use activity::{ActivityLine, SPINNER_FRAMES};
pub use approval::{ApprovalChoice, ApprovalPrompt};
pub use commands::{SLASH_COMMANDS, SlashCommand, help_text, matching};
pub use history::History;
pub use input::{Input, route_input};
pub use input_line::{InputLine, InputView};
pub use markdown::render_markdown;
pub use picker::{Picker, PickerOption, PickerOutcome};
pub use result_table::ResultTable;
pub use status_bar::StatusBar;
pub use suggest::SlashSuggest;
pub use theme::Theme;
