//! Choice of the log format: automatic, built-in or custom regex.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use logview_core::{CompiledFormat, Detection, FormatKind, FormatMode, LogFormat, ProfileFormat};

const DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatChoice {
    Auto,
    Builtin(usize),
    Custom,
}

pub struct FormatPanel {
    pub choice: FormatChoice,
    pub custom: LogFormat,
    /// Join continuation lines (stack traces) with their record.
    pub multiline: bool,
    pub error: Option<String>,
    /// Set by the "Wskaż czas w linii" button; the tab opens the picker.
    pub pick_timestamp: bool,
    builtins: Vec<LogFormat>,
    due_at: Option<Instant>,
}

impl Default for FormatPanel {
    fn default() -> Self {
        Self {
            choice: FormatChoice::Auto,
            custom: LogFormat {
                name: "Własny".into(),
                kind: FormatKind::Regex(
                    r"^(?P<ts>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}) (?P<level>\w+) (?P<msg>.*)$"
                        .into(),
                ),
                timestamp_format: String::new(),
            },
            multiline: true,
            error: None,
            pick_timestamp: false,
            builtins: LogFormat::builtins(),
            due_at: None,
        }
    }
}

/// What the panel shows besides the editable settings.
pub struct FormatInfo<'a> {
    pub detected: Option<&'a Detection>,
    pub active: Option<&'a CompiledFormat>,
    /// Fraction of sample lines matching the active format.
    pub active_ratio: Option<f32>,
    /// `(records, done, progress)` of the parse job.
    pub parse: Option<(usize, bool, f32)>,
}

impl FormatPanel {
    /// The format to use, given the detection result.
    pub fn resolve(&self, detected: Option<&Detection>) -> LogFormat {
        match self.choice {
            FormatChoice::Auto => detected.map_or_else(LogFormat::plain, |d| d.format.clone()),
            FormatChoice::Builtin(i) => self
                .builtins
                .get(i)
                .cloned()
                .unwrap_or_else(LogFormat::plain),
            FormatChoice::Custom => self.custom.clone(),
        }
    }

    /// Format settings to save in a profile.
    pub fn profile_format(&self) -> ProfileFormat {
        let mut out = ProfileFormat {
            multiline: self.multiline,
            ..Default::default()
        };
        match self.choice {
            FormatChoice::Auto => out.mode = FormatMode::Auto,
            FormatChoice::Builtin(i) => {
                out.mode = FormatMode::Builtin;
                out.name = self.builtins[i].name.clone();
            }
            FormatChoice::Custom => {
                out.mode = FormatMode::Custom;
                if let FormatKind::Regex(r) = &self.custom.kind {
                    out.regex = r.clone();
                }
                out.timestamp_format = self.custom.timestamp_format.clone();
            }
        }
        out
    }

    /// Loads format settings from a profile and applies them next frame. An
    /// unknown built-in name falls back to automatic detection.
    pub fn set_profile_format(&mut self, format: &ProfileFormat) {
        self.multiline = format.multiline;
        self.choice = match format.mode {
            FormatMode::Auto => FormatChoice::Auto,
            FormatMode::Builtin => self
                .builtins
                .iter()
                .position(|f| f.name == format.name)
                .map_or(FormatChoice::Auto, FormatChoice::Builtin),
            FormatMode::Custom => {
                self.custom = LogFormat {
                    name: "Własny".into(),
                    kind: FormatKind::Regex(format.regex.clone()),
                    timestamp_format: format.timestamp_format.clone(),
                };
                FormatChoice::Custom
            }
        };
        self.error = None;
        self.changed(true);
    }

