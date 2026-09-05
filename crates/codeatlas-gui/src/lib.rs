//! Native `egui` presentation layer for `CodeAtlas`.

mod state;
mod ui;

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use codeatlas_core::{AppCommand, AppEvent};

pub use state::{GuiState, WorkspacePanel};

static GUI_INSTANCE: AtomicU64 = AtomicU64::new(0);

/// Runs the native GUI until its window closes.
///
/// Backend events are relayed by a small bridge thread so event arrival wakes
/// the native window. The GUI thread only reduces already-produced events and
/// sends presentation-neutral commands.
///
/// # Errors
///
/// Returns an error if the native window or graphics context cannot be created.
pub fn run_gui(
    commands: mpsc::Sender<AppCommand>,
    events: mpsc::Receiver<AppEvent>,
    initial_repository: Option<String>,
) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1_420.0, 860.0])
            .with_min_inner_size([540.0, 480.0]),
        ..eframe::NativeOptions::default()
    };
    let namespace = fresh_namespace();
    eframe::run_native(
        "CodeAtlas",
        options,
        Box::new(move |context| {
            Ok(Box::new(ui::GuiWindow::new(
                context,
                commands,
                events,
                initial_repository,
                namespace,
            )))
        }),
    )
}

fn fresh_namespace() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let instance = GUI_INSTANCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{now}-{instance}", std::process::id())
}
