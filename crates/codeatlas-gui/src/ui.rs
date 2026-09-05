use std::{
    fmt::Write as _,
    fs,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use codeatlas_core::{
    AppCommand, AppEvent, ClaimKind, DiagramDecision, DiagramKind, EntryPointKind, Language,
    ModelUsage,
};
use eframe::egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use eframe::egui::{
    self, Align, Color32, FontData, FontFamily, FontId, Frame, Key, KeyboardShortcut, Layout,
    Margin, Modifiers, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, Vec2,
};

use crate::state::{
    ActivityStatus, GuiState, SourceStatus, TurnStatus, WorkStatus, WorkspacePanel, call_target,
    short_id,
};

const MAX_EVENTS_PER_FRAME: usize = 96;
const EVENT_QUEUE_CAPACITY: usize = 64;
const PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_millis(40);
const BG: Color32 = Color32::from_rgb(12, 17, 22);
const SURFACE: Color32 = Color32::from_rgb(18, 25, 32);
const SURFACE_HIGH: Color32 = Color32::from_rgb(24, 33, 42);
const BORDER: Color32 = Color32::from_rgb(45, 59, 70);
const TEXT: Color32 = Color32::from_rgb(217, 226, 232);
const MUTED: Color32 = Color32::from_rgb(133, 151, 163);
const CYAN: Color32 = Color32::from_rgb(66, 205, 220);
const FACT: Color32 = Color32::from_rgb(100, 205, 143);
const INFERENCE: Color32 = Color32::from_rgb(225, 190, 92);
const UNKNOWN: Color32 = Color32::from_rgb(171, 128, 224);
const ERROR: Color32 = Color32::from_rgb(232, 102, 108);
const NARROW_WORKSPACE_WIDTH: f32 = 980.0;
const REPOSITORY_PANEL_MIN_WIDTH: f32 = 210.0;
const REPOSITORY_PANEL_MAX_WIDTH: f32 = 340.0;
const EVIDENCE_PANEL_MIN_WIDTH: f32 = 300.0;
const EVIDENCE_PANEL_MAX_WIDTH: f32 = 480.0;
const CONVERSATION_PANEL_MIN_WIDTH: f32 = 440.0;

pub(crate) struct GuiWindow {
    state: GuiState,
    commands: mpsc::Sender<AppCommand>,
    events: mpsc::Receiver<AppEvent>,
    directory_results: mpsc::Receiver<Option<PathBuf>>,
    directory_sender: mpsc::Sender<Option<PathBuf>>,
    directory_pending: bool,
    question_ime_active: bool,
    repository_editor_open: bool,
}

impl GuiWindow {
    pub(crate) fn new(
        context: &eframe::CreationContext<'_>,
        commands: mpsc::Sender<AppCommand>,
        backend_events: mpsc::Receiver<AppEvent>,
        initial_repository: Option<String>,
        namespace: String,
    ) -> Self {
        configure_style(&context.egui_ctx);
        let (event_sender, events) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let repaint = context.egui_ctx.clone();
        thread::Builder::new()
            .name("codeatlas-gui-events".to_owned())
            .spawn(move || relay_backend_events(&backend_events, &event_sender, &repaint))
            .expect("failed to start GUI event bridge");
        let (directory_sender, directory_results) = mpsc::channel();
        let mut state = GuiState::new(namespace);
        state.repository_input = initial_repository.unwrap_or_default();
        let mut window = Self {
            state,
            commands,
            events,
            directory_results,
            directory_sender,
            directory_pending: false,
            question_ime_active: false,
            repository_editor_open: false,
        };
        if !window.state.repository_input.trim().is_empty() {
            let command = window.state.index_repository();
            window.send_optional(command);
        }
        window
    }

    fn drain_events(&mut self, context: &egui::Context) {
        for _ in 0..MAX_EVENTS_PER_FRAME {
            match self.events.try_recv() {
                Ok(event) => self.state.reduce(event),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return,
            }
        }
        context.request_repaint();
    }

    fn drain_directory_result(&mut self) {
        let Ok(path) = self.directory_results.try_recv() else {
            return;
        };
        self.directory_pending = false;
        if let Some(path) = path {
            self.state.repository_input = path.to_string_lossy().into_owned();
        }
    }

    fn choose_directory(&mut self, context: &egui::Context) {
        if self.directory_pending {
            return;
        }
        self.directory_pending = true;
        let sender = self.directory_sender.clone();
        let repaint = context.clone();
        thread::Builder::new()
            .name("codeatlas-directory-picker".to_owned())
            .spawn(move || {
                let selected = rfd::FileDialog::new()
                    .set_title("Choose a Rust or Python repository")
                    .pick_folder();
                let _ = sender.send(selected);
                repaint.request_repaint();
            })
            .expect("failed to start directory picker");
    }

    fn send(&mut self, command: AppCommand) {
        if self.commands.send(command).is_err() {
            self.state.local_error(
                "application_disconnected",
                "The CodeAtlas background service stopped. Restart the application to continue.",
            );
        }
    }

    fn send_optional(&mut self, command: Option<AppCommand>) {
        if let Some(command) = command {
            self.send(command);
        }
    }

    fn top_bar(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("top_bar")
            .frame(
                Frame::new()
                    .fill(SURFACE)
                    .inner_margin(Margin::symmetric(18, 10)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("CODEATLAS")
                            .font(FontId::proportional(17.0))
                            .strong()
                            .color(CYAN),
                    );
                    ui.add_space(16.0);
                    if let Some(repository) = &self.state.repository {
                        ui.label(RichText::new(&repository.map.name).strong().color(TEXT));
                        ui.label(RichText::new(&repository.path).small().color(MUTED));
                    } else {
                        ui.label(RichText::new("No repository indexed").color(MUTED));
                    }
                    ui.add_space(8.0);
                    status_pill(ui, self.state.status);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("New session").clicked() {
                            self.state.new_session();
                        }
                        if ui.button("History").clicked() {
                            let command = self.state.list_sessions();
                            self.send(command);
                        }
                        if ui.button("Repository").clicked() {
                            self.repository_editor_open = true;
                        }
                    });
                });
            });
    }

    fn banners(&mut self, ui: &mut Ui) {
        if let Some(message) = self.state.read_only_message() {
            banner(ui, UNKNOWN, "READ-ONLY SESSION", message);
        }
        if let Some(error) = &self.state.error {
            let retry = if error.retryable {
                " You can retry."
            } else {
                ""
            };
            banner(
                ui,
                ERROR,
                &format!("ERROR | {}", error.code),
                &format!("{}{retry}", error.message),
            );
        }
        for notice in &self.state.notices {
            banner(
                ui,
                INFERENCE,
                "ANSWER NOTICE",
                &format!("{}: {}", notice.code, notice.message),
            );
        }
    }

    fn welcome(&mut self, context: &egui::Context) {
        self.activity_panel(context);
        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG))
            .show(context, |ui| {
                ui.add_space((ui.available_height() * 0.14).max(28.0));
                ui.vertical_centered(|ui| {
                    ui.set_max_width(680.0);
                    ui.label(
                        RichText::new("Understand the code before changing it.")
                            .font(FontId::proportional(32.0))
                            .strong()
                            .color(TEXT),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(
                            "CodeAtlas builds an evidence-backed mental model of an unfamiliar repository. It is a research workbench, not an IDE.",
                        )
                        .size(16.0)
                        .color(MUTED),
                    );
                    ui.add_space(30.0);
                    Frame::new()
                        .fill(SURFACE)
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(8)
                        .inner_margin(24)
                        .show(ui, |ui| {
                            ui.set_width(610.0_f32.min(ui.available_width()));
                            ui.label(RichText::new("Repository directory").strong().color(TEXT));
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [ui.available_width() - 112.0, 32.0],
                                    TextEdit::singleline(&mut self.state.repository_input)
                                        .hint_text("/path/to/repository"),
                                );
                                let label = if self.directory_pending {
                                    "Choosing..."
                                } else {
                                    "Choose folder"
                                };
                                if ui
                                    .add_enabled(!self.directory_pending, egui::Button::new(label))
                                    .clicked()
                                {
                                    self.choose_directory(context);
                                }
                            });
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Supported now").small().color(MUTED));
                                language_badge(ui, "RUST");
                                language_badge(ui, "PYTHON");
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let enabled = !self.state.is_busy();
                                    if ui
                                        .add_enabled(enabled, egui::Button::new("Index repository"))
                                        .clicked()
                                    {
                                        let command = self.state.index_repository();
                                        self.send_optional(command);
                                    }
                                });
                            });
                            if self.state.status == WorkStatus::Indexing {
                                ui.add_space(12.0);
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(
                                        RichText::new("Building a read-only code map...")
                                            .color(CYAN),
                                    );
                                });
                            }
                            self.banners(ui);
                        });
                });
            });
    }

    fn workspace(&mut self, context: &egui::Context) {
        self.activity_panel(context);
        let viewport_width = context.screen_rect().width();
        let narrow = viewport_width < NARROW_WORKSPACE_WIDTH;
        if narrow {
            egui::TopBottomPanel::top("workspace_tabs")
                .frame(Frame::new().fill(BG).inner_margin(Margin::symmetric(12, 8)))
                .show(context, |ui| {
                    ui.horizontal(|ui| {
                        panel_tab(
                            ui,
                            &mut self.state.panel,
                            WorkspacePanel::Repository,
                            "Repository",
                        );
                        panel_tab(
                            ui,
                            &mut self.state.panel,
                            WorkspacePanel::Conversation,
                            "Conversation",
                        );
                        panel_tab(
                            ui,
                            &mut self.state.panel,
                            WorkspacePanel::Evidence,
                            "Evidence",
                        );
                    });
                });
            egui::CentralPanel::default()
                .frame(Frame::new().fill(BG).inner_margin(12))
                .show(context, |ui| {
                    self.banners(ui);
                    match self.state.panel {
                        WorkspacePanel::Repository => self.repository_panel(ui),
                        WorkspacePanel::Conversation => self.conversation_panel(ui),
                        WorkspacePanel::Evidence => self.evidence_panel(ui),
                    }
                });
        } else {
            let (repository_max_width, evidence_max_width) =
                workspace_side_panel_max_widths(viewport_width);
            egui::SidePanel::left("repository_panel")
                .resizable(true)
                .default_width(255.0)
                .width_range(REPOSITORY_PANEL_MIN_WIDTH..=repository_max_width)
                .frame(panel_frame())
                .show(context, |ui| self.repository_panel(ui));
            egui::SidePanel::right("evidence_panel")
                .resizable(true)
                .default_width(355.0)
                .width_range(EVIDENCE_PANEL_MIN_WIDTH..=evidence_max_width)
                .frame(panel_frame())
                .show(context, |ui| self.evidence_panel(ui));
            egui::CentralPanel::default()
                .frame(Frame::new().fill(BG).inner_margin(18))
                .show(context, |ui| {
                    self.banners(ui);
                    self.conversation_panel(ui);
                });
        }
    }

    fn repository_panel(&mut self, ui: &mut Ui) {
        section_title(
            ui,
            "REPOSITORY OVERVIEW",
            "A bounded map of structure and entry points",
        );
        let Some(repository) = &self.state.repository else {
            ui.label(RichText::new("No repository map yet.").color(MUTED));
            return;
        };
        ScrollArea::vertical().show(ui, |ui| {
            ui.label(
                RichText::new(format!(
                    "{} files  |  {} symbols",
                    repository.file_count, repository.symbol_count
                ))
                .color(CYAN),
            );
            ui.label(
                RichText::new(format!(
                    "{} calls  |  {} unresolved",
                    repository.map.call_count, repository.map.unresolved_call_count
                ))
                .color(MUTED),
            );
            ui.add_space(18.0);
            small_heading(ui, "LANGUAGES");
            for language in &repository.map.languages {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(language_name(&language.language)).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(language.file_count.to_string()).color(MUTED));
                    });
                });
            }
            ui.add_space(18.0);
            small_heading(ui, "MODULES");
            ui.label(
                RichText::new(format!("{} discovered", repository.map.module_count)).color(MUTED),
            );
            for module in &repository.map.modules {
                ui.add_space(6.0);
                ui.label(RichText::new(&module.name).strong().color(TEXT));
                ui.label(RichText::new(module.path.as_str()).small().color(MUTED));
            }
            if repository.map.modules_truncated {
                ui.label(
                    RichText::new("Map intentionally abbreviated")
                        .italics()
                        .color(MUTED),
                );
            }
            ui.add_space(18.0);
            small_heading(ui, "ENTRY POINTS");
            for entry in &repository.map.entry_points {
                Frame::new()
                    .fill(SURFACE_HIGH)
                    .corner_radius(4)
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&entry.label).strong().color(TEXT));
                        ui.label(
                            RichText::new(format!(
                                "{}  |  {}:{}",
                                entry_kind(&entry.kind),
                                entry.path.as_str(),
                                entry.line
                            ))
                            .small()
                            .color(MUTED),
                        );
                    });
                ui.add_space(4.0);
            }
        });
    }

    fn conversation_panel(&mut self, ui: &mut Ui) {
        section_title(
            ui,
            "CONVERSATION",
            "Questions and evidence-backed explanations",
        );
        let selected_usage = self.state.selected_usage().cloned();
        if let Some(usage) = &selected_usage {
            render_usage_summary(ui, usage);
            ui.add_space(8.0);
        }
        let input_height = 102.0;
        let conversation_height = (ui.available_height() - input_height).max(64.0);
        conversation_scroll_area(conversation_height).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if self.state.turns.is_empty() {
                    ui.add_space(36.0);
                    wrapped_label(
                        ui,
                        RichText::new("Ask for a mental model, a request flow, or the evidence behind a behavior.")
                            .size(17.0)
                            .color(MUTED),
                    );
                }
                let turns = self.state.turns.clone();
                for turn in turns {
                    let selected = self.state.selected_task == Some(turn.request_id);
                    let diagram_message = turn.answer.as_ref().and_then(|answer| {
                        let diagram_id = match &answer.diagram {
                            DiagramDecision::Needed { diagram, .. } => {
                                diagram.artifact.as_ref().map(|artifact| artifact.id)
                            }
                            DiagramDecision::NotNeeded { .. } => None,
                        };
                        diagram_id.and_then(|id| {
                            self.state.diagram_message(id).map(str::to_owned)
                        })
                    });
                    let response = Frame::new()
                        .fill(if selected { SURFACE_HIGH } else { SURFACE })
                        .stroke(Stroke::new(1.0, if selected { CYAN } else { BORDER }))
                        .corner_radius(6)
                        .inner_margin(14)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(RichText::new("YOU").small().strong().color(CYAN));
                            wrapped_label(ui, RichText::new(&turn.question).color(TEXT));
                            ui.add_space(12.0);
                            ui.label(RichText::new("CODEATLAS").small().strong().color(MUTED));
                            render_turn_answer(ui, &turn.answer_text, turn.status);
                            turn.answer.as_ref().and_then(|answer| {
                                render_answer_details(ui, answer, diagram_message.as_deref())
                            })
                        });
                    if let Some(diagram_id) = response.inner {
                        let command = self.state.open_diagram(diagram_id);
                        self.send(command);
                    }
                    if response.response.interact(Sense::click()).clicked() {
                        self.state.select_task(turn.request_id);
                    }
                    ui.add_space(10.0);
                }
            });
        ui.separator();
        self.question_composer(ui);
    }

    fn question_composer(&mut self, ui: &mut Ui) {
        ui.add_enabled_ui(self.state.can_ask(), |ui| {
            let input_events = ui.input(|input| input.events.clone());
            ui.horizontal(|ui| {
                let input = ui.add_sized(
                    [(ui.available_width() - 90.0).max(180.0), 62.0],
                    question_text_edit(&mut self.state.question_input),
                );
                let submit_with_enter = question_enter_submits(
                    &input_events,
                    input.has_focus(),
                    &mut self.question_ime_active,
                );
                if input.has_focus() && has_plain_enter(&input_events) {
                    ui.input_mut(|state| {
                        state.events.retain(|event| !is_plain_enter(event));
                    });
                }
                let ask_clicked = ui
                    .add_sized([76.0, 38.0], egui::Button::new("Ask"))
                    .clicked();
                if submit_with_enter || ask_clicked {
                    let command = self.state.ask();
                    self.send_optional(command);
                }
            });
        });
        if !self.state.can_ask() {
            let message = if self.state.is_busy() {
                "A request is already running."
            } else if self.state.is_read_only() {
                "This historical session is read-only."
            } else {
                "Index a repository to ask a question."
            };
            ui.label(RichText::new(message).small().color(MUTED));
        }
    }

    fn evidence_panel(&mut self, ui: &mut Ui) {
        section_title(
            ui,
            "CLAIMS & EVIDENCE",
            "Verified support from the selected answer",
        );
        ui.set_max_width(ui.available_width());
        ScrollArea::vertical()
            .auto_shrink([false, true])
            .show(ui, |ui| self.evidence_contents(ui));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "claims, evidence, and selected source form one cohesive inspector"
    )]
    fn evidence_contents(&mut self, ui: &mut Ui) {
        let claims = self.state.visible_claims().to_vec();
        let mut fact = 0;
        let mut inference = 0;
        let mut unknown = 0;
        for claim in claims {
            let (label, color) = match claim.kind {
                ClaimKind::Fact => {
                    fact += 1;
                    (format!("F{fact}"), FACT)
                }
                ClaimKind::Inference => {
                    inference += 1;
                    (format!("I{inference}"), INFERENCE)
                }
                ClaimKind::Unknown => {
                    unknown += 1;
                    (format!("U{unknown}"), UNKNOWN)
                }
            };
            let selected = self.state.selected_claim == Some(claim.id);
            let response = Frame::new()
                .fill(if selected { SURFACE_HIGH } else { SURFACE })
                .stroke(Stroke::new(1.0, if selected { color } else { BORDER }))
                .corner_radius(5)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.label(RichText::new(label).strong().color(color));
                    ui.add(egui::Label::new(RichText::new(&claim.text).color(TEXT)).wrap());
                });
            if response.response.interact(Sense::click()).clicked() {
                self.state.select_claim(claim.id);
            }
            ui.add_space(5.0);
        }
        if self.state.selected_claim.is_some()
            && ui
                .link(RichText::new("Show all evidence").color(CYAN))
                .clicked()
        {
            self.state.selected_claim = None;
            self.state.source = None;
        }
        ui.add_space(10.0);
        small_heading(ui, "EVIDENCE");
        let evidence = self
            .state
            .visible_evidence()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let all_evidence_ids = self
            .state
            .selected_answer()
            .map_or_else(Vec::new, |answer| {
                answer.evidence.iter().map(|item| item.id).collect()
            });
        for item in evidence {
            let number = all_evidence_ids
                .iter()
                .position(|candidate| *candidate == item.id)
                .map_or(0, |index| index + 1);
            let selected = self.state.selected_evidence == Some(item.id);
            let response = Frame::new()
                .fill(if selected { SURFACE_HIGH } else { SURFACE })
                .stroke(Stroke::new(1.0, if selected { CYAN } else { BORDER }))
                .corner_radius(5)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("E{number}")).strong().color(CYAN));
                    ui.add(egui::Label::new(RichText::new(item.path.as_str()).color(TEXT)).wrap());
                    ui.label(
                        RichText::new(format!(
                            "lines {}-{}",
                            item.span.start().line(),
                            item.span.end().line()
                        ))
                        .small()
                        .color(MUTED),
                    );
                    if let Some(symbol) = item.symbol_id {
                        ui.label(
                            RichText::new(format!("symbol {}", short_id(&symbol)))
                                .small()
                                .color(MUTED),
                        );
                    }
                });
            if response.response.interact(Sense::click()).clicked() {
                let command = self.state.load_evidence(item.id);
                self.send_optional(command);
            }
            ui.add_space(5.0);
        }
        if evidence_is_empty(&self.state) {
            ui.label(RichText::new("No final evidence for this selection.").color(MUTED));
        }
        if let Some(source) = &self.state.source {
            ui.separator();
            ui.horizontal(|ui| {
                small_heading(ui, "SOURCE");
                match source.status {
                    SourceStatus::Loading => {
                        ui.spinner();
                        ui.label(RichText::new("Loading from index").small().color(CYAN));
                    }
                    SourceStatus::Loaded => {
                        ui.label(RichText::new("INDEXED SOURCE").small().color(FACT));
                    }
                    SourceStatus::Failed => {
                        ui.label(RichText::new("LOAD FAILED").small().color(ERROR));
                    }
                    SourceStatus::ReadOnly => {
                        ui.label(RichText::new("SAVED EXCERPT").small().color(UNKNOWN));
                    }
                    SourceStatus::Excerpt => {}
                }
            });
            ui.label(
                RichText::new(format!(
                    "{}:{}-{}",
                    source.path.as_str(),
                    source.start_line,
                    source.end_line
                ))
                .small()
                .color(MUTED),
            );
            if let Some(message) = &source.message {
                ui.label(RichText::new(message).color(ERROR));
            }
            ScrollArea::both().max_height(280.0).show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(&source.content)
                            .font(FontId::monospace(12.0))
                            .color(TEXT),
                    )
                    .wrap(),
                );
            });
        }
    }

    fn activity_panel(&mut self, context: &egui::Context) {
        let height = if self.state.activity_open {
            190.0
        } else {
            38.0
        };
        egui::TopBottomPanel::bottom("activity")
            .exact_height(height)
            .frame(
                Frame::new()
                    .fill(SURFACE)
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.state.activity_open, "ACTIVITY")
                        .clicked()
                    {
                        self.state.activity_open = !self.state.activity_open;
                    }
                    let selected = self.state.selected_task;
                    let count = self
                        .state
                        .activity
                        .iter()
                        .filter(|item| selected.is_none_or(|request| item.request_id == request))
                        .count();
                    ui.label(RichText::new(format!("{count} steps")).small().color(MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.state.is_busy() && ui.button("Cancel current work").clicked() {
                            let command = self.state.cancel();
                            self.send_optional(command);
                        }
                    });
                });
                if self.state.activity_open {
                    ui.separator();
                    ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                        let selected = self.state.selected_task;
                        for item in self.state.activity.iter().filter(|item| {
                            selected.is_none_or(|request| item.request_id == request)
                        }) {
                            let color = match item.status {
                                ActivityStatus::Running => CYAN,
                                ActivityStatus::Complete => FACT,
                                ActivityStatus::Cancelled => INFERENCE,
                                ActivityStatus::Failed => ERROR,
                                ActivityStatus::Information => MUTED,
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(activity_mark(item.status)).color(color));
                                ui.label(RichText::new(&item.title).color(TEXT));
                                if let Some(detail) = &item.detail {
                                    ui.label(RichText::new(detail).small().color(MUTED));
                                }
                            });
                        }
                    });
                }
            });
    }

    fn history_window(&mut self, context: &egui::Context) {
        if !self.state.history_open {
            return;
        }
        let mut open = self.state.history_open;
        let mut selected = None;
        egui::Window::new("Session history")
            .open(&mut open)
            .default_size([620.0, 430.0])
            .show(context, |ui| {
                ui.label(
                    RichText::new(
                        "Saved research sessions from all repositories. Cross-repository sessions open read-only.",
                    )
                    .color(MUTED),
                );
                ui.separator();
                if self.state.history_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading saved sessions...");
                    });
                } else if self.state.sessions.is_empty() {
                    ui.label(RichText::new("No saved sessions yet.").color(MUTED));
                } else {
                    ScrollArea::vertical().show(ui, |ui| {
                        for session in &self.state.sessions {
                            let current_repository = self
                                .state
                                .repository
                                .as_ref()
                                .is_some_and(|repository| repository.id == session.repository_id);
                            let title = session
                                .tasks
                                .last()
                                .map_or("Empty session", |task| task.question.as_str());
                            let response = Frame::new()
                                .fill(SURFACE)
                                .stroke(Stroke::new(1.0, BORDER))
                                .corner_radius(5)
                                .inner_margin(10)
                                .show(ui, |ui| {
                                    ui.label(RichText::new(title).strong().color(TEXT));
                                    ui.label(
                                        RichText::new(format!(
                                            "{} tasks | repository {} | {}",
                                            session.tasks.len(),
                                            short_id(&session.repository_id),
                                            if current_repository { "can continue" } else { "read-only" }
                                        ))
                                        .small()
                                        .color(if current_repository { FACT } else { UNKNOWN }),
                                    );
                                });
                            if response.response.interact(Sense::click()).clicked() {
                                selected = Some(session.session_id);
                            }
                            ui.add_space(7.0);
                        }
                    });
                }
            });
        self.state.history_open = open;
        if let Some(session_id) = selected {
            let command = self.state.load_session(session_id);
            self.send_optional(command);
        }
    }

    fn repository_window(&mut self, context: &egui::Context) {
        if !self.repository_editor_open {
            return;
        }
        let mut open = self.repository_editor_open;
        let window_width = (context.screen_rect().width() - 48.0).clamp(420.0, 620.0);
        egui::Window::new("Index a repository")
            .open(&mut open)
            .fixed_size([window_width, 138.0])
            .collapsible(false)
            .show(context, |ui| {
                ui.label(RichText::new("Rust and Python repositories are supported.").color(MUTED));
                let input_width = (ui.available_width() - 118.0).max(240.0);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [input_width, 32.0],
                        TextEdit::singleline(&mut self.state.repository_input),
                    );
                    if ui.button("Choose folder").clicked() {
                        self.choose_directory(context);
                    }
                });
                ui.add_space(10.0);
                if ui
                    .add_enabled(!self.state.is_busy(), egui::Button::new("Index repository"))
                    .clicked()
                {
                    let command = self.state.index_repository();
                    self.send_optional(command);
                    if self.state.error.is_none() {
                        self.repository_editor_open = false;
                    }
                }
            });
        self.repository_editor_open &= open;
    }
}