    /// Switches to a custom format (e.g. from the timestamp picker).
    pub fn set_custom(&mut self, format: LogFormat) {
        self.custom = LogFormat {
            name: "Własny".into(),
            ..format
        };
        self.choice = FormatChoice::Custom;
        self.error = None;
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

    fn changed(&mut self, immediate: bool) {
        let delay = if immediate { Duration::ZERO } else { DEBOUNCE };
        self.due_at = Some(Instant::now() + delay);
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        info: &FormatInfo<'_>,
        columns_mode: &mut bool,
        hidden_columns: &mut HashSet<String>,
    ) {
        let auto_label = match info.detected {
            Some(d) => format!("Automatycznie: {}", d.format.name),
            None => "Automatycznie".to_owned(),
        };
        let selected = match self.choice {
            FormatChoice::Auto => auto_label.clone(),
            FormatChoice::Builtin(i) => self.builtins[i].name.clone(),
            FormatChoice::Custom => "Własny (regex)".to_owned(),
        };
        let before = self.choice;
        egui::ComboBox::from_id_salt("format-choice")
            .width(ui.available_width() - 8.0)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.choice, FormatChoice::Auto, auto_label);
                for (i, f) in self.builtins.iter().enumerate() {
                    ui.selectable_value(&mut self.choice, FormatChoice::Builtin(i), &f.name);
                }
                ui.selectable_value(&mut self.choice, FormatChoice::Custom, "Własny (regex)");
            });
        if self.choice != before {
            self.changed(true);
        }
        if ui
            .button("Wskaż czas w linii…")
            .on_hover_text(
                "Zaznacz znacznik czasu w przykładowej linii – program ustali jego \n                 położenie i format (np. gdy czas nie jest na początku linii)",
            )
            .clicked()
        {
            self.pick_timestamp = true;
        }

        // Regex of the selected built-in format, with an option to edit a copy.
        let shown = self.resolve(info.detected);
        if self.choice != FormatChoice::Custom
            && let FormatKind::Regex(pattern) = &shown.kind
        {
            let mut copy = false;
            egui::CollapsingHeader::new("Wyrażenie")
                .id_salt("format-regex")
                .show(ui, |ui| {
                    ui.label(RichText::new(pattern).monospace().small());
                    copy = ui.button("Edytuj jako własny").clicked();
                });
            if copy {
                self.custom = LogFormat {
                    name: "Własny".into(),
                    ..shown.clone()
                };
                self.choice = FormatChoice::Custom;
                self.changed(true);
            }
        }

        if self.choice == FormatChoice::Custom {
            if let FormatKind::Regex(pattern) = &mut self.custom.kind {
                ui.label("Wyrażenie z nazwanymi grupami (ts, level, msg, …):");
                let edit = ui.add(
                    egui::TextEdit::multiline(pattern)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if edit.changed() {
                    self.changed(false);
                }
            }
            ui.horizontal(|ui| {
                ui.label("Format czasu:");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.custom.timestamp_format)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("automatycznie, np. %d.%m.%Y %H:%M:%S")
                        .desired_width(f32::INFINITY),
                );
                if edit.changed() {
                    self.changed(false);
                }
            });
        }
        if let Some(e) = &self.error {
            ui.label(
                RichText::new(e)
                    .monospace()
                    .small()
                    .color(ui.visuals().error_fg_color),
            );
        }

        if ui
            .checkbox(&mut self.multiline, "Łącz linie w rekordy (stack trace)")
            .on_hover_text(
                "Linie niepasujące do formatu należą do poprzedniego rekordu; \
                 filtry działają wtedy na całych rekordach",
            )
            .changed()
        {
            self.changed(true);
        }

        if let Some(active) = info.active {
            if let Some(ratio) = info.active_ratio {
                ui.weak(format!("Pasuje {:.0}% linii początku pliku", ratio * 100.0));
            }
            if !active.columns().is_empty() {
                ui.checkbox(columns_mode, "Widok kolumn (Ctrl+T)");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Kolumny:");
                    for name in active.columns() {
                        let mut visible = !hidden_columns.contains(name);
                        if ui.checkbox(&mut visible, name).changed() {
                            if visible {
                                hidden_columns.remove(name);
                            } else {
                                hidden_columns.insert(name.clone());
                            }
                        }
                    }
                });
            }
        }
        if let Some((records, done, progress)) = info.parse {
            ui.horizontal(|ui| {
                if !done {
                    ui.add(egui::Spinner::new());
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .desired_width(120.0)
                            .show_percentage(),
                    );
                }
                ui.weak(format!("Rekordów: {}", crate::fmt::num(records as u64)));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_format_round_trip() {
        let builtin_name = LogFormat::builtins()[2].name.clone();
        let cases = [
            ProfileFormat::default(),
            ProfileFormat {
                mode: FormatMode::Builtin,
                name: builtin_name,
                multiline: false,
                ..Default::default()
            },
            ProfileFormat {
                mode: FormatMode::Custom,
                regex: r"^(?P<ts>\S+) (?P<msg>.*)$".into(),
                timestamp_format: "%d.%m.%Y".into(),
                ..Default::default()
            },
        ];
        for case in cases {
            let mut panel = FormatPanel::default();
            panel.set_profile_format(&case);
            assert_eq!(panel.profile_format(), case);
            assert!(panel.take_due(), "applied on the next frame");
        }
    }

    #[test]
    fn unknown_builtin_falls_back_to_auto() {
        let mut panel = FormatPanel::default();
        panel.set_profile_format(&ProfileFormat {
            mode: FormatMode::Builtin,
            name: "nie istnieje".into(),
            ..Default::default()
        });
        assert_eq!(panel.choice, FormatChoice::Auto);
    }
}
