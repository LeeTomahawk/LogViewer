//! "Wskaż czas w linii": the user selects the timestamp in a sample line and
//! a custom format is built from it ([`logview_core::ts_locate`]).

use std::ops::Range;

use eframe::egui::{self, RichText};
use logview_core::ts_locate::{self, TimestampGuess};
use logview_core::{FormatKind, LogFormat, time};

use super::Tab;

/// Longest part of a line shown for selecting.
const MAX_SHOWN_BYTES: usize = 4000;
/// Lines (from the sample line on) checked by the preview.
const PREVIEW_LINES: usize = 400;
/// Matching lines listed in the preview.
const PREVIEW_SHOWN: usize = 6;

pub struct TsPicker {
    line: usize,
    text: String,
    /// Last selection (byte range in `text`) the guess was made for.
    selection: Range<usize>,
    guess: Option<Result<TimestampGuess, String>>,
    /// Time format; filled from the guess, editable.
    time_format: String,
    preview: Option<Preview>,
}

struct Preview {
    checked: usize,
    headers: usize,
    with_time: usize,
    rows: Vec<(usize, String, Option<i64>)>,
    error: Option<String>,
}

fn shown_text(bytes: &[u8]) -> String {
    let cut = &bytes[..bytes.len().min(MAX_SHOWN_BYTES)];
    String::from_utf8_lossy(cut).into_owned()
}

fn byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(b, _)| b)
}

fn format_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    format!("{}.{:03}", time::format_secs(secs), ms.rem_euclid(1000))
}

impl Tab {
    pub(super) fn open_ts_picker(&mut self) {
        let Some(file) = &self.file else { return };
        let count = file.line_count();
        let mut line = self
            .view
            .cursor
            .or(self.view.top_line())
            .unwrap_or(0)
            .min(count.saturating_sub(1));
        // First non-empty line from there.
        for (n, bytes) in file.lines(line..(line + 50).min(count)) {
            if !bytes.trim_ascii().is_empty() {
                line = n;
                break;
            }
        }
        let text = shown_text(file.line(line).unwrap_or_default());
        self.ts_picker = Some(TsPicker {
            line,
            text,
            selection: 0..0,
            guess: None,
            time_format: String::new(),
            preview: None,
        });
    }

    fn picker_set_line(&mut self, line: usize) {
        let Some(file) = &self.file else { return };
        let Some(picker) = &mut self.ts_picker else {
            return;
        };
        if let Some(bytes) = file.line(line) {
            picker.line = line;
            picker.text = shown_text(bytes);
            picker.selection = 0..0;
            picker.guess = None;
            picker.preview = None;
        }
    }

    /// The format the picker would apply, with the edited time format.
    fn picker_format(picker: &TsPicker) -> Option<LogFormat> {
        let guess = picker.guess.as_ref()?.as_ref().ok()?;
        Some(LogFormat {
            timestamp_format: picker.time_format.trim().to_owned(),
            ..guess.format.clone()
        })
    }

    fn picker_preview(&mut self) {
        let Some(file) = &self.file else { return };
        let Some(picker) = &mut self.ts_picker else {
            return;
        };
        let Some(format) = Self::picker_format(picker) else {
            picker.preview = None;
            return;
        };
        let end = (picker.line + PREVIEW_LINES).min(file.line_count());
        let lines = file.lines(picker.line..end);
        let mut preview = Preview {
            checked: lines.len(),
            headers: 0,
            with_time: 0,
            rows: Vec::new(),
            error: None,
        };
        match format.compile(&[]) {
            Ok(compiled) => {
                for (n, bytes) in lines {
                    let Some(meta) = compiled.meta(bytes) else {
                        continue;
                    };
                    preview.headers += 1;
                    preview.with_time += usize::from(meta.ts.is_some());
                    if preview.rows.len() < PREVIEW_SHOWN {
                        let mut text = shown_text(&bytes[..bytes.len().min(160)]);
                        text = text.replace('\t', " ");
                        preview.rows.push((n, text, meta.ts));
                    }
                }
            }
            Err(e) => preview.error = Some(e),
        }
        picker.preview = Some(preview);
    }