impl eframe::App for GuiWindow {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(context);
        self.drain_directory_result();
        self.top_bar(context);
        if self.state.repository.is_none() {
            self.welcome(context);
        } else {
            self.workspace(context);
        }
        self.history_window(context);
        self.repository_window(context);
    }
}

fn relay_backend_events(
    backend: &mpsc::Receiver<AppEvent>,
    frontend: &mpsc::SyncSender<AppEvent>,
    repaint: &egui::Context,
) {
    let mut pending_progress = None;
    let mut next_progress_flush = Instant::now() + PROGRESS_FLUSH_INTERVAL;
    loop {
        let wait = next_progress_flush.saturating_duration_since(Instant::now());
        match backend.recv_timeout(wait) {
            Ok(event @ AppEvent::Progress { .. }) => pending_progress = Some(event),
            Ok(event) => {
                // Progress is disposable under pressure; contract and terminal events are not.
                flush_progress_before_critical(frontend, repaint, &mut pending_progress);
                if frontend.send(event).is_err() {
                    return;
                }
                repaint.request_repaint();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !try_flush_progress(frontend, repaint, &mut pending_progress) {
                    return;
                }
                next_progress_flush = Instant::now() + PROGRESS_FLUSH_INTERVAL;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = try_flush_progress(frontend, repaint, &mut pending_progress);
                return;
            }
        }
        if Instant::now() >= next_progress_flush {
            if !try_flush_progress(frontend, repaint, &mut pending_progress) {
                return;
            }
            next_progress_flush = Instant::now() + PROGRESS_FLUSH_INTERVAL;
        }
    }
}

