//! Application shell: tabs, menus, global windows (help, merge, profiles)
//! and the saved session. Everything about one file lives in [`Tab`].

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, FontFamily, Key, Modifiers};
use logview_core::{MergeSource, Notify, TextEncoding, merge_files};

mod profiles;

use crate::config::{Session, Theme};
use crate::tab::Tab;
use profiles::ProfileUi;

/// "Scal karty" dialog: which tabs (by id) take part.
struct MergeDialog {
    selected: Vec<(u64, bool)>,
}

pub struct LogViewApp {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    notify: Notify,
    bold_family: Option<FontFamily>,
    show_details: bool,
    show_filter_panel: bool,
    show_timeline: bool,
    show_help: bool,
    title: String,
    /// Errors not tied to a tab (configuration, profiles).
    error: Option<String>,
    profile_ui: ProfileUi,
    merge_dialog: Option<MergeDialog>,
    /// Id of the tab last scrolled into view in the tab bar.
    tab_bar_shown: Option<u64>,
}

impl LogViewApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let notify: Notify = Arc::new(move || ctx.request_repaint());
        let bold_family = crate::fonts::install(&cc.egui_ctx);
        // Converted files left by a session that did not exit cleanly.
        logview_core::prepare::clean_temp_dir(std::time::Duration::from_secs(24 * 3600));
        let first = Tab::new(0, Arc::clone(&notify), bold_family.clone());
        let mut app = Self {
            tabs: vec![first],
            active: 0,
            next_id: 1,
            notify,
            bold_family,
            show_details: true,
            show_filter_panel: true,
            show_timeline: true,
            show_help: false,
            title: String::new(),
            error: None,
            profile_ui: ProfileUi::default(),
            merge_dialog: None,
            tab_bar_shown: None,
        };
        app.init_config(&cc.egui_ctx, initial);
        app
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    /// Adds a tab with the settings of the active tab and makes it active.
    fn add_tab(&mut self) -> &mut Tab {
        let settings = self.tab().snapshot_profile();
        let active_profile = self.tab().active_profile.clone();
        let mut tab = Tab::new(
            self.next_id,
            Arc::clone(&self.notify),
            self.bold_family.clone(),
        );
        self.next_id += 1;
        tab.apply_profile(&settings);
        tab.active_profile = active_profile;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.tab_mut()
    }

    /// Opens `path` in a new tab (or in the active tab if it is empty).
    /// With `auto_profile`, a profile matching the file name is loaded.
    fn open(&mut self, path: PathBuf, auto_profile: bool) -> bool {
        self.open_as(path, auto_profile, TextEncoding::Auto)
    }

    /// Like [`open`](Self::open), reading the file as `encoding`.
    fn open_as(&mut self, path: PathBuf, auto_profile: bool, encoding: TextEncoding) -> bool {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.file.as_ref().is_some_and(|f| f.path() == path))
        {
            // Already open: switch to it.
            self.active = i;
            return true;
        }
        let reuse = self.tab().file.is_none();
        if !reuse {
            self.add_tab();
        }
        self.tab_mut().encoding = encoding;
        let opened = self.tab_mut().open(path.clone());
        if opened {
            Session::add_recent(&mut self.profile_ui.recent, &path);
            if auto_profile {
                self.auto_profile(&path);
            }
        } else {
            self.profile_ui.recent.retain(|p| p != &path);
            if !reuse {
                // Keep the error visible on the previous tab.
                let error = self.tab_mut().error.take();
                self.close_tab(self.active);
                self.error = error;
            }
        }
        opened
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            let tab = Tab::new(
                self.next_id,
                Arc::clone(&self.notify),
                self.bold_family.clone(),
            );
            self.next_id += 1;
            self.tabs.push(tab);
        }
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
    }

    fn pick_files(&mut self) {
        let dialog = rfd::FileDialog::new()
            .set_title("Otwórz pliki logów")
            .add_filter("Logi", &["log", "txt", "out", "err", "json", "jsonl"])
            .add_filter("Wszystkie pliki", &["*"]);
        for path in dialog.pick_files().unwrap_or_default() {
            self.open(path, true);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.tab().file {
            Some(f) => format!("{} — logview", f.path().display()),
            None => "logview".to_owned(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    // ---- tabs bar ----------------------------------------------------------------

    fn tab_bar(&mut self, ctx: &egui::Context) {
        let mut select = None;
        let mut close = None;
        let mut open = false;
        let active_id = self.tabs.get(self.active).map(|t| t.id);
        let scroll_to_active = self.tab_bar_shown != active_id;
        self.tab_bar_shown = active_id;
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Fixed buttons on the right; the tabs scroll in the rest of
                // the row (mouse wheel, no scroll bar eating the bar's height).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("+")
                        .on_hover_text("Otwórz pliki (Ctrl+O)")
                        .clicked()
                    {
                        open = true;
                    }
                    if self.tabs.len() > 1 {
                        ui.menu_button(format!("▾ {}", self.tabs.len()), |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(ui.ctx().screen_rect().height() * 0.6)
                                .show(ui, |ui| {
                                    for (i, tab) in self.tabs.iter().enumerate() {
                                        if ui
                                            .selectable_label(i == self.active, tab_title(tab))
                                            .clicked()
                                        {
                                            select = Some(i);
                                            ui.close_menu();
                                        }
                                    }
                                });
                        })
                        .response
                        .on_hover_text("Lista wszystkich kart");
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        egui::ScrollArea::horizontal()
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    for (i, tab) in self.tabs.iter().enumerate() {
                                        let hover = tab.file.as_ref().map_or_else(
                                            || "pusta karta".to_owned(),
                                            |f| f.path().display().to_string(),
                                        );
                                        let resp = ui
                                            .selectable_label(i == self.active, tab_title(tab))
                                            .on_hover_text(hover);
                                        if resp.clicked() {
                                            select = Some(i);
                                        }
                                        if resp.middle_clicked() {
                                            close = Some(i);
                                        }
                                        let x = ui
                                            .small_button("×")
                                            .on_hover_text("Zamknij kartę (Ctrl+W)");
                                        if x.clicked() {
                                            close = Some(i);
                                        }
                                        if scroll_to_active && i == self.active {
                                            resp.union(x).scroll_to_me(None);
                                        }
                                        ui.separator();
                                    }
                                });
                            });
                    });
                });
            });
        });
        if let Some(i) = select {
            self.active = i;
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if open {
            self.pick_files();
        }
    }

    // ---- merge ---------------------------------------------------------------------

    fn open_merge_dialog(&mut self) {
        self.merge_dialog = Some(MergeDialog {
            selected: self
                .tabs
                .iter()
                .filter(|t| {
                    t.file
                        .as_ref()
                        .is_some_and(|f| f.merged_sources().is_none())
                })
                .map(|t| (t.id, t.merge_ready()))
                .collect(),
        });
    }

    fn merge_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.merge_dialog else {
            return;
        };
        let mut merge = false;
        let mut cancel = false;
        egui::Window::new("Scal karty po czasie")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(
                    "Rekordy wybranych plików zostaną połączone w kolejności znaczników czasu.",
                );
                ui.weak("Pliki muszą być zaindeksowane i mieć rozpoznany format z polem czasu.");
                ui.add_space(4.0);
                for (id, on) in &mut dialog.selected {
                    let Some(tab) = self.tabs.iter().find(|t| t.id == *id) else {
                        continue;
                    };
                    let ready = tab.merge_ready();
                    let label = match &tab.format {
                        Some(f) => format!("{} ({})", tab.title(), f.name),
                        None => tab.title(),
                    };
                    ui.add_enabled(ready, egui::Checkbox::new(on, label))
                        .on_disabled_hover_text("Plik jeszcze się wczytuje lub analizuje");
                    if !ready {
                        *on = false;
                    }
                }
                let count = dialog.selected.iter().filter(|(_, on)| *on).count();
                if count > logview_core::merge::MAX_SOURCES {
                    ui.colored_label(ui.visuals().error_fg_color, "Za dużo plików (maks. 255).");
                }
                ui.horizontal(|ui| {
                    merge = ui
                        .add_enabled(count >= 2, egui::Button::new(format!("Scal ({count})")))
                        .clicked();
                    cancel = ui.button("Anuluj").clicked();
                });
            });
        let ids: Vec<u64> = dialog
            .selected
            .iter()
            .filter(|(_, on)| *on)
            .map(|(id, _)| *id)
            .collect();
        if cancel || merge {
            self.merge_dialog = None;
        }
        if !merge {
            return;
        }
        let chosen: Vec<&Tab> = ids
            .iter()
            .filter_map(|id| self.tabs.iter().find(|t| t.id == *id))
            .collect();
        let sources: Vec<MergeSource> = chosen
            .iter()
            .filter_map(|t| {
                Some(MergeSource {
                    file: t.file.clone()?,
                    format: Arc::clone(t.format.as_ref()?),
                    records: t.records_handle()?,
                    name: t.title(),
                })
            })
            .collect();
        if sources.len() < 2 {
            return;
        }
        let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
        let name = format!("Scalone: {}", names.join(" + "));
        let settings_from = chosen[0].id;
        let file = merge_files(sources, &name, Arc::clone(&self.notify));
        if let Some(i) = self.tabs.iter().position(|t| t.id == settings_from) {
            self.active = i;
        }
        let tab = self.add_tab();
        tab.set_file(file);
        tab.set_notice("Scalanie w toku – wiersze pojawiają się na bieżąco".into());
    }

    // ---- menu -----------------------------------------------------------------------

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Plik", |ui| {
                    if ui
                        .add(egui::Button::new("Otwórz…").shortcut_text("Ctrl+O"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.pick_files();
                    }
                    if let Some(path) = self.recent_menu(ui) {
                        self.open(path, true);
                    }
                    ui.separator();
                    let mergeable = self
                        .tabs
                        .iter()
                        .filter(|t| {
                            t.file
                                .as_ref()
                                .is_some_and(|f| f.merged_sources().is_none())
                        })
                        .count();
                    if ui
                        .add_enabled(mergeable >= 2, egui::Button::new("Scal karty po czasie…"))
                        .on_disabled_hover_text("Otwórz co najmniej dwa pliki")
                        .clicked()
                    {
                        ui.close_menu();
                        self.open_merge_dialog();
                    }
                    ui.separator();
                    if ui
                        .add(egui::Button::new("Zamknij kartę").shortcut_text("Ctrl+W"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.close_tab(self.active);
                    }
                    if ui.button("Zakończ").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Szukaj", |ui| {
                    let tab = &mut self.tabs[self.active];
                    if ui
                        .add(egui::Button::new("Szukaj…").shortcut_text("Ctrl+F"))
                        .clicked()
                    {
                        ui.close_menu();
                        tab.search.open_and_focus();
                    }
                    if ui
                        .add(egui::Button::new("Następne").shortcut_text("F3"))
                        .clicked()
                    {
                        ui.close_menu();
                        tab.step_search(true);
                    }
                    if ui
                        .add(egui::Button::new("Poprzednie").shortcut_text("Shift+F3"))
                        .clicked()
                    {
                        ui.close_menu();
                        tab.step_search(false);
                    }
                });
                ui.menu_button("Widok", |ui| {
                    let tab = &mut self.tabs[self.active];
                    ui.checkbox(&mut tab.prefs.wrap, "Zawijanie wierszy");
                    let mut follow = tab.follow;
                    let can_follow = tab
                        .file
                        .as_ref()
                        .is_some_and(|f| f.merged_sources().is_none() && f.conversion().is_none());
                    if ui
                        .add_enabled(
                            can_follow,
                            egui::Checkbox::new(&mut follow, "Śledź koniec pliku (Ctrl+L)"),
                        )
                        .changed()
                    {
                        tab.set_follow(follow);
                    }
                    let has_columns = tab.format.as_ref().is_some_and(|f| !f.columns().is_empty());
                    ui.add_enabled(
                        has_columns,
                        egui::Checkbox::new(&mut tab.prefs.columns, "Widok kolumn (Ctrl+T)"),
                    );
                    let mut filtered = tab.show_filtered;
                    if ui
                        .checkbox(&mut filtered, "Tylko pasujące linie (Ctrl+H)")
                        .changed()
                    {
                        tab.toggle_filtered_view();
                    }
                    ui.checkbox(&mut self.show_details, "Panel szczegółów");
                    ui.checkbox(&mut self.show_filter_panel, "Panel filtrów");
                    ui.checkbox(&mut self.show_timeline, "Oś czasu (Ctrl+J)");
                    ui.separator();
                    let tab = &mut self.tabs[self.active];
                    ui.horizontal(|ui| {
                        ui.label("Czcionka:");
                        ui.add(
                            egui::DragValue::new(&mut tab.prefs.font_size)
                                .range(6.0..=48.0)
                                .speed(0.25),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Limit długości linii (B):");
                        ui.add(
                            egui::DragValue::new(&mut tab.prefs.max_display_bytes)
                                .range(80..=1_000_000)
                                .speed(64),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Szerokość tabulatora:");
                        ui.add(egui::DragValue::new(&mut tab.prefs.tab_width).range(1..=16));
                    });
                    ui.separator();
                    ui.label("Motyw:");
                    ui.horizontal(|ui| {
                        if ui.button("Systemowy").clicked() {
                            self.set_theme(ctx, Theme::System);
                        }
                        if ui.button("Jasny").clicked() {
                            self.set_theme(ctx, Theme::Light);
                        }
                        if ui.button("Ciemny").clicked() {
                            self.set_theme(ctx, Theme::Dark);
                        }
                    });
                });
                ui.menu_button("Profile", |ui| self.profile_menu(ui));
                ui.menu_button("Pomoc", |ui| {
                    if ui
                        .add(egui::Button::new("Skróty klawiszowe").shortcut_text("F1"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.show_help = true;
                    }
                });
            });
        });
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("Skróty klawiszowe")
            .open(&mut self.show_help)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::Grid::new("shortcuts").striped(true).show(ui, |ui| {
                    for (keys, action) in [
                        ("Ctrl+O", "otwórz pliki (lub przeciągnij pliki na okno)"),
                        ("Ctrl+W, środkowy przycisk", "zamknij kartę"),
                        ("Ctrl+Tab / Ctrl+Shift+Tab", "następna / poprzednia karta"),
                        ("↑ / ↓, kółko myszy", "przewiń o linię"),
                        ("PgUp / PgDn", "przewiń o stronę"),
                        ("Home / End", "początek / koniec pliku"),
                        ("← / →, Shift+kółko", "przewijanie w poziomie"),
                        ("Ctrl+kółko", "rozmiar czcionki"),
                        (
                            "klik / Shift+klik / Ctrl+klik",
                            "zaznacz / zakres / dodaj-usuń",
                        ),
                        ("Ctrl+A", "zaznacz wszystko"),
                        ("Ctrl+C", "kopiuj zaznaczone linie"),
                        ("Esc", "wyczyść zaznaczenie"),
                        ("Ctrl+H", "tylko pasujące / cały plik"),
                        ("Enter w polu filtra", "zastosuj od razu"),
                        (
                            "Ctrl+F",
                            "szukaj (Enter / Shift+Enter: następne / poprzednie)",
                        ),
                        ("F3 / Shift+F3", "następne / poprzednie trafienie"),
                        ("Ctrl+B, klik w numer linii", "zakładka na linii"),
                        ("F2 / Shift+F2", "następna / poprzednia zakładka"),
                        ("Ctrl+G", "przejdź do linii lub czasu"),
                        (
                            "Ctrl+J",
                            "oś czasu (klik: przejdź, przeciągnij: filtr czasu)",
                        ),
                        ("Ctrl+T", "widok kolumn (pola formatu)"),
                        (
                            "Ctrl+L",
                            "śledź koniec pliku (live tail); End wraca na koniec",
                        ),
                        (
                            "klik w pasek przewijania",
                            "skok do miejsca (znaczniki reguł i trafień)",
                        ),
                        ("F1", "ta pomoc"),
                    ] {
                        ui.monospace(keys);
                        ui.label(action);
                        ui.end_row();
                    }
                });
            });
    }

    fn error_windows(&mut self, ctx: &egui::Context) {
        let error = self.error.clone().or_else(|| self.tab().error.clone());
        let Some(error) = error else { return };
        egui::Window::new("Błąd")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(error);
                if ui.button("OK").clicked() {
                    if self.error.is_some() {
                        self.error = None;
                    } else {
                        self.tab_mut().error = None;
                    }
                }
            });
    }
}

