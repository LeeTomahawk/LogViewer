//! Editing of the highlight (coloring) rules, with debounced application.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, RichText, color_picker::Alpha};
use logview_core::{HighlightRule, Pattern, PatternKind, Rgba, RuleError, Scope};

const DEBOUNCE: Duration = Duration::from_millis(300);
const DEFAULT_FG: Rgba = [0xFF, 0x52, 0x52, 0xFF];
const DEFAULT_BG: Rgba = [0xFF, 0xEB, 0x3B, 0x50];
/// Lines around the view checked for the hit counts of the rules.
pub const HIT_SAMPLE_LINES: usize = 2000;

/// How a rule does on the lines around the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleHits {
    pub lines: usize,
    /// First matched text.
    pub example: Option<String>,
}

pub struct HighlightPanel {
    pub rules: Vec<HighlightRule>,
    errors: Vec<Option<String>>,
    due_at: Option<Instant>,
    /// Hit counts per rule (`None` = inactive or invalid).
    hits: Vec<Option<RuleHits>>,
    /// Number of lines the hits were counted in.
    hit_sample: usize,
    /// View line the hits were counted around, `None` = recount.
    pub hit_center: Option<usize>,
}

impl Default for HighlightPanel {
    fn default() -> Self {
        let rules = HighlightRule::defaults();
        Self {
            errors: vec![None; rules.len()],
            rules,
            // Apply the defaults on the first frame.
            due_at: Some(Instant::now()),
            hits: Vec::new(),
            hit_sample: 0,
            hit_center: None,
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

    /// Counts the lines of `sample` each active rule matches.
    pub fn count_hits(&mut self, sample: &[&[u8]]) {
        self.hit_sample = sample.len();
        self.hits = self
            .rules
            .iter()
            .map(|rule| {
                if !rule.is_active() {
                    return None;
                }
                let pattern = Pattern::new(&rule.pattern, rule.kind, rule.case_sensitive).ok()?;
                let mut hits = RuleHits {
                    lines: 0,
                    example: None,
                };
                for line in sample {
                    if hits.example.is_none() {
                        if let Some(range) = pattern.find_iter(line).next() {
                            let text = String::from_utf8_lossy(&line[range]);
                            let mut text: String = text.chars().take(60).collect();
                            if text.len() < line.len() && text.chars().count() == 60 {
                                text.push('…');
                            }
                            hits.example = Some(text);
                            hits.lines += 1;
                        }
                    } else if pattern.is_match(line) {
                        hits.lines += 1;
                    }
                }
                Some(hits)
            })
            .collect();
    }

    /// Hit count shown under a rule.
    fn hits_label(ui: &mut egui::Ui, hits: Option<&RuleHits>, sample: usize, rule: &HighlightRule) {
        let Some(hits) = hits else {
            return;
        };
        let sample = crate::fmt::num(sample as u64);
        if hits.lines == 0 {
            let looks_like_regex = rule.kind == PatternKind::Text
                && rule.pattern.chars().any(|c| "\\[](){}|*+?^$".contains(c));
            let text = if looks_like_regex {
                "0 trafień – to wygląda na regex, włącz .*"
            } else {
                "0 trafień"
            };
            ui.colored_label(ui.visuals().warn_fg_color, text)
                .on_hover_text(format!(
                    "Żadna z {sample} linii wokół widoku nie pasuje do tej reguły"
                ));
        } else {
            let mut tip = format!(
                "Pasuje {} z {sample} linii wokół widoku",
                crate::fmt::num(hits.lines as u64)
            );
            if let Some(example) = &hits.example {
                tip.push_str(&format!("\nPrzykład dopasowania: {example}"));
            }
            ui.weak(format!("✓ {}", crate::fmt::num(hits.lines as u64)))
                .on_hover_text(tip);
        }
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
                let hits = self.hits.get(i).and_then(Option::as_ref);
                Self::hits_label(ui, hits, self.hit_sample, rule);
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
                // New rules are regexes (plain words work as regexes too).
                self.rules.push(HighlightRule {
                    kind: PatternKind::Regex,
                    ..HighlightRule::default()
                });
                self.errors.push(None);
            }
            if ui.button("Przywróć domyślne").clicked() {
                *self = Self::default();
            }
        });
    }
}