fn flush_progress_before_critical(
    frontend: &mpsc::SyncSender<AppEvent>,
    repaint: &egui::Context,
    pending: &mut Option<AppEvent>,
) {
    let Some(progress) = pending.take() else {
        return;
    };
    if frontend.try_send(progress).is_ok() {
        repaint.request_repaint();
    }
}

fn try_flush_progress(
    frontend: &mpsc::SyncSender<AppEvent>,
    repaint: &egui::Context,
    pending: &mut Option<AppEvent>,
) -> bool {
    let Some(progress) = pending.take() else {
        return true;
    };
    match frontend.try_send(progress) {
        Ok(()) => {
            repaint.request_repaint();
            true
        }
        Err(mpsc::TrySendError::Full(progress)) => {
            *pending = Some(progress);
            true
        }
        Err(mpsc::TrySendError::Disconnected(_)) => false,
    }
}

fn configure_style(context: &egui::Context) {
    register_cjk_fallback(context);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = Color32::from_rgb(9, 13, 17);
    visuals.faint_bg_color = SURFACE_HIGH;
    visuals.widgets.inactive.bg_fill = SURFACE_HIGH;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(29, 47, 56);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, CYAN);
    visuals.widgets.active.bg_fill = Color32::from_rgb(30, 65, 72);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, CYAN);
    visuals.selection.bg_fill = Color32::from_rgb(25, 90, 99);
    visuals.selection.stroke = Stroke::new(1.0, CYAN);
    visuals.override_text_color = Some(TEXT);
    context.set_visuals(visuals);
    context.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 7.0);
        style.spacing.button_padding = Vec2::new(12.0, 7.0);
        style.visuals.window_corner_radius = 7.into();
    });
}