impl eframe::App for LogViewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        for path in dropped {
            self.open(path, true);
        }
        let (open, help, close, next_tab, prev_tab) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::NONE, Key::F1),
                i.consume_key(Modifiers::COMMAND, Key::W),
                i.consume_key(Modifiers::COMMAND, Key::Tab),
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab),
            )
        });
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::J)) {
            self.show_timeline = !self.show_timeline;
        }
        if open {
            self.pick_files();
        }
        if help {
            self.show_help = !self.show_help;
        }
        if close {
            self.close_tab(self.active);
        }
        if next_tab {
            self.active = (self.active + 1) % self.tabs.len();
        }
        if prev_tab {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }

        for tab in &mut self.tabs {
            tab.frame(ctx);
        }
        self.tab_mut().handle_keys(ctx);

        self.update_title(ctx);
        self.menu_bar(ctx);
        self.tab_bar(ctx);
        let (details, filters, timeline) = (
            self.show_details,
            self.show_filter_panel,
            self.show_timeline,
        );
        self.tab_mut().draw(ctx, details, filters, timeline);

        self.error_windows(ctx);
        self.help_window(ctx);
        self.merge_window(ctx);
        self.profile_windows(ctx);

        let closing = ctx.input(|i| i.viewport().close_requested());
        self.save_session_if_changed(ctx, closing);
    }
}

/// Title of a tab in the tab bar: `●` when following, `⧉` for merged views.
fn tab_title(tab: &Tab) -> String {
    let mut title = tab.title();
    if tab.follow {
        title = format!("● {title}");
    }
    if tab
        .file
        .as_ref()
        .is_some_and(|f| f.merged_sources().is_some())
    {
        title = format!("⧉ {title}");
    }
    title
}
