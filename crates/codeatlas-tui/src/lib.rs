//! Terminal presentation layer for `CodeAtlas`.
//!
//! This crate only accepts [`codeatlas_core::AppEvent`] values and emits
//! [`codeatlas_core::AppCommand`] values. It does not inspect repositories,
//! execute tools, call models, or retain credentials.

mod app;
mod highlight;
mod port;
mod render;
mod terminal;

pub use app::{
    Activity, AnswerView, ConversationEntry, EvidenceViewer, HistoryView, InputMode, LayoutMode,
    Panel, ProgressTrace, RepositoryView, RequestKind, ToolTrace, ToolTraceStatus, TuiApp,
    TuiSnapshot, UiError, UiPreferences,
};
pub use port::{ApplicationPort, ChannelApplicationPort, ChannelPortError};
pub use terminal::{TuiError, run_tui};

#[cfg(test)]
mod tests;