fn register_cjk_fallback(context: &egui::Context) {
    let font = cjk_font_paths().iter().find_map(|path| fs::read(path).ok());
    let Some(font) = font else {
        return;
    };
    context.add_font(FontInsert::new(
        "codeatlas-system-cjk",
        FontData::from_owned(font),
        vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                priority: FontPriority::Lowest,
            },
            InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    ));
}

#[cfg(target_os = "linux")]
const fn cjk_font_paths() -> &'static [&'static str] {
    &[
        // WSL can reuse the host's fonts even when the Linux distribution has
        // no CJK font packages installed.
        "/mnt/c/Windows/Fonts/msyh.ttc",
        "/mnt/c/Windows/Fonts/msyhl.ttc",
        "/mnt/c/Windows/Fonts/simsun.ttc",
        "/mnt/c/Windows/Fonts/simhei.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/opentype/adobe-source-han-sans/SourceHanSansCN-Regular.otf",
        "/usr/share/fonts/opentype/source-han-sans/SourceHanSansSC-Regular.otf",
    ]
}

#[cfg(target_os = "windows")]
const fn cjk_font_paths() -> &'static [&'static str] {
    &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ]
}

#[cfg(target_os = "macos")]
const fn cjk_font_paths() -> &'static [&'static str] {
    &[
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
    ]
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
const fn cjk_font_paths() -> &'static [&'static str] {
    &[]
}

