//! Quick search bar (Ctrl+F).

use std::time::{Duration, Instant};

use eframe::egui::{self, Key, RichText};
use logview_core::PatternKind;

const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchAction {
    Next,
    Prev,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Found {
        line: usize,
        wrapped: bool,
    },
    /// No hit yet, but the search is still running.
    Pending,
    NotFound,
}

/// Finds the next (`forward`) or previous hit whose line is `visible`,
/// wrapping around the hit list once.
///
/// * `len` – hits found so far; `running` – whether more may come;
/// * `start_row` – forward: first candidate row; backward: one past the first
///   candidate row (candidates are `start_row - 1`, `start_row - 2`, …);
/// * `line_at(row)` – line of hit `row`;
/// * at most `max_steps` hits are checked.
pub fn find_hit(
    len: usize,
    running: bool,
    start_row: usize,
    forward: bool,
    line_at: impl Fn(usize) -> Option<usize>,
    visible: impl Fn(usize) -> bool,
    max_steps: usize,
) -> Step {
    let mut wrapped = false;
    let mut row = start_row.min(len);
    let mut steps = 0;
    loop {
        // Next candidate row, wrapping (or waiting) at the ends.
        let candidate = if forward {
            if row >= len {
                if running {
                    return Step::Pending;
                }
                if wrapped || len == 0 {
                    return Step::NotFound;
                }
                wrapped = true;
                row = 0;
                continue;
            }
            row
        } else {
            if row == 0 {
                if running {
                    return Step::Pending;
                }
                if wrapped || len == 0 {
                    return Step::NotFound;
                }
                wrapped = true;
                row = len;
                continue;
            }
            row - 1
        };
        steps += 1;
        if steps > max_steps {
            return Step::NotFound;
        }
        let Some(line) = line_at(candidate) else {
            return Step::NotFound;
        };
        if visible(line) {
            return Step::Found { line, wrapped };
        }
        row = if forward { candidate + 1 } else { candidate };
    }
}

#[derive(Default)]
pub struct SearchBar {
    pub open: bool,
    pub query: String,
    pub kind: PatternKind,
    pub case_sensitive: bool,
    pub error: Option<String>,
    focus: bool,
    due_at: Option<Instant>,
}

impl SearchBar {
    pub fn open_and_focus(&mut self) {
        if !self.open {
            self.open = true;
            // Re-apply the remembered query.
            self.due_at = Some(Instant::now());
        }
        self.focus = true;
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

    /// Draws the bar. `status` is shown next to the buttons (e.g. "3 / 120").
    pub fn show(&mut self, ui: &mut egui::Ui, status: &str, running: bool) -> Option<SearchAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label("Szukaj:");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id(egui::Id::new("logview-search"))
                    .font(egui::TextStyle::Monospace)
                    .hint_text("Enter: następne, Shift+Enter: poprzednie, Esc: zamknij")
                    .desired_width(340.0),
            );
            if self.focus {
                edit.request_focus();
                self.focus = false;
            }
            if edit.changed() {
                self.changed(false);
            }
            if edit.lost_focus() {
                let (enter, shift, esc) = ui.input(|i| {
                    (
                        i.key_pressed(Key::Enter),
                        i.modifiers.shift,
                        i.key_pressed(Key::Escape),
                    )
                });
                if esc {
                    action = Some(SearchAction::Close);
                } else if enter {
                    if self.due_at.is_some() {
                        self.changed(true);
                    }
                    action = Some(if shift {
                        SearchAction::Prev
                    } else {
                        SearchAction::Next
                    });
                    edit.request_focus();
                }
            }

            let mut is_regex = self.kind == PatternKind::Regex;
            if ui
                .toggle_value(&mut is_regex, RichText::new(".*").monospace())
                .on_hover_text("Wyrażenie regularne")
                .changed()
            {
                self.kind = if is_regex {
                    PatternKind::Regex
                } else {
                    PatternKind::Text
                };
                self.changed(true);
            }
            if ui
                .toggle_value(&mut self.case_sensitive, RichText::new("Aa").monospace())
                .on_hover_text("Rozróżniaj wielkość liter")
                .changed()
            {
                self.changed(true);
            }
            if ui
                .button("↑")
                .on_hover_text("Poprzednie (Shift+F3)")
                .clicked()
            {
                action = Some(SearchAction::Prev);
            }
            if ui.button("↓").on_hover_text("Następne (F3)").clicked() {
                action = Some(SearchAction::Next);
            }
            if running {
                ui.add(egui::Spinner::new());
            }
            ui.label(status);
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e.lines().last().unwrap_or(e))
                    .on_hover_text(RichText::new(e).monospace());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("✕").on_hover_text("Zamknij (Esc)").clicked() {
                    action = Some(SearchAction::Close);
                }
            });
        });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits on these lines.
    const HITS: [usize; 4] = [3, 10, 20, 30];

    fn find(
        start_row: usize,
        forward: bool,
        running: bool,
        visible: impl Fn(usize) -> bool,
    ) -> Step {
        find_hit(
            HITS.len(),
            running,
            start_row,
            forward,
            |r| HITS.get(r).copied(),
            visible,
            100,
        )
    }

    #[test]
    fn forward_and_backward() {
        assert_eq!(
            find(1, true, false, |_| true),
            Step::Found {
                line: 10,
                wrapped: false
            }
        );
        assert_eq!(
            find(1, false, false, |_| true),
            Step::Found {
                line: 3,
                wrapped: false
            }
        );
    }

    #[test]
    fn wraps_around() {
        assert_eq!(
            find(4, true, false, |_| true),
            Step::Found {
                line: 3,
                wrapped: true
            }
        );
        assert_eq!(
            find(0, false, false, |_| true),
            Step::Found {
                line: 30,
                wrapped: true
            }
        );
    }

    #[test]
    fn skips_invisible_hits() {
        let visible = |l: usize| l == 20;
        assert_eq!(
            find(0, true, false, visible),
            Step::Found {
                line: 20,
                wrapped: false
            }
        );
        assert_eq!(
            find(3, true, false, visible),
            Step::Found {
                line: 20,
                wrapped: true
            }
        );
        assert_eq!(
            find(1, false, false, visible),
            Step::Found {
                line: 20,
                wrapped: true
            }
        );
    }

    #[test]
    fn nothing_visible() {
        assert_eq!(find(2, true, false, |_| false), Step::NotFound);
        assert_eq!(find(2, false, false, |_| false), Step::NotFound);
    }

    #[test]
    fn waits_while_running_at_the_end() {
        assert_eq!(find(4, true, true, |_| true), Step::Pending);
        assert_eq!(find(0, false, true, |_| true), Step::Pending);
        // A hit before the end is returned even while running.
        assert_eq!(
            find(2, true, true, |_| true),
            Step::Found {
                line: 20,
                wrapped: false
            }
        );
    }

    #[test]
    fn empty_result() {
        let step = find_hit(0, false, 0, true, |_| None, |_| true, 10);
        assert_eq!(step, Step::NotFound);
    }

    #[test]
    fn step_limit() {
        let step = find_hit(1000, false, 0, true, Some, |l| l == 999, 10);
        assert_eq!(step, Step::NotFound);
    }
}