    pub(super) fn ts_picker_window(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.format_panel.pick_timestamp) {
            self.open_ts_picker();
        }
        let Some(picker) = &mut self.ts_picker else {
            return;
        };
        let line_count = self.file.as_ref().map_or(0, |f| f.line_count());
        let mut open = true;
        let mut go_to = None;
        let mut apply = false;
        let mut cancel = false;
        let mut reguess = false;
        let mut repreview = false;
        egui::Window::new("Wskaż czas w linii")
            .open(&mut open)
            .collapsible(false)
            .default_width(720.0)
            .show(ctx, |ui| {
                ui.label(
                    "Zaznacz myszą znacznik czasu w linii poniżej. Program ustali, \
                     gdzie leży w linii i jak go odczytać.",
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(picker.line > 0, egui::Button::new("◀"))
                        .on_hover_text("Poprzednia linia")
                        .clicked()
                    {
                        go_to = Some(picker.line - 1);
                    }
                    ui.label(format!("Linia {}", crate::fmt::num(picker.line as u64 + 1)));
                    if ui
                        .add_enabled(picker.line + 1 < line_count, egui::Button::new("▶"))
                        .on_hover_text("Następna linia")
                        .clicked()
                    {
                        go_to = Some(picker.line + 1);
                    }
                });
                let output = egui::TextEdit::multiline(&mut picker.text.as_str())
                    .id_salt(("ts-picker-line", picker.line))
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(2)
                    .show(ui);
                if let Some(range) = output.cursor_range {
                    let chars = range.as_sorted_char_range();
                    let bytes =
                        byte_index(&picker.text, chars.start)..byte_index(&picker.text, chars.end);
                    // Act when the mouse is released (not on every drag step).
                    let dragging = ui.input(|i| i.pointer.any_down());
                    if !bytes.is_empty() && bytes != picker.selection && !dragging {
                        picker.selection = bytes;
                        reguess = true;
                    }
                }

                ui.separator();
                match &picker.guess {
                    None => {
                        ui.weak("Nic nie zaznaczono.");
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    Some(Ok(guess)) => {
                        let selected = &picker.text[picker.selection.clone()];
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Czas:");
                            ui.label(RichText::new(selected.trim()).monospace().strong());
                            match guess.example {
                                Some(ms)
                                    if picker.time_format.trim()
                                        == guess.format.timestamp_format =>
                                {
                                    ui.label(format!("→ {}", format_ms(ms)));
                                }
                                _ if !guess.recognized => {
                                    ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        "nie rozpoznano formatu – wpisz go poniżej",
                                    );
                                }
                                _ => {}
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Format czasu:");
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut picker.time_format)
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text("automatycznie, np. %d.%m.%Y %H:%M:%S%.f")
                                    .desired_width(260.0),
                            );
                            repreview |= edit.changed();
                        })
                        .response
                        .on_hover_text(
                            "Format chrono/strftime: %Y rok, %m miesiąc, %d dzień, %H godzina, \
                             %M minuta, %S sekunda, %.f ułamek, %b nazwa miesiąca, %z strefa. \
                             Puste = rozpoznawanie automatyczne (ISO, syslog, Apache, epoch).",
                        );
                        if let FormatKind::Regex(pattern) = &guess.format.kind {
                            egui::CollapsingHeader::new("Wyrażenie")
                                .id_salt("ts-picker-regex")
                                .show(ui, |ui| {
                                    ui.label(RichText::new(pattern).monospace().small());
                                });
                        }
                    }
                }

                if let Some(preview) = &picker.preview {
                    ui.separator();
                    if let Some(e) = &preview.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    } else {
                        ui.label(format!(
                            "Podgląd ({} linii od zaznaczonej): {} początków rekordów, \
                             czas odczytany w {}",
                            preview.checked, preview.headers, preview.with_time
                        ));
                        egui::Grid::new("ts-picker-preview")
                            .striped(true)
                            .show(ui, |ui| {
                                for (n, text, ts) in &preview.rows {
                                    ui.weak(crate::fmt::num(*n as u64 + 1));
                                    match ts {
                                        Some(ms) => ui.monospace(format_ms(*ms)),
                                        None => ui.colored_label(
                                            ui.visuals().error_fg_color,
                                            "brak czasu",
                                        ),
                                    };
                                    ui.label(RichText::new(text).monospace().small());
                                    ui.end_row();
                                }
                            });
                    }
                }

                ui.separator();
                ui.horizontal(|ui| {
                    let ok = matches!(&picker.guess, Some(Ok(_)));
                    apply = ui
                        .add_enabled(ok, egui::Button::new("Użyj tego formatu"))
                        .on_hover_text("Ustawia własny format (regex) w sekcji Format")
                        .clicked();
                    cancel = ui.button("Anuluj").clicked();
                });
            });

        if reguess {
            let guess = ts_locate::locate(&picker.text, picker.selection.clone());
            if let Ok(g) = &guess {
                picker.time_format = g.format.timestamp_format.clone();
            }
            picker.guess = Some(guess);
            repreview = true;
        }
        if apply && let Some(format) = Self::picker_format(picker) {
            self.format_panel.set_custom(format);
            self.ts_picker = None;
            return;
        }
        if !open || cancel {
            self.ts_picker = None;
            return;
        }
        if let Some(line) = go_to {
            self.picker_set_line(line);
        } else if repreview {
            self.picker_preview();
        }
    }
}