fn panel_frame() -> Frame {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(14)
}

fn section_title(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).strong().color(CYAN));
    ui.label(RichText::new(subtitle).small().color(MUTED));
    ui.add_space(9.0);
}

fn small_heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(MUTED));
}

fn banner(ui: &mut Ui, color: Color32, title: &str, message: &str) {
    Frame::new()
        .fill(SURFACE_HIGH)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(5)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.label(RichText::new(title).small().strong().color(color));
            ui.label(RichText::new(message).color(TEXT));
        });
    ui.add_space(8.0);
}

fn status_pill(ui: &mut Ui, status: WorkStatus) {
    let color = match status {
        WorkStatus::Error => ERROR,
        WorkStatus::Indexing | WorkStatus::Thinking | WorkStatus::Cancelling => CYAN,
        WorkStatus::Indexed | WorkStatus::AnswerReady => FACT,
        WorkStatus::Cancelled => INFERENCE,
        WorkStatus::Idle => MUTED,
    };
    Frame::new()
        .fill(SURFACE_HIGH)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(status.label()).small().color(color));
        });
}

fn language_badge(ui: &mut Ui, name: &str) {
    Frame::new()
        .fill(SURFACE_HIGH)
        .corner_radius(3)
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(name).small().strong().color(CYAN));
        });
}

fn panel_tab(ui: &mut Ui, panel: &mut WorkspacePanel, value: WorkspacePanel, label: &str) {
    if ui.selectable_label(*panel == value, label).clicked() {
        *panel = value;
    }
}

fn render_turn_answer(ui: &mut Ui, text: &str, status: TurnStatus) {
    if !text.is_empty() {
        render_markdown(ui, text);
    }
    match status {
        TurnStatus::Running if text.is_empty() => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Researching the repository...").color(MUTED));
            });
        }
        TurnStatus::Running | TurnStatus::Completed if !text.is_empty() => {}
        TurnStatus::Completed => {
            ui.label(RichText::new("The answer completed without display text.").color(MUTED));
        }
        TurnStatus::Cancelled => {
            ui.label(RichText::new("Request cancelled.").color(INFERENCE));
        }
        TurnStatus::Failed => {
            ui.label(
                RichText::new("The request failed before an answer was completed.").color(ERROR),
            );
        }
        TurnStatus::Running => {}
    }
}

fn question_text_edit(input: &mut String) -> TextEdit<'_> {
    TextEdit::multiline(input)
        .hint_text("Ask how this codebase works...")
        .desired_rows(2)
        .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter))
}

fn is_plain_enter(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: Key::Enter,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none()
    )
}

fn has_plain_enter(events: &[egui::Event]) -> bool {
    events.iter().any(is_plain_enter)
}

fn question_enter_submits(events: &[egui::Event], has_focus: bool, ime_active: &mut bool) -> bool {
    if !has_focus {
        *ime_active = false;
        return false;
    }

    // A commit and its confirming Enter may arrive in the same frame. Treat any
    // IME activity in that frame as composition so candidate confirmation never submits.
    let composition_in_progress = *ime_active
        || events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Ime(
                    egui::ImeEvent::Enabled
                        | egui::ImeEvent::Preedit(_)
                        | egui::ImeEvent::Commit(_)
                )
            )
        });
    for event in events {
        match event {
            egui::Event::Ime(egui::ImeEvent::Enabled | egui::ImeEvent::Preedit(_)) => {
                *ime_active = true;
            }
            egui::Event::Ime(egui::ImeEvent::Commit(_) | egui::ImeEvent::Disabled) => {
                *ime_active = false;
            }
            _ => {}
        }
    }

    !composition_in_progress && has_plain_enter(events)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InlineStyle {
    strong: bool,
    emphasis: bool,
    kind: InlineKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InlineKind {
    #[default]
    Text,
    Code,
    Link,
}

#[derive(Debug, PartialEq, Eq)]
struct InlineSpan {
    text: String,
    style: InlineStyle,
}

#[derive(Debug, PartialEq, Eq)]
enum MarkdownBlock {
    Heading { level: usize, text: String },
    Paragraph(String),
    UnorderedItem(String),
    OrderedItem { number: String, text: String },
    Quote(String),
    Code(String),
    Rule,
}

fn render_markdown(ui: &mut Ui, text: &str) {
    for block in parse_markdown_blocks(text) {
        match block {
            MarkdownBlock::Heading { level, text } => {
                ui.add_space(if level <= 2 { 8.0 } else { 4.0 });
                let size = match level {
                    1 => 21.0,
                    2 => 18.0,
                    3 => 16.0,
                    _ => 14.0,
                };
                markdown_inline_label(ui, &text, size, true);
                ui.add_space(2.0);
            }
            MarkdownBlock::Paragraph(text) => {
                markdown_inline_label(ui, &text, 14.0, false);
                ui.add_space(3.0);
            }
            MarkdownBlock::UnorderedItem(text) => {
                markdown_list_item(ui, "\u{2022}", &text);
            }
            MarkdownBlock::OrderedItem { number, text } => {
                markdown_list_item(ui, &format!("{number}."), &text);
            }
            MarkdownBlock::Quote(text) => {
                Frame::new()
                    .fill(SURFACE_HIGH)
                    .stroke(Stroke::new(2.0, CYAN))
                    .inner_margin(Margin::symmetric(8, 5))
                    .show(ui, |ui| markdown_inline_label(ui, &text, 14.0, false));
            }
            MarkdownBlock::Code(code) => {
                Frame::new()
                    .fill(Color32::from_rgb(9, 13, 17))
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(4)
                    .inner_margin(Margin::symmetric(8, 7))
                    .show(ui, |ui| {
                        let mut job = egui::text::LayoutJob::simple(
                            if code.is_empty() {
                                " ".to_owned()
                            } else {
                                code
                            },
                            FontId::monospace(12.0),
                            TEXT,
                            ui.available_width(),
                        );
                        job.wrap.break_anywhere = true;
                        ui.add(egui::Label::new(job).wrap());
                    });
                ui.add_space(3.0);
            }
            MarkdownBlock::Rule => {
                ui.add_space(2.0);
                ui.separator();
                ui.add_space(2.0);
            }
        }
    }
}

fn markdown_list_item(ui: &mut Ui, marker: &str, text: &str) {
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(marker).strong().color(CYAN));
        markdown_inline_label(ui, text, 14.0, false);
    });
}

fn markdown_inline_label(ui: &mut Ui, text: &str, size: f32, strong: bool) {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.break_anywhere = true;
    let base_style = InlineStyle {
        strong,
        ..InlineStyle::default()
    };
    for span in parse_inline(text, base_style) {
        let mut rich = RichText::new(span.text).size(size);
        match span.style.kind {
            InlineKind::Code => {
                rich = rich
                    .font(FontId::monospace((size - 1.0).max(11.0)))
                    .background_color(Color32::from_rgb(35, 45, 53))
                    .color(TEXT);
            }
            InlineKind::Link => rich = rich.color(CYAN).underline(),
            InlineKind::Text if span.style.strong => rich = rich.strong(),
            InlineKind::Text => rich = rich.color(TEXT),
        }
        if span.style.emphasis {
            rich = rich.italics();
        }
        rich.append_to(
            &mut job,
            ui.style(),
            egui::FontSelection::Default,
            Align::Center,
        );
    }
    ui.add(egui::Label::new(job).wrap());
}

