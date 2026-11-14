//! Editing of the filter chain, with debounced application.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, RichText};
use logview_core::{FilterRule, FilterSpec, IncludeMode, Level, PatternKind, RuleError, time};

use crate::fmt;

/// Delay between the last keystroke in a pattern and applying the filter.
const DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_CONTEXT: usize = 10_000;

pub struct FilterPanel {
    pub spec: FilterSpec,
    /// Compile error per rule (same length as `spec.rules`).
    errors: Vec<Option<String>>,
    /// When the edited spec should be applied.
    due_at: Option<Instant>,
    time_from_text: String,
    time_to_text: String,
    time_error: Option<String>,
}

impl Default for FilterPanel {
    fn default() -> Self {
        Self {
            spec: FilterSpec {
                rules: vec![FilterRule::default()],
                ..Default::default()
            },
            errors: vec![None],
            due_at: None,
            time_from_text: String::new(),
            time_to_text: String::new(),
            time_error: None,
        }
    }
}

/// Record statistics shown next to the level and time controls.
#[derive(Default)]
pub struct MetaInfo {
    /// Records per level (index 0 = no level).
    pub level_counts: Option<[usize; 7]>,
    /// Earliest and latest timestamp in seconds.
    pub time_bounds: Option<(i64, i64)>,
}

/// Label color of a level (same hues as the default highlight rules).
pub fn level_color(level: Option<Level>) -> Color32 {
    match level {
        Some(Level::Fatal | Level::Error) => Color32::from_rgb(0xF4, 0x43, 0x36),
        Some(Level::Warn) => Color32::from_rgb(0xFF, 0x98, 0x00),
        Some(Level::Info) => Color32::from_rgb(0x4C, 0xAF, 0x50),
        Some(Level::Debug) => Color32::from_rgb(0x42, 0xA5, 0xF5),
        Some(Level::Trace) | None => Color32::from_rgb(0x9E, 0x9E, 0x9E),
    }
}

enum RowAction {
    Up(usize),
    Down(usize),
    Remove(usize),
}

impl FilterPanel {
    /// Returns `true` once when the edited filter should be applied.
    pub fn take_due(&mut self) -> bool {
        match self.due_at {
            Some(at) if at <= Instant::now() => {
                self.due_at = None;
                true
            }
            _ => false,
        }
    }

