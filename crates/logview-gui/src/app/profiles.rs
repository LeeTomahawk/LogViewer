//! Profiles, session save/restore and recent files. Profile operations act
//! on the active tab; every tab has its own settings and active profile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use logview_core::profile::write_atomic;
use logview_core::{Profile, StoredProfile};

use super::LogViewApp;
use crate::config::{Config, SavedBookmark, Session, SessionTab, Theme};

/// Session changes are written at most this often (and always on exit).
const SESSION_SAVE_DELAY: Duration = Duration::from_secs(2);

/// State of the profile dialogs.
#[derive(Default)]
pub struct ProfileUi {
    pub config: Option<Config>,
    pub profiles: Vec<StoredProfile>,
    pub recent: Vec<PathBuf>,
    pub theme: Theme,
    saved_session: Option<Session>,
    dirty_since: Option<Instant>,
    save_as: Option<SaveAs>,
    manage_open: bool,
    /// Edited file patterns in the manage window, per profile file.
    pattern_edits: HashMap<PathBuf, String>,
    confirm_delete: Option<PathBuf>,
}

struct SaveAs {
    name: String,
    patterns: String,
}

enum ManageAction {
    Load(Profile),
    SavePatterns(Profile, String),
    Export(Profile),
    Delete(PathBuf),
    Import,
    Refresh,
}

fn patterns_text(patterns: &[String]) -> String {
    patterns.join(", ")
}

fn parse_patterns(text: &str) -> Vec<String> {
    text.split([',', ';'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    });
}

impl LogViewApp {
    /// Loads the configuration and the previous session; opens `initial`
    /// (choosing profiles by file name) or the tabs of the last session.
    pub(super) fn init_config(&mut self, ctx: &egui::Context, initial: Vec<PathBuf>) {
        let mut restore = Vec::new();
        let mut active_tab = 0;
        if let Some(config) = Config::locate() {
            if let Err(e) = config.seed_examples() {
                self.error = Some(format!("Nie można utworzyć przykładowych profili: {e}"));
            }
            let (session, error) = config.load_session();
            if error.is_some() {
                self.error = error;
            }
            self.profile_ui.config = Some(config);
            self.refresh_profiles();
            self.profile_ui.theme = session.theme;
            apply_theme(ctx, session.theme);
            self.show_filter_panel = session.show_filter_panel;
            self.show_details = session.show_details;
            self.show_timeline = session.show_timeline;
            self.profile_ui.recent = session.recent_files.clone();
            // Settings for new tabs.
            let active_profile = session
                .active_profile
                .as_deref()
                .and_then(|name| self.find_profile(name));
            let tab = self.tab_mut();
            tab.apply_profile(&session.current);
            tab.active_profile = active_profile;
            restore = session.tabs_to_restore();
            active_tab = session.active_tab;
            self.profile_ui.saved_session = Some(session);
        }
        if !initial.is_empty() {
            for path in initial {
                self.open(path, true);
            }
            return;
        }
        let mut opened = Vec::new();
        for saved in restore {
            if !saved.path.exists() || !self.open_as(saved.path.clone(), false, saved.encoding) {
                continue;
            }
            let active_profile = saved
                .active_profile
                .as_deref()
                .and_then(|name| self.find_profile(name));
            let tab = self.tab_mut();
            tab.apply_profile(&saved.settings);
            tab.active_profile = active_profile;
            tab.bookmarks = saved
                .bookmarks
                .iter()
                .map(|b| (b.line, b.note.clone()))
                .collect();
            if saved.follow {
                tab.set_follow(true);
            } else if let Some(line) = saved.line {
                tab.view.scroll_to_line(line);
            }
            opened.push(self.active);
        }
        if let Some(&index) = opened.get(active_tab) {
            self.active = index;
        }
    }

    pub(super) fn refresh_profiles(&mut self) {
        self.profile_ui.profiles = self
            .profile_ui
            .config
            .as_ref()
            .map(|c| c.store.list())
            .unwrap_or_default();
    }

    fn find_profile(&self, name: &str) -> Option<Profile> {
        self.profile_ui
            .profiles
            .iter()
            .filter_map(|s| s.profile.as_ref().ok())
            .find(|p| p.name == name)
            .cloned()
    }