fn parse_markdown_blocks(text: &str) -> Vec<MarkdownBlock> {
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut code = Vec::new();
    let mut fence = None;

    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some((fence_char, fence_len)) = fence {
            if is_closing_fence(line, fence_char, fence_len) {
                blocks.push(MarkdownBlock::Code(code.join("\n")));
                code.clear();
                fence = None;
            } else {
                code.push(line.to_owned());
            }
            continue;
        }

        if let Some(opening) = opening_fence(line) {
            flush_markdown_paragraph(&mut blocks, &mut paragraph);
            fence = Some(opening);
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush_markdown_paragraph(&mut blocks, &mut paragraph);
        } else if is_markdown_rule(trimmed) {
            flush_markdown_paragraph(&mut blocks, &mut paragraph);
            blocks.push(MarkdownBlock::Rule);
        } else if let Some((level, heading)) = markdown_heading(trimmed) {
            flush_markdown_paragraph(&mut blocks, &mut paragraph);
            blocks.push(MarkdownBlock::Heading {
                level,
                text: heading.to_owned(),
            });
        } else if let Some(item) = unordered_list_item(trimmed) {
            flush_markdown_paragraph(&mut blocks, &mut paragraph);
            blocks.push(MarkdownBlock::UnorderedItem(item.to_owned()));
        } else if let Some((number, item)) = ordered_list_item(trimmed) {
            flush_markdown_paragraph(&mut blocks, &mut paragraph);
            blocks.push(MarkdownBlock::OrderedItem {
                number: number.to_owned(),
                text: item.to_owned(),
            });
        } else if let Some(quote) = trimmed.strip_prefix("> ") {
            flush_markdown_paragraph(&mut blocks, &mut paragraph);
            blocks.push(MarkdownBlock::Quote(quote.to_owned()));
        } else {
            paragraph.push(line.trim().to_owned());
        }
    }
    flush_markdown_paragraph(&mut blocks, &mut paragraph);
    if fence.is_some() {
        blocks.push(MarkdownBlock::Code(code.join("\n")));
    }
    blocks
}

fn flush_markdown_paragraph(blocks: &mut Vec<MarkdownBlock>, lines: &mut Vec<String>) {
    if lines.is_empty() {
        return;
    }
    let mut paragraph = String::new();
    for line in lines.drain(..) {
        if !paragraph.is_empty() {
            if paragraph.ends_with("  ") {
                paragraph.truncate(paragraph.trim_end().len());
                paragraph.push('\n');
            } else {
                paragraph.push(' ');
            }
        }
        paragraph.push_str(&line);
    }
    blocks.push(MarkdownBlock::Paragraph(paragraph));
}

fn opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let fence_char = trimmed.chars().next()?;
    if !matches!(fence_char, '`' | '~') {
        return None;
    }
    let count = trimmed.chars().take_while(|ch| *ch == fence_char).count();
    (count >= 3).then_some((fence_char, count))
}

fn is_closing_fence(line: &str, fence_char: char, minimum_len: usize) -> bool {
    let trimmed = line.trim();
    let count = trimmed.chars().take_while(|ch| *ch == fence_char).count();
    count >= minimum_len && trimmed[count..].trim().is_empty()
}

fn is_markdown_rule(text: &str) -> bool {
    let mut marker = None;
    let mut count = 0;
    for ch in text.chars().filter(|ch| !ch.is_whitespace()) {
        if !matches!(ch, '-' | '_' | '*') || marker.is_some_and(|marker| marker != ch) {
            return false;
        }
        marker = Some(ch);
        count += 1;
    }
    count >= 3
}

fn markdown_heading(text: &str) -> Option<(usize, &str)> {
    let level = text.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&level) || text.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    Some((level, text[level + 1..].trim()))
}

fn unordered_list_item(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    (bytes.len() >= 2 && matches!(bytes[0], b'-' | b'+' | b'*') && bytes[1].is_ascii_whitespace())
        .then(|| text[2..].trim_start())
}

fn ordered_list_item(text: &str) -> Option<(&str, &str)> {
    let digit_count = text.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 || digit_count > 9 {
        return None;
    }
    let bytes = text.as_bytes();
    if !matches!(bytes.get(digit_count), Some(b'.' | b')'))
        || !bytes
            .get(digit_count + 1)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }
    Some((&text[..digit_count], text[digit_count + 2..].trim_start()))
}

fn parse_inline(text: &str, base_style: InlineStyle) -> Vec<InlineSpan> {
    let mut spans = Vec::new();
    parse_inline_into(text, base_style, &mut spans);
    spans
}

fn parse_inline_into(text: &str, style: InlineStyle, spans: &mut Vec<InlineSpan>) {
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        if let Some(escaped) = rest.strip_prefix('\\').and_then(|tail| tail.chars().next()) {
            if "\\`*_[]".contains(escaped) {
                push_inline_span(spans, &escaped.to_string(), style);
                index += 1 + escaped.len_utf8();
                continue;
            }
        }

        if rest.starts_with('`') {
            let delimiter_len = rest.bytes().take_while(|byte| *byte == b'`').count();
            let delimiter = &rest[..delimiter_len];
            if let Some(end) = find_unescaped(text, index + delimiter_len, delimiter) {
                let code = text[index + delimiter_len..end].replace('\n', " ");
                let code = normalize_code_span(&code);
                push_inline_span(
                    spans,
                    code,
                    InlineStyle {
                        kind: InlineKind::Code,
                        ..style
                    },
                );
                index = end + delimiter_len;
                continue;
            }
        }

        if let Some((delimiter, delimiter_len)) = ["**", "__"]
            .into_iter()
            .find(|delimiter| rest.starts_with(delimiter))
            .map(|delimiter| (delimiter, delimiter.len()))
        {
            if let Some(end) = find_unescaped(text, index + delimiter_len, delimiter) {
                if end > index + delimiter_len
                    && emphasis_delimiters_are_valid(text, index, end, delimiter)
                {
                    parse_inline_into(
                        &text[index + delimiter_len..end],
                        InlineStyle {
                            strong: true,
                            ..style
                        },
                        spans,
                    );
                    index = end + delimiter_len;
                    continue;
                }
            }
        }

        if rest.starts_with('[') {
            if let Some(label_end) = find_unescaped(text, index + 1, "](") {
                if let Some(target_end) = find_unescaped(text, label_end + 2, ")") {
                    if label_end > index + 1 && target_end > label_end + 2 {
                        parse_inline_into(
                            &text[index + 1..label_end],
                            InlineStyle {
                                kind: InlineKind::Link,
                                ..style
                            },
                            spans,
                        );
                        index = target_end + 1;
                        continue;
                    }
                }
            }
        }

        if let Some(delimiter) = ['*', '_'].into_iter().find(|ch| rest.starts_with(*ch)) {
            let delimiter_text = &rest[..delimiter.len_utf8()];
            if let Some(end) = find_unescaped(text, index + delimiter.len_utf8(), delimiter_text) {
                if end > index + delimiter.len_utf8()
                    && emphasis_delimiters_are_valid(text, index, end, delimiter_text)
                {
                    parse_inline_into(
                        &text[index + delimiter.len_utf8()..end],
                        InlineStyle {
                            emphasis: true,
                            ..style
                        },
                        spans,
                    );
                    index = end + delimiter.len_utf8();
                    continue;
                }
            }
        }

        let ch = rest.chars().next().expect("non-empty inline text");
        push_inline_span(spans, &ch.to_string(), style);
        index += ch.len_utf8();
    }
}

fn emphasis_delimiters_are_valid(
    text: &str,
    opening: usize,
    closing: usize,
    delimiter: &str,
) -> bool {
    let content = &text[opening + delimiter.len()..closing];
    if content.chars().next().is_some_and(char::is_whitespace)
        || content.chars().next_back().is_some_and(char::is_whitespace)
    {
        return false;
    }
    delimiter.starts_with('*')
        || (!text[..opening]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
            && !text[closing + delimiter.len()..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric))
}