    /// Time left until the pending change is applied.
    pub fn wait_time(&self) -> Option<Duration> {
        self.due_at
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    pub fn set_errors(&mut self, errors: &[RuleError]) {
        self.errors = vec![None; self.spec.rules.len()];
        for e in errors {
            if let Some(slot) = self.errors.get_mut(e.rule) {
                *slot = Some(e.message.clone());
            }
        }
    }

    pub fn clear_errors(&mut self) {
        self.errors = vec![None; self.spec.rules.len()];
    }

    pub fn has_errors(&self) -> bool {
        self.errors.iter().any(Option::is_some)
    }

    fn changed(&mut self, immediate: bool) {
        let delay = if immediate { Duration::ZERO } else { DEBOUNCE };
        self.due_at = Some(Instant::now() + delay);
    }

    /// Replaces the filter (e.g. from a profile), keeping the current time
    /// range, and applies it on the next frame.
    pub fn set_spec(&mut self, mut spec: FilterSpec) {
        if spec.rules.is_empty() {
            spec.rules.push(FilterRule::default());
        }
        spec.time_from = self.spec.time_from;
        spec.time_to = self.spec.time_to;
        self.errors = vec![None; spec.rules.len()];
        self.spec = spec;
        self.changed(true);
    }

    /// The filter to save in a profile (without empty placeholder rules and
    /// the time range).
    pub fn saved_spec(&self) -> FilterSpec {
        FilterSpec {
            rules: self
                .spec
                .rules
                .iter()
                .filter(|r| !r.pattern.is_empty())
                .cloned()
                .collect(),
            time_from: None,
            time_to: None,
            ..self.spec.clone()
        }
    }

    /// Parses a time field; empty = no limit. Returns `Err` for invalid input.
    fn parse_time(text: &str) -> Result<Option<i64>, ()> {
        if text.trim().is_empty() {
            Ok(None)
        } else {
            time::parse_user_time(text).map(Some).ok_or(())
        }
    }

    fn apply_time_texts(&mut self) {
        match (
            Self::parse_time(&self.time_from_text),
            Self::parse_time(&self.time_to_text),
        ) {
            (Ok(from), Ok(to)) => {
                self.spec.time_from = from;
                self.spec.time_to = to;
                self.time_error = None;
            }
            _ => self.time_error = Some("Użyj formatu RRRR-MM-DD [GG:MM[:SS]]".into()),
        }
    }

    pub fn set_time_range(&mut self, from: Option<i64>, to: Option<i64>) {
        self.time_from_text = from.map(time::format_secs).unwrap_or_default();
        self.time_to_text = to.map(time::format_secs).unwrap_or_default();
        self.apply_time_texts();
        self.changed(true);
    }

    /// Level checkboxes and time range.
    fn show_meta(&mut self, ui: &mut egui::Ui, meta: &MetaInfo) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Poziomy:");
            let options = Level::ALL
                .iter()
                .rev()
                .map(|l| Some(*l))
                .chain(std::iter::once(None));
            for level in options {
                let mut on = self.spec.levels.contains(level);
                let name = level.map_or("brak", Level::name);
                let count = meta
                    .level_counts
                    .map(|c| c[level.map_or(0, Level::code) as usize]);
                let label = RichText::new(name).color(level_color(level)).strong();
                let resp = ui.checkbox(&mut on, label);
                let resp = match count {
                    Some(c) => resp.on_hover_text(format!("{} rekordów", fmt::num(c as u64))),
                    None => resp,
                };
                if resp.changed() {
                    self.spec.levels.set(level, on);
                    self.changed(true);
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Czas od");
            let from = ui.add(
                egui::TextEdit::singleline(&mut self.time_from_text)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("RRRR-MM-DD GG:MM:SS")
                    .desired_width(150.0),
            );
            ui.label("do");
            let to = ui.add(
                egui::TextEdit::singleline(&mut self.time_to_text)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("RRRR-MM-DD GG:MM:SS")
                    .desired_width(150.0),
            );
            if from.changed() || to.changed() {
                self.apply_time_texts();
                self.changed(false);
            }
        });
        ui.horizontal_wrapped(|ui| {
            let last = meta.time_bounds.map(|(_, max)| max);
            ui.add_enabled_ui(last.is_some(), |ui| {
                ui.label("Ostatnie:");
                for (label, secs) in [("15 min", 900), ("1 h", 3600), ("24 h", 86_400)] {
                    if ui
                        .button(label)
                        .on_hover_text("względem ostatniego wpisu w pliku")
                        .clicked()
                        && let Some(last) = last
                    {
                        self.set_time_range(Some(last - secs), None);
                    }
                }
            });
            if ui.button("cały czas").clicked() {
                self.set_time_range(None, None);
            }
        });
        if let Some((min, max)) = meta.time_bounds {
            ui.weak(format!(
                "W pliku: {} – {}",
                time::format_secs(min),
                time::format_secs(max)
            ));
        }
        if let Some(e) = &self.time_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if self.spec.time_from.is_some() || self.spec.time_to.is_some() {
            ui.weak("Rekordy bez znacznika czasu są pomijane.");
        }
    }

    /// Draws the rule editor.
    pub fn show(&mut self, ui: &mut egui::Ui, meta: &MetaInfo) {
        self.show_meta(ui, meta);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Dołączone:");
            let mode = &mut self.spec.mode;
            let before = *mode;
            ui.radio_value(mode, IncludeMode::All, "wszystkie (AND)")
                .on_hover_text("Linia musi pasować do każdego filtra „+”");
            ui.radio_value(mode, IncludeMode::Any, "dowolny (OR)")
                .on_hover_text("Linia musi pasować do co najmniej jednego filtra „+”");
            if *mode != before {
                self.changed(true);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Kontekst:");
            let b = ui.add(
                egui::DragValue::new(&mut self.spec.context_before)
                    .range(0..=MAX_CONTEXT)
                    .prefix("przed "),
            );
            let a = ui.add(
                egui::DragValue::new(&mut self.spec.context_after)
                    .range(0..=MAX_CONTEXT)
                    .prefix("po "),
            );
            if b.changed() || a.changed() {
                self.changed(false);
            }
        });
        ui.add_space(4.0);