    /// After opening `path` in the active tab: picks the first profile whose
    /// patterns match the file name, unless it is already active there with
    /// unchanged settings.
    pub(super) fn auto_profile(&mut self, path: &Path) {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return;
        };
        let found = self
            .profile_ui
            .profiles
            .iter()
            .filter_map(|s| s.profile.as_ref().ok())
            .find(|p| p.matches_file(&name))
            .cloned();
        let Some(profile) = found else { return };
        let tab = self.tab_mut();
        let current = tab.snapshot_profile();
        let already = tab
            .active_profile
            .as_ref()
            .is_some_and(|a| a.name == profile.name && profile.same_settings(&current));
        if !already {
            tab.apply_profile(&profile);
            tab.set_notice(format!("Profil „{}” dobrany do pliku {name}", profile.name));
            tab.active_profile = Some(profile);
        }
    }

    fn save_profile(&mut self, name: String, patterns: Vec<String>) {
        let Some(config) = &self.profile_ui.config else {
            self.error = Some("Brak folderu konfiguracji – użyj „Eksportuj do pliku…”".into());
            return;
        };
        let profile = Profile {
            name,
            file_patterns: patterns,
            ..self.tabs[self.active].snapshot_profile()
        };
        match config.store.save(&profile) {
            Ok(_) => {
                let tab = self.tab_mut();
                tab.set_notice(format!("Zapisano profil „{}”", profile.name));
                tab.active_profile = Some(profile);
                self.refresh_profiles();
            }
            Err(e) => self.error = Some(format!("Nie można zapisać profilu: {e}")),
        }
    }

    /// Copies the active tab's settings (and active profile) to all tabs.
    fn apply_to_all_tabs(&mut self) {
        let settings = self.tab().snapshot_profile();
        let active_profile = self.tab().active_profile.clone();
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i != active {
                tab.apply_profile(&settings);
                tab.active_profile = active_profile.clone();
            }
        }
        let count = self.tabs.len() - 1;
        self.tab_mut()
            .set_notice(format!("Ustawienia skopiowane do {count} kart"));
    }

    fn export_profile(&mut self, profile: &Profile) {
        let name = if profile.name.is_empty() {
            "profil".to_owned()
        } else {
            logview_core::profile::file_stem_for(&profile.name)
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Eksportuj profil")
            .add_filter("Profil (TOML)", &["toml"])
            .set_file_name(format!("{name}.toml"))
            .save_file()
        else {
            return;
        };
        let mut profile = profile.clone();
        if profile.name.is_empty() {
            profile.name = name;
        }
        let result = profile
            .to_toml()
            .and_then(|text| write_atomic(&path, &text).map_err(|e| e.to_string()));
        match result {
            Ok(()) => self
                .tab_mut()
                .set_notice(format!("Wyeksportowano profil do {}", path.display())),
            Err(e) => self.error = Some(format!("Eksport profilu nie powiódł się: {e}")),
        }
    }

    fn import_profile(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Importuj profil")
            .add_filter("Profil (TOML)", &["toml"])
            .pick_file()
        else {
            return;
        };
        let profile = match Profile::load(&path) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(format!("Nie można wczytać profilu: {e}"));
                return;
            }
        };
        if let Some(config) = &self.profile_ui.config
            && let Err(e) = config.store.save(&profile)
        {
            self.error = Some(format!("Nie można zapisać profilu: {e}"));
        }
        self.refresh_profiles();
        self.tab_mut().load_profile(profile);
    }

    // ---- session ---------------------------------------------------------------

    fn session_snapshot(&self) -> Session {
        // Merged views are not restored.
        let saved: Vec<(usize, SessionTab)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, tab)| {
                let file = tab.file.as_ref()?;
                if file.merged_sources().is_some() {
                    return None;
                }
                Some((
                    i,
                    SessionTab {
                        path: file.path().to_path_buf(),
                        line: tab.view.top_line(),
                        follow: tab.follow,
                        active_profile: tab.active_profile.as_ref().map(|p| p.name.clone()),
                        settings: tab.snapshot_profile(),
                        encoding: tab.encoding,
                        bookmarks: tab
                            .bookmarks
                            .iter()
                            .map(|(&line, note)| SavedBookmark {
                                line,
                                note: note.clone(),
                            })
                            .collect(),
                    },
                ))
            })
            .collect();
        let active_tab = saved
            .iter()
            .position(|(i, _)| *i == self.active)
            .unwrap_or(0);
        Session {
            recent_files: self.profile_ui.recent.clone(),
            theme: self.profile_ui.theme,
            show_filter_panel: self.show_filter_panel,
            show_details: self.show_details,
            show_timeline: self.show_timeline,
            current: self.tab().snapshot_profile(),
            active_profile: self.tab().active_profile.as_ref().map(|p| p.name.clone()),
            tabs: saved.into_iter().map(|(_, t)| t).collect(),
            active_tab,
            ..Default::default()
        }
    }

    /// Writes the session when it changed (debounced) or when `force`.
    pub(super) fn save_session_if_changed(&mut self, ctx: &egui::Context, force: bool) {
        if self.profile_ui.config.is_none() {
            return;
        }
        let snapshot = self.session_snapshot();
        if self.profile_ui.saved_session.as_ref() == Some(&snapshot) {
            self.profile_ui.dirty_since = None;
            return;
        }
        let since = *self.profile_ui.dirty_since.get_or_insert_with(Instant::now);
        let waited = since.elapsed();
        if !force && waited < SESSION_SAVE_DELAY {
            ctx.request_repaint_after(SESSION_SAVE_DELAY - waited);
            return;
        }
        if let Some(config) = &self.profile_ui.config
            && let Err(e) = config.save_session(&snapshot)
        {
            self.tab_mut()
                .set_notice(format!("Nie można zapisać sesji: {e}"));
        }
        self.profile_ui.saved_session = Some(snapshot);
        self.profile_ui.dirty_since = None;
    }

    pub(super) fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        self.profile_ui.theme = theme;
        apply_theme(ctx, theme);
    }

    // ---- menus and windows --------------------------------------------------------

    /// "Ostatnie pliki" submenu; returns a file to open.
    pub(super) fn recent_menu(&mut self, ui: &mut egui::Ui) -> Option<PathBuf> {
        let mut open = None;
        ui.add_enabled_ui(!self.profile_ui.recent.is_empty(), |ui| {
            ui.menu_button("Ostatnie pliki", |ui| {
                for path in &self.profile_ui.recent {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    if ui
                        .button(name)
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        open = Some(path.clone());
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button("Wyczyść listę").clicked() {
                    self.profile_ui.recent.clear();
                    ui.close_menu();
                }
            });
        });
        open
    }

    pub(super) fn profile_menu(&mut self, ui: &mut egui::Ui) {
        let mut load = None;
        let active_name = self.tab().active_profile.as_ref().map(|p| p.name.clone());
        if self.profile_ui.profiles.is_empty() {
            ui.weak("Brak zapisanych profili");
        }
        for stored in &self.profile_ui.profiles {
            match &stored.profile {
                Ok(p) => {
                    let selected = active_name.as_deref() == Some(p.name.as_str());
                    let mut button = ui.selectable_label(selected, &p.name);
                    if !p.file_patterns.is_empty() {
                        button = button
                            .on_hover_text(format!("Pliki: {}", patterns_text(&p.file_patterns)));
                    }
                    if button.clicked() {
                        load = Some(p.clone());
                        ui.close_menu();
                    }
                }
                Err(e) => {
                    let name = stored
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.add_enabled(false, egui::Button::new(format!("⚠ {name}")))
                        .on_disabled_hover_text(e);
                }
            }
        }
        ui.separator();
        if ui
            .add_enabled(active_name.is_some(), egui::Button::new("Zapisz profil"))
            .on_hover_text("Nadpisuje aktywny profil bieżącymi ustawieniami")
            .clicked()
            && let Some(active) = self.tab().active_profile.clone()
        {
            self.save_profile(active.name, active.file_patterns);
            ui.close_menu();
        }
        if ui.button("Zapisz jako…").clicked() {
            let active = self.tabs[self.active].active_profile.as_ref();
            self.profile_ui.save_as = Some(SaveAs {
                name: active.map(|p| p.name.clone()).unwrap_or_default(),
                patterns: active
                    .map(|p| patterns_text(&p.file_patterns))
                    .unwrap_or_default(),
            });
            ui.close_menu();
        }
        if ui.button("Zarządzaj profilami…").clicked() {
            self.profile_ui.manage_open = true;
            self.profile_ui.pattern_edits.clear();
            self.refresh_profiles();
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Importuj z pliku…").clicked() {
            ui.close_menu();
            self.import_profile();
        }
        if ui.button("Eksportuj bieżące ustawienia…").clicked() {
            ui.close_menu();
            let profile = self.tab().snapshot_profile();
            self.export_profile(&profile);
        }
        ui.separator();
        if ui
            .button("Ustawienia domyślne")
            .on_hover_text("Wyłącza profil i przywraca domyślne filtry, kolory i format")
            .clicked()
        {
            let tab = self.tab_mut();
            tab.apply_profile(&Profile::default());
            tab.active_profile = None;
            ui.close_menu();
        }
        if ui
            .add_enabled(
                self.tabs.len() > 1,
                egui::Button::new("Zastosuj do wszystkich kart"),
            )
            .on_hover_text(
                "Kopiuje ustawienia tej karty (format, filtry, kolory, widok) do pozostałych",
            )
            .clicked()
        {
            self.apply_to_all_tabs();
            ui.close_menu();
        }
        if let Some(p) = load {
            self.tab_mut().load_profile(p);
        }
    }

    pub(super) fn profile_windows(&mut self, ctx: &egui::Context) {
        self.save_as_window(ctx);
        self.manage_window(ctx);
    }

    fn save_as_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.profile_ui.save_as else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        let exists = self
            .profile_ui
            .profiles
            .iter()
            .filter_map(|s| s.profile.as_ref().ok())
            .any(|p| p.name == dialog.name.trim());
        egui::Window::new("Zapisz profil")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Grid::new("save-as").num_columns(2).show(ui, |ui| {
                    ui.label("Nazwa:");
                    let name = ui.add(egui::TextEdit::singleline(&mut dialog.name).desired_width(260.0));
                    if name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        save = true;
                    }
                    ui.end_row();
                    ui.label("Pliki:");
                    ui.add(
                        egui::TextEdit::singleline(&mut dialog.patterns)
                            .hint_text("np. *nginx*.log, access*.log")
                            .desired_width(260.0),
                    )
                    .on_hover_text("Wzorce nazw plików (* i ?), oddzielone przecinkami – profil będzie wybierany automatycznie");
                    ui.end_row();
                });
                if exists {
                    ui.colored_label(ui.visuals().warn_fg_color, "Nadpisze istniejący profil o tej nazwie.");
                }
                ui.horizontal(|ui| {
                    save |= ui
                        .add_enabled(!dialog.name.trim().is_empty(), egui::Button::new("Zapisz"))
                        .clicked();
                    cancel = ui.button("Anuluj").clicked();
                });
            });
        if save && !dialog.name.trim().is_empty() {
            let name = dialog.name.trim().to_owned();
            let patterns = parse_patterns(&dialog.patterns);
            self.profile_ui.save_as = None;
            self.save_profile(name, patterns);
        } else if cancel {
            self.profile_ui.save_as = None;
        }
    }

    fn manage_window(&mut self, ctx: &egui::Context) {
        if !self.profile_ui.manage_open {
            return;
        }
        let mut open = true;
        let mut actions = Vec::new();
        let active_name = self.tab().active_profile.as_ref().map(|p| p.name.clone());
        let ui_state = &mut self.profile_ui;
        egui::Window::new("Profile")
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                if let Some(config) = &ui_state.config {
                    ui.horizontal(|ui| {
                        ui.label("Folder:");
                        ui.monospace(config.store.dir().display().to_string());
                    });
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        egui::Grid::new("profiles-grid")
                            .num_columns(3)
                            .striped(true)
                            .spacing([10.0, 6.0])
                            .show(ui, |ui| {
                                ui.strong("Nazwa");
                                ui.strong("Wzorce plików");
                                ui.strong("");
                                ui.end_row();
                                for stored in &ui_state.profiles {
                                    match &stored.profile {
                                        Ok(p) => {
                                            let label = if active_name.as_deref()
                                                == Some(p.name.as_str())
                                            {
                                                format!("{} ✓", p.name)
                                            } else {
                                                p.name.clone()
                                            };
                                            ui.label(label);
                                            let text = ui_state
                                                .pattern_edits
                                                .entry(stored.path.clone())
                                                .or_insert_with(|| patterns_text(&p.file_patterns));
                                            ui.add(
                                                egui::TextEdit::singleline(text)
                                                    .hint_text("brak – wybierany ręcznie")
                                                    .desired_width(220.0),
                                            );
                                            let edited = parse_patterns(text) != p.file_patterns;
                                            ui.horizontal(|ui| {
                                                if ui.button("Wczytaj").clicked() {
                                                    actions.push(ManageAction::Load(p.clone()));
                                                }
                                                if ui
                                                    .add_enabled(
                                                        edited,
                                                        egui::Button::new("Zapisz wzorce"),
                                                    )
                                                    .clicked()
                                                {
                                                    actions.push(ManageAction::SavePatterns(
                                                        p.clone(),
                                                        text.clone(),
                                                    ));
                                                }
                                                if ui.button("Eksportuj…").clicked() {
                                                    actions.push(ManageAction::Export(p.clone()));
                                                }
                                                delete_button(
                                                    ui,
                                                    &mut ui_state.confirm_delete,
                                                    &stored.path,
                                                    &mut actions,
                                                );
                                            });
                                        }
                                        Err(e) => {
                                            ui.colored_label(
                                                ui.visuals().error_fg_color,
                                                stored
                                                    .path
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().into_owned())
                                                    .unwrap_or_default(),
                                            )
                                            .on_hover_text(e);
                                            ui.weak("błąd odczytu (najedź na nazwę)");
                                            ui.horizontal(|ui| {
                                                delete_button(
                                                    ui,
                                                    &mut ui_state.confirm_delete,
                                                    &stored.path,
                                                    &mut actions,
                                                );
                                            });
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Importuj…").clicked() {
                        actions.push(ManageAction::Import);
                    }
                    if ui.button("Odśwież").clicked() {
                        actions.push(ManageAction::Refresh);
                    }
                });
            });
        self.profile_ui.manage_open = open;
        for action in actions {
            match action {
                ManageAction::Load(p) => self.tab_mut().load_profile(p),
                ManageAction::SavePatterns(mut p, text) => {
                    p.file_patterns = parse_patterns(&text);
                    let saved = self.profile_ui.config.as_ref().map(|c| c.store.save(&p));
                    match saved {
                        Some(Ok(_)) => {
                            for tab in &mut self.tabs {
                                if let Some(active) = &mut tab.active_profile
                                    && active.name == p.name
                                {
                                    active.file_patterns = p.file_patterns.clone();
                                }
                            }
                            self.tab_mut()
                                .set_notice(format!("Zapisano wzorce profilu „{}”", p.name));
                        }
                        Some(Err(e)) => self.error = Some(e),
                        None => {}
                    }
                    self.refresh_profiles();
                    self.profile_ui.pattern_edits.clear();
                }
                ManageAction::Export(p) => self.export_profile(&p),
                ManageAction::Delete(path) => {
                    let deleted = self
                        .profile_ui
                        .config
                        .as_ref()
                        .map(|c| c.store.delete(&path));
                    if let Some(Err(e)) = deleted {
                        self.error = Some(e);
                    }
                    self.profile_ui.confirm_delete = None;
                    self.refresh_profiles();
                }
                ManageAction::Import => self.import_profile(),
                ManageAction::Refresh => {
                    self.refresh_profiles();
                    self.profile_ui.pattern_edits.clear();
                }
            }
        }
    }
}

/// "Usuń" button that asks for confirmation with a second click.
fn delete_button(
    ui: &mut egui::Ui,
    confirm: &mut Option<PathBuf>,
    path: &Path,
    actions: &mut Vec<ManageAction>,
) {
    if confirm.as_deref() == Some(path) {
        if ui
            .button(egui::RichText::new("Na pewno usunąć?").color(ui.visuals().error_fg_color))
            .clicked()
        {
            actions.push(ManageAction::Delete(path.to_path_buf()));
        }
    } else if ui.button("Usuń").clicked() {
        *confirm = Some(path.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_parse_and_join() {
        assert_eq!(parse_patterns(" *.log, a?.txt;; "), ["*.log", "a?.txt"]);
        assert_eq!(patterns_text(&parse_patterns("*.a,*.b")), "*.a, *.b");
        assert!(parse_patterns("  ").is_empty());
    }
}