fn find_unescaped(text: &str, start: usize, delimiter: &str) -> Option<usize> {
    let mut search_from = start;
    while let Some(relative) = text[search_from..].find(delimiter) {
        let found = search_from + relative;
        let backslashes = text[..found]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\\')
            .count();
        if backslashes % 2 == 0 {
            return Some(found);
        }
        search_from = found + delimiter.len();
    }
    None
}

fn normalize_code_span(code: &str) -> &str {
    if code.starts_with(' ') && code.ends_with(' ') && !code.trim().is_empty() {
        &code[1..code.len() - 1]
    } else {
        code
    }
}

fn push_inline_span(spans: &mut Vec<InlineSpan>, text: &str, style: InlineStyle) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
        last.text.push_str(text);
    } else {
        spans.push(InlineSpan {
            text: text.to_owned(),
            style,
        });
    }
}

fn render_answer_details(
    ui: &mut Ui,
    answer: &codeatlas_core::AgentAnswer,
    diagram_message: Option<&str>,
) -> Option<codeatlas_core::DiagramId> {
    let mut open_diagram = None;
    if !answer.call_paths.is_empty() {
        ui.add_space(12.0);
        small_heading(ui, "CALL PATHS");
        for path in &answer.call_paths {
            let title = path.label.as_deref().unwrap_or("Call path");
            wrapped_label(
                ui,
                RichText::new(format!(
                    "{} | {}",
                    title,
                    if path.complete { "complete" } else { "partial" }
                ))
                .strong()
                .color(CYAN),
            );
            let steps = path
                .steps
                .iter()
                .map(|step| call_target(&step.target))
                .collect::<Vec<_>>()
                .join("  ->  ");
            wrapped_label(
                ui,
                RichText::new(steps)
                    .font(FontId::monospace(12.0))
                    .color(TEXT),
            );
        }
    }
    ui.add_space(12.0);
    small_heading(ui, "DIAGRAM DECISION");
    match &answer.diagram {
        DiagramDecision::NotNeeded { reason } => {
            wrapped_label(
                ui,
                RichText::new(format!("Not needed | {reason}")).color(MUTED),
            );
        }
        DiagramDecision::Needed { reason, diagram } => {
            ui.label(
                RichText::new(format!(
                    "{} | {} nodes | {} links",
                    diagram_kind(diagram.kind),
                    diagram.nodes.len(),
                    diagram.edges.len()
                ))
                .color(CYAN),
            );
            wrapped_label(ui, RichText::new(reason).color(MUTED));
            if let Some(artifact) = &diagram.artifact {
                ui.label(
                    RichText::new(format!(
                        "SVG {} | {} bytes",
                        short_id(&artifact.id),
                        artifact.byte_size
                    ))
                    .small()
                    .color(MUTED),
                );
                if ui.button("Open diagram").clicked() {
                    open_diagram = Some(artifact.id);
                }
            } else {
                ui.label(RichText::new("No local SVG artifact is available.").color(INFERENCE));
            }
        }
    }
    if let Some(message) = diagram_message {
        ui.label(RichText::new(message).small().color(MUTED));
    }
    open_diagram
}

fn workspace_side_panel_max_widths(viewport_width: f32) -> (f32, f32) {
    let expandable = (viewport_width
        - CONVERSATION_PANEL_MIN_WIDTH
        - REPOSITORY_PANEL_MIN_WIDTH
        - EVIDENCE_PANEL_MIN_WIDTH)
        .max(0.0);
    let repository =
        (REPOSITORY_PANEL_MIN_WIDTH + expandable * 0.4).min(REPOSITORY_PANEL_MAX_WIDTH);
    let evidence = (EVIDENCE_PANEL_MIN_WIDTH + expandable * 0.6).min(EVIDENCE_PANEL_MAX_WIDTH);
    (repository, evidence)
}

fn conversation_scroll_area(max_height: f32) -> ScrollArea {
    ScrollArea::vertical()
        .id_salt("conversation_turns")
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .max_height(max_height)
}

fn wrapped_label(ui: &mut Ui, text: RichText) -> egui::Response {
    ui.add(egui::Label::new(text).wrap())
}

fn render_usage_summary(ui: &mut Ui, usage: &ModelUsage) {
    Frame::new()
        .fill(SURFACE_HIGH)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(5)
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            small_heading(ui, "TOKEN USAGE");
            wrapped_label(ui, RichText::new(usage_summary(usage)).small().color(TEXT));
        });
}

fn usage_summary(usage: &ModelUsage) -> String {
    let tokens = usage.tokens;
    let mut summary = format!(
        "Input {}  |  Output {}  |  Cached {}  |  Total {} tokens",
        tokens.input_tokens, tokens.output_tokens, tokens.cached_input_tokens, tokens.total_tokens
    );
    if let Some(cost) = &usage.cost {
        let _ = write!(summary, "  |  Cost {:.4} {}", cost.amount, cost.currency);
        if cost.estimated {
            summary.push_str(" (estimated)");
        }
    }
    summary
}

fn evidence_is_empty(state: &GuiState) -> bool {
    state.visible_evidence().is_empty()
}

const fn activity_mark(status: ActivityStatus) -> &'static str {
    match status {
        ActivityStatus::Running => "RUN",
        ActivityStatus::Complete => "OK",
        ActivityStatus::Cancelled => "STOP",
        ActivityStatus::Failed => "ERR",
        ActivityStatus::Information => "STEP",
    }
}

fn language_name(language: &Language) -> &str {
    match language {
        Language::Rust => "Rust",
        Language::Python => "Python",
        Language::C => "C",
        Language::Cpp => "C++",
        Language::Java => "Java",
        Language::JavaScript => "JavaScript",
        Language::TypeScript => "TypeScript",
        Language::Go => "Go",
        Language::Other(name) => name,
    }
}

fn entry_kind(kind: &EntryPointKind) -> &str {
    match kind {
        EntryPointKind::Executable => "executable",
        EntryPointKind::Library => "library",
        EntryPointKind::Test => "test",
        EntryPointKind::Benchmark => "benchmark",
        EntryPointKind::WebRoute => "web route",
        EntryPointKind::BackgroundTask => "background task",
        EntryPointKind::Other(name) => name,
    }
}

const fn diagram_kind(kind: DiagramKind) -> &'static str {
    match kind {
        DiagramKind::Architecture => "Architecture",
        DiagramKind::Flow => "Flow",
        DiagramKind::Relationship => "Relationship",
    }
}

#[cfg(test)]
mod tests {
    use codeatlas_core::{
        AgentAnswer, AnswerId, Cost, DiagramDecision, ModelUsage, Progress, ProgressPhase,
        RequestId, TokenUsage,
    };

    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn cjk_fallback_includes_wsl_windows_fonts() {
        assert!(cjk_font_paths().contains(&"/mnt/c/Windows/Fonts/msyh.ttc"));
    }

    #[test]
    fn side_panel_limits_always_preserve_conversation_width() {
        for viewport_width in [NARROW_WORKSPACE_WIDTH, 1_100.0, 1_400.0, 1_920.0] {
            let (repository, evidence) = workspace_side_panel_max_widths(viewport_width);
            assert!(repository >= REPOSITORY_PANEL_MIN_WIDTH);
            assert!(repository <= REPOSITORY_PANEL_MAX_WIDTH);
            assert!(evidence >= EVIDENCE_PANEL_MIN_WIDTH);
            assert!(evidence <= EVIDENCE_PANEL_MAX_WIDTH);
            assert!(
                viewport_width - repository - evidence >= CONVERSATION_PANEL_MIN_WIDTH,
                "viewport {viewport_width} left only {} points for Conversation",
                viewport_width - repository - evidence
            );
        }
    }