        self.errors.resize(self.spec.rules.len(), None);
        let mut action = None;
        let rule_count = self.spec.rules.len();
        let error_color = ui.visuals().error_fg_color;
        for i in 0..rule_count {
            let mut immediate = false;
            let mut typed = false;
            let rule = &mut self.spec.rules[i];
            ui.horizontal(|ui| {
                immediate |= ui
                    .checkbox(&mut rule.enabled, "")
                    .on_hover_text("Włącz/wyłącz filtr")
                    .changed();

                let (label, color, hint) = if rule.exclude {
                    (
                        "−",
                        Color32::from_rgb(0xE0, 0x55, 0x55),
                        "Wyklucza pasujące linie (kliknij, aby zmienić)",
                    )
                } else {
                    (
                        "+",
                        Color32::from_rgb(0x4C, 0xAF, 0x50),
                        "Wymaga dopasowania (kliknij, aby zmienić)",
                    )
                };
                if ui
                    .add(
                        egui::Button::new(RichText::new(label).strong().color(color))
                            .min_size(egui::vec2(22.0, 0.0)),
                    )
                    .on_hover_text(hint)
                    .clicked()
                {
                    rule.exclude = !rule.exclude;
                    immediate = true;
                }

                // Buttons from the right first; the pattern field takes exactly
                // the remaining width (see highlight_panel.rs).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("⋯", |ui| {
                        if ui.add_enabled(i > 0, egui::Button::new("W górę")).clicked() {
                            action = Some(RowAction::Up(i));
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(i + 1 < rule_count, egui::Button::new("W dół"))
                            .clicked()
                        {
                            action = Some(RowAction::Down(i));
                            ui.close_menu();
                        }
                        if ui.button("Usuń").clicked() {
                            action = Some(RowAction::Remove(i));
                            ui.close_menu();
                        }
                    });
                    immediate |= ui
                        .toggle_value(&mut rule.case_sensitive, RichText::new("Aa").monospace())
                        .on_hover_text("Rozróżniaj wielkość liter")
                        .changed();
                    let mut is_regex = rule.kind == PatternKind::Regex;
                    if ui
                        .toggle_value(&mut is_regex, RichText::new(".*").monospace())
                        .on_hover_text("Wyrażenie regularne")
                        .changed()
                    {
                        rule.kind = if is_regex {
                            PatternKind::Regex
                        } else {
                            PatternKind::Text
                        };
                        immediate = true;
                    }
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut rule.pattern)
                            .font(egui::TextStyle::Monospace)
                            .hint_text(if rule.kind == PatternKind::Regex {
                                "wyrażenie regularne"
                            } else {
                                "tekst"
                            })
                            .desired_width(ui.available_width()),
                    );
                    typed |= edit.changed();
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        immediate = true;
                    }
                });
            });
            if let Some(message) = &self.errors[i] {
                ui.label(
                    RichText::new(message)
                        .monospace()
                        .small()
                        .color(error_color),
                );
            }
            if immediate {
                self.changed(true);
            } else if typed {
                self.changed(false);
            }
        }

        match action {
            Some(RowAction::Up(i)) => {
                self.spec.rules.swap(i, i - 1);
                self.errors.swap(i, i - 1);
                self.changed(true);
            }
            Some(RowAction::Down(i)) => {
                self.spec.rules.swap(i, i + 1);
                self.errors.swap(i, i + 1);
                self.changed(true);
            }
            Some(RowAction::Remove(i)) => {
                self.spec.rules.remove(i);
                self.errors.remove(i);
                if self.spec.rules.is_empty() {
                    self.spec.rules.push(FilterRule::default());
                    self.errors.push(None);
                }
                self.changed(true);
            }
            None => {}
        }

        ui.horizontal(|ui| {
            if ui.button("+ Dodaj filtr").clicked() {
                self.spec.rules.push(FilterRule::default());
                self.errors.push(None);
            }
            if ui.button("Wyczyść").clicked() {
                *self = Self {
                    due_at: Some(Instant::now()),
                    ..Self::default()
                };
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_round_trip_keeps_time_range() {
        let mut panel = FilterPanel::default();
        panel.spec.time_from = Some(10);
        let saved = FilterSpec {
            rules: vec![FilterRule::include("x"), FilterRule::exclude("y").regex()],
            mode: IncludeMode::Any,
            context_after: 3,
            ..Default::default()
        };
        panel.set_spec(saved.clone());
        assert_eq!(panel.spec.time_from, Some(10), "time range is per file");
        assert_eq!(panel.saved_spec(), saved);
        assert!(panel.take_due());
    }

    #[test]
    fn empty_profile_filter_gets_placeholder_rule() {
        let mut panel = FilterPanel::default();
        panel.set_spec(FilterSpec::default());
        assert_eq!(panel.spec.rules.len(), 1, "one empty row to type into");
        assert!(panel.saved_spec().rules.is_empty());
    }
}
