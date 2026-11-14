//! Editing of the highlight (coloring) rules, with debounced application.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, RichText, color_picker::Alpha};
use logview_core::{HighlightRule, PatternKind, Rgba, RuleError, Scope};

const DEBOUNCE: Duration = Duration::from_millis(300);
const DEFAULT_FG: Rgba = [0xFF, 0x52, 0x52, 0xFF];
const DEFAULT_BG: Rgba = [0xFF, 0xEB, 0x3B, 0x50];

pub struct HighlightPanel {
    pub rules: Vec<HighlightRule>,
    errors: Vec<Option<String>>,
    due_at: Option<Instant>,
}

impl Default for HighlightPanel {
    fn default() -> Self {
        let rules = HighlightRule::defaults();
        Self {
            errors: vec![None; rules.len()],
            rules,
            // Apply the defaults on the first frame.
            due_at: Some(Instant::now()),
        }
    }
}

enum RowAction {
    Up(usize),
    Down(usize),
    Remove(usize),
    Duplicate(usize),
}

fn to_color([r, g, b, a]: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Optional color: checkbox + color button. Returns `true` if changed.
fn color_option(ui: &mut egui::Ui, label: &str, value: &mut Option<Rgba>, default: Rgba) -> bool {
    let mut changed = false;
    let mut on = value.is_some();
    if ui.checkbox(&mut on, label).changed() {
        *value = on.then_some(value.unwrap_or(default));
        changed = true;
    }
    if let Some(rgba) = value {
        let mut color = to_color(*rgba);
        if egui::color_picker::color_edit_button_srgba(ui, &mut color, Alpha::OnlyBlend).changed() {
            *rgba = color.to_srgba_unmultiplied();
            changed = true;
        }
    }
    changed
}

impl HighlightPanel {
    /// Replaces the rules (e.g. from a profile) and applies them next frame.
    pub fn set_rules(&mut self, rules: Vec<HighlightRule>) {
        self.errors = vec![None; rules.len()];
        self.rules = rules;
        self.changed(true);
    }

    pub fn take_due(&mut self) -> bool {
        match self.due_at {
            Some(at) if at <= Instant::now() => {
                self.due_at = None;
                true
            }
            _ => false,
        }
    }

    pub fn wait_time(&self) -> Option<Duration> {
        self.due_at
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    pub fn set_errors(&mut self, errors: &[RuleError]) {
        self.errors = vec![None; self.rules.len()];
        for e in errors {
            if let Some(slot) = self.errors.get_mut(e.rule) {
                *slot = Some(e.message.clone());
            }
        }
    }

    pub fn clear_errors(&mut self) {
        self.errors = vec![None; self.rules.len()];
    }

    pub fn has_errors(&self) -> bool {
        self.errors.iter().any(Option::is_some)
    }

    fn changed(&mut self, immediate: bool) {
        let delay = if immediate { Duration::ZERO } else { DEBOUNCE };
        self.due_at = Some(Instant::now() + delay);
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.weak("Pierwsza reguła ma najwyższy priorytet.");
        ui.add_space(2.0);
        self.errors.resize(self.rules.len(), None);
        let count = self.rules.len();
        let error_color = ui.visuals().error_fg_color;
        let mut action = None;

        for i in 0..count {
            let mut immediate = false;
            let mut typed = false;
            let rule = &mut self.rules[i];
            ui.horizontal(|ui| {
                immediate |= ui
                    .checkbox(&mut rule.enabled, "")
                    .on_hover_text("Włącz/wyłącz regułę")
                    .changed();
                // Buttons are laid out from the right first, so the pattern
                // field gets exactly the remaining width. (Sizing the field
                // from a guessed button width made the row wider than the
                // panel, and the panel then grew a little every frame.)
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("⋯", |ui| {
                        if ui.add_enabled(i > 0, egui::Button::new("W górę")).clicked() {
                            action = Some(RowAction::Up(i));
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(i + 1 < count, egui::Button::new("W dół"))
                            .clicked()
                        {
                            action = Some(RowAction::Down(i));
                            ui.close_menu();
                        }
                        if ui.button("Duplikuj").clicked() {
                            action = Some(RowAction::Duplicate(i));
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
                    let mut preview = egui::TextStyle::Monospace.resolve(ui.style());
                    preview.size = preview.size.max(12.0);
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut rule.pattern)
                            .font(preview)
                            .text_color_opt(rule.style.fg.map(to_color))
                            .background_color(
                                rule.style
                                    .bg
                                    .map_or(ui.visuals().extreme_bg_color, to_color),
                            )
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
            ui.horizontal_wrapped(|ui| {
                ui.add_space(22.0);
                immediate |= color_option(ui, "tekst", &mut rule.style.fg, DEFAULT_FG);
                immediate |= color_option(ui, "tło", &mut rule.style.bg, DEFAULT_BG);
                immediate |= ui
                    .toggle_value(&mut rule.style.bold, RichText::new("B").strong())
                    .on_hover_text("Pogrubienie")
                    .changed();
                immediate |= ui
                    .toggle_value(&mut rule.style.italic, RichText::new("I").italics())
                    .on_hover_text("Kursywa")
                    .changed();
                let mut whole_line = rule.scope == Scope::Line;
                if ui
                    .toggle_value(&mut whole_line, "cała linia")
                    .on_hover_text("Koloruj całą linię zamiast samego dopasowania")
                    .changed()
                {
                    rule.scope = if whole_line {
                        Scope::Line
                    } else {
                        Scope::Match
                    };
                    immediate = true;
                }
                immediate |= ui
                    .toggle_value(&mut rule.minimap, "mapa")
                    .on_hover_text("Pokazuj pasujące linie na pasku przewijania")
                    .changed();
            });
            if let Some(message) = &self.errors[i] {
                ui.label(
                    RichText::new(message)
                        .monospace()
                        .small()
                        .color(error_color),
                );
            }
            ui.add_space(3.0);
            if immediate {
                self.changed(true);
            } else if typed {
                self.changed(false);
            }
        }

        match action {
            Some(RowAction::Up(i)) => {
                self.rules.swap(i, i - 1);
                self.errors.swap(i, i - 1);
                self.changed(true);
            }
            Some(RowAction::Down(i)) => {
                self.rules.swap(i, i + 1);
                self.errors.swap(i, i + 1);
                self.changed(true);
            }
            Some(RowAction::Duplicate(i)) => {
                let copy = self.rules[i].clone();
                self.rules.insert(i + 1, copy);
                self.errors.insert(i + 1, None);
                self.changed(true);
            }
            Some(RowAction::Remove(i)) => {
                self.rules.remove(i);
                self.errors.remove(i);
                self.changed(true);
            }
            None => {}
        }

        ui.horizontal(|ui| {
            if ui.button("+ Dodaj regułę").clicked() {
                self.rules.push(HighlightRule::default());
                self.errors.push(None);
            }
            if ui.button("Przywróć domyślne").clicked() {
                *self = Self::default();
            }
        });
    }
}