    #[test]
    fn usage_summary_contains_all_token_counts_and_optional_cost() {
        let usage = ModelUsage {
            tokens: TokenUsage {
                input_tokens: 1_200,
                output_tokens: 345,
                cached_input_tokens: 678,
                total_tokens: 1_545,
            },
            cost: Some(Cost {
                currency: "USD".to_owned(),
                amount: 0.0123,
                estimated: true,
            }),
        };

        assert_eq!(
            usage_summary(&usage),
            "Input 1200  |  Output 345  |  Cached 678  |  Total 1545 tokens  |  Cost 0.0123 USD (estimated)"
        );
    }

    #[test]
    fn markdown_parses_headings_lists_and_fenced_code_as_blocks() {
        assert_eq!(
            parse_markdown_blocks(
                "# 标题 Heading\n\n- 第一项\n* second\n1. ordered\n2) 第二项\n\n```rust\nfn main() {}\n```"
            ),
            vec![
                MarkdownBlock::Heading {
                    level: 1,
                    text: "标题 Heading".to_owned(),
                },
                MarkdownBlock::UnorderedItem("第一项".to_owned()),
                MarkdownBlock::UnorderedItem("second".to_owned()),
                MarkdownBlock::OrderedItem {
                    number: "1".to_owned(),
                    text: "ordered".to_owned(),
                },
                MarkdownBlock::OrderedItem {
                    number: "2".to_owned(),
                    text: "第二项".to_owned(),
                },
                MarkdownBlock::Code("fn main() {}".to_owned()),
            ]
        );
    }

    #[test]
    fn markdown_inline_markers_become_styles_without_damaging_unicode() {
        let spans = parse_inline(
            "中文 **bold 强调** and `foo::bar` with [链接](https://example.com)",
            InlineStyle::default(),
        );
        let visible_text = spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert_eq!(visible_text, "中文 bold 强调 and foo::bar with 链接");
        assert!(
            spans
                .iter()
                .any(|span| span.text == "bold 强调" && span.style.strong)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "foo::bar" && span.style.kind == InlineKind::Code)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "链接" && span.style.kind == InlineKind::Link)
        );
        assert_eq!(
            parse_inline("unmatched **marker", InlineStyle::default()),
            vec![InlineSpan {
                text: "unmatched **marker".to_owned(),
                style: InlineStyle::default(),
            }]
        );
        assert_eq!(
            parse_inline("provider_call_ids", InlineStyle::default()),
            vec![InlineSpan {
                text: "provider_call_ids".to_owned(),
                style: InlineStyle::default(),
            }]
        );
    }

    #[test]
    fn enter_shortcuts_respect_shift_and_ime_composition() {
        let enter = egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let shift_enter = egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::SHIFT,
        };
        let mut ime_active = false;

        assert!(question_enter_submits(
            std::slice::from_ref(&enter),
            true,
            &mut ime_active
        ));
        assert!(!question_enter_submits(
            &[egui::Event::Ime(egui::ImeEvent::Enabled), shift_enter],
            true,
            &mut ime_active,
        ));
        assert!(ime_active);
        assert!(!question_enter_submits(
            &[
                egui::Event::Ime(egui::ImeEvent::Commit("中文".to_owned())),
                enter.clone(),
            ],
            true,
            &mut ime_active,
        ));
        assert!(!ime_active);
        assert!(question_enter_submits(&[enter], true, &mut ime_active));
    }

    #[test]
    fn native_text_edit_accepts_ime_commit_and_shift_enter_newline() {
        let context = egui::Context::default();
        let input_id = egui::Id::new("native-question-input-test");
        let mut question = String::new();
        let run_input = |events, question: &mut String| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(320.0, 180.0),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        ui.memory_mut(|memory| memory.request_focus(input_id));
                        ui.add(question_text_edit(question).id(input_id));
                    });
                },
            )
        };

        let _ = run_input(Vec::new(), &mut question);
        let _ = run_input(
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Commit("中文 input".to_owned())),
            ],
            &mut question,
        );
        let _ = run_input(
            vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::SHIFT,
            }],
            &mut question,
        );

        assert_eq!(question, "中文 input\n");
    }

    #[test]
    fn markdown_wraps_mixed_long_text_within_conversation_width() {
        let context = egui::Context::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(320.0, 600.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let width = 260.0;
                    let bounds = ui
                        .allocate_ui_with_layout(
                            Vec2::new(width, ui.available_height()),
                            Layout::top_down(Align::Min),
                            |ui| {
                                let right_limit = ui.max_rect().right();
                                render_markdown(
                                    ui,
                                    "一段没有空格但必须完整显示的中文文本一段没有空格但必须完整显示的中文文本\n- a_really_long_code_identifier_without_any_spaces_that_must_wrap_inside_the_conversation_column\n```rust\na_really_long_code_identifier_without_any_spaces_that_must_also_wrap_in_a_code_block\n```",
                                );
                                (ui.min_rect().right(), right_limit)
                            },
                        )
                        .inner;

                    assert!(
                        bounds.0 <= bounds.1 + 0.5,
                        "content right {} exceeded limit {}",
                        bounds.0,
                        bounds.1
                    );
                });
            },
        );
    }

    #[test]
    fn conversation_scroll_keeps_answer_details_reachable_at_bottom() {
        let context = egui::Context::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(340.0, 500.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    ui.set_width(280.0);
                    let answer = AgentAnswer {
                        id: AnswerId::from_stable_parts(&["scroll-answer"]),
                        text: "Answer".to_owned(),
                        claims: Vec::new(),
                        evidence: Vec::new(),
                        call_paths: Vec::new(),
                        diagram: DiagramDecision::NotNeeded {
                            reason: "The detail at the bottom remains reachable.".to_owned(),
                        },
                        usage: None,
                    };
                    let output = conversation_scroll_area(120.0).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for _ in 0..30 {
                            wrapped_label(
                                ui,
                                RichText::new("A long answer line that wraps safely."),
                            );
                        }
                        render_answer_details(ui, &answer, None);
                    });
                    let max_offset = output.content_size.y - output.inner_rect.height();

                    assert!(max_offset > 0.0);
                    assert!((output.state.offset.y - max_offset).abs() < 0.5);
                    assert!(output.content_size.x <= output.inner_rect.width() + 0.5);
                });
            },
        );
    }

    #[test]
    fn event_bridge_coalesces_progress_but_preserves_terminal_events() {
        let request_id = RequestId::from_stable_parts(&["bridge-progress"]);
        let (backend_sender, backend_receiver) = mpsc::channel();
        let (frontend_sender, frontend_receiver) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let bridge = thread::spawn(move || {
            relay_backend_events(
                &backend_receiver,
                &frontend_sender,
                &egui::Context::default(),
            );
        });
        for step in 0..5_000 {
            backend_sender
                .send(AppEvent::Progress {
                    request_id,
                    progress: Progress {
                        phase: ProgressPhase::Parsing,
                        message: format!("file {step}"),
                        completed: Some(step),
                        total: Some(5_000),
                    },
                })
                .expect("bridge is running");
        }
        backend_sender
            .send(AppEvent::Cancelled { request_id })
            .expect("terminal event");
        drop(backend_sender);

        let mut progress_events = 0;
        let mut terminal_seen = false;
        while let Ok(event) = frontend_receiver.recv_timeout(Duration::from_secs(2)) {
            match event {
                AppEvent::Progress { .. } => progress_events += 1,
                AppEvent::Cancelled {
                    request_id: cancelled,
                } if cancelled == request_id => terminal_seen = true,
                _ => {}
            }
        }
        bridge.join().expect("event bridge");

        assert!(terminal_seen);
        assert!(progress_events <= EVENT_QUEUE_CAPACITY);
    }
}
