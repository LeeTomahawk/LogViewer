//! Opening compressed and non-UTF-8 files (via a temporary UTF-8 copy) and
//! the encoding picker.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use logview_core::{LogFile, PrepareJob, TextEncoding, prepare};

use super::Tab;
use crate::fmt;

impl Tab {
    /// Opens `path` with the tab's encoding: directly (UTF-8) or after
    /// converting it in the background. Returns `false` if it cannot be read.
    pub fn open(&mut self, path: PathBuf) -> bool {
        match prepare::plan(&path, self.encoding) {
            Err(e) => {
                self.error = Some(format!("nie można otworzyć pliku {}: {e}", path.display()));
                false
            }
            Ok(plan) if plan.needed() => {
                self.preparing = Some(PrepareJob::start(&path, plan, Arc::clone(&self.notify)));
                true
            }
            Ok(_) => self.open_direct(path),
        }
    }

    fn open_direct(&mut self, path: PathBuf) -> bool {
        match LogFile::open(&path, Arc::clone(&self.notify)) {
            Ok(file) => {
                self.attach(file);
                true
            }
            Err(e) => {
                self.error = Some(e.to_string());
                false
            }
        }
    }

    /// Opens the converted file once the conversion is done.
    pub(super) fn poll_prepare(&mut self) {
        let Some(job) = &self.preparing else { return };
        let Some(result) = job.take_result() else {
            return;
        };
        let source = job.source().to_path_buf();
        self.preparing = None;
        match result {
            Ok(prepared) => {
                match LogFile::open_prepared(&prepared, &source, Arc::clone(&self.notify)) {
                    Ok(file) => {
                        let size = std::fs::metadata(&prepared.path).map_or(0, |m| m.len());
                        self.attach(file);
                        self.set_notice(format!(
                            "{}: {} → {} (plik tymczasowy, usuwany po zamknięciu)",
                            prepared.description,
                            fmt::bytes(std::fs::metadata(&source).map_or(0, |m| m.len())),
                            fmt::bytes(size)
                        ));
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Err(e) => self.error = Some(format!("Nie można przygotować pliku: {e}")),
        }
    }

    pub(super) fn is_preparing(&self) -> bool {
        self.preparing.is_some()
    }

    /// Name of the file being converted (for the tab title).
    pub(super) fn preparing_name(&self) -> Option<String> {
        let job = self.preparing.as_ref()?;
        Some(job.source().file_name().map_or_else(
            || job.source().display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ))
    }

    /// Progress of a running conversion, in the central area.
    pub(super) fn preparing_ui(&mut self, ui: &mut egui::Ui) {
        let Some(job) = &self.preparing else { return };
        let plan = job.plan();
        let what = plan.describe(plan.encoding);
        let what = if what.is_empty() {
            "przygotowanie".to_owned()
        } else {
            what
        };
        let mut cancel = false;
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);
            ui.heading(format!(
                "{}: {what}…",
                self.preparing_name().unwrap_or_default()
            ));
            ui.add(
                egui::ProgressBar::new(job.progress())
                    .desired_width(360.0)
                    .show_percentage(),
            );
            ui.weak(format!(
                "zapisano {} tekstu UTF-8",
                fmt::bytes(job.written())
            ));
            ui.add_space(6.0);
            cancel = ui.button("Anuluj").clicked();
        });
        if cancel {
            // Dropping the job cancels it and removes the partial file.
            self.preparing = None;
        }
    }

    /// Encoding picker for the status bar; returns a new choice.
    pub(super) fn encoding_picker(
        &self,
        ui: &mut egui::Ui,
        file: &LogFile,
    ) -> Option<TextEncoding> {
        let current = match file.conversion() {
            Some(c) => c.to_owned(),
            None => format!("{} (lossy)", self.encoding.short()),
        };
        let mut chosen = None;
        egui::ComboBox::from_id_salt(("encoding", self.id))
            .selected_text(current)
            .width(200.0)
            .show_ui(ui, |ui| {
                for choice in TextEncoding::CHOICES {
                    if ui
                        .selectable_label(self.encoding == choice, choice.label())
                        .clicked()
                    {
                        chosen = Some(choice);
                    }
                }
            })
            .response
            .on_hover_text(
                "Kodowanie pliku. Inne niż UTF-8 oraz pliki .gz/.zst są konwertowane do \
                 tymczasowego pliku UTF-8 (usuwanego po zamknięciu).",
            );
        chosen.filter(|c| *c != self.encoding)
    }

    /// Reopens the file with another encoding.
    pub fn set_encoding(&mut self, encoding: TextEncoding) {
        self.encoding = encoding;
        if file_path(self).is_some_and(|p| p.exists())
            && let Some(path) = file_path(self)
        {
            let line = self.view.top_line();
            if self.open(path)
                && let Some(line) = line
            {
                self.view.scroll_to_line(line);
            }
        }
    }
}

fn file_path(tab: &Tab) -> Option<PathBuf> {
    let file = tab.file.as_ref()?;
    file.merged_sources()
        .is_none()
        .then(|| file.path().to_path_buf())
}
