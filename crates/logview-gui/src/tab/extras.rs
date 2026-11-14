//! Bookmarks, "go to" (line or time) and the timeline panel of a tab.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, FontId, Key, Modifiers, Rect, Sense, Stroke, pos2, vec2,
};
use logview_core::{Level, Timeline, TimelineJob, time};

use super::Tab;
use crate::filter_panel::level_color;
use crate::fmt;

/// Buckets of the timeline (aggregated to pixel columns when drawn).
const TIMELINE_BUCKETS: usize = 1000;
/// The timeline of a growing file is recomputed at most this often.
const TIMELINE_GROWTH_INTERVAL: Duration = Duration::from_secs(5);
/// Bookmarks listed in the side panel at most.
const MAX_LISTED_BOOKMARKS: usize = 500;
/// Level slots stacked from the bottom: errors first, so they stay visible.
const STACK: [usize; 7] = [6, 5, 4, 3, 2, 1, 0];

pub struct GotoDialog {
    text: String,
    error: Option<String>,
    focus: bool,
}

/// What the current timeline was computed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineKey {
    file_gen: u64,
    filter_gen: u64,
    filtered: bool,
    records: usize,
    rows: usize,
}

impl TimelineKey {
    fn same_source(&self, other: &Self) -> bool {
        Self {
            records: 0,
            rows: 0,
            ..*self
        } == Self {
            records: 0,
            rows: 0,
            ..*other
        }
    }
}

/// Parses "HH:MM" or "HH:MM:SS" into seconds since midnight.
fn parse_time_of_day(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let nums: Option<Vec<i64>> = parts.iter().map(|p| p.trim().parse().ok()).collect();
    let nums = nums?;
    let (h, m, s) = (nums[0], nums[1], *nums.get(2).unwrap_or(&0));
    ((0..24).contains(&h) && (0..60).contains(&m) && (0..61).contains(&s))
        .then_some(h * 3600 + m * 60 + s)
}

/// A level slot (0 = no level) as `Option<Level>`.
fn slot_level(slot: usize) -> Option<Level> {
    Level::from_code(slot as u8)
}

fn short_time(secs: i64, span: i64) -> String {
    let full = time::format_secs(secs);
    if span <= 2 * 86_400 {
        full[11..].to_owned()
    } else {
        full[5..16].to_owned()
    }
}

impl Tab {
    pub(super) fn handle_extra_keys(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (bookmark, next, prev, goto) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::B),
                i.consume_key(Modifiers::NONE, Key::F2),
                i.consume_key(Modifiers::SHIFT, Key::F2),
                i.consume_key(Modifiers::COMMAND, Key::G),
            )
        });
        if bookmark && let Some(line) = self.view.cursor.or(self.view.top_line()) {
            self.toggle_bookmark(line);
        }
        if next || prev {
            self.step_bookmark(next);
        }
        if goto {
            self.goto = Some(GotoDialog {
                text: String::new(),
                error: None,
                focus: true,
            });
        }
    }

    /// Line number as shown to the user (1-based; in a merged view the line
    /// in its source file).
    fn shown_line(&self, line: usize) -> String {
        match self
            .file
            .as_ref()
            .and_then(|f| f.origin(line).zip(f.merged_sources()))
        {
            Some(((src, l), sources)) => {
                format!("{}:{}", sources[src].name, fmt::num(l as u64 + 1))
            }
            None => fmt::num(line as u64 + 1),
        }
    }

    pub fn toggle_bookmark(&mut self, line: usize) {
        let label = self.shown_line(line);
        if self.bookmarks.remove(&line).is_some() {
            self.set_notice(format!("Usunięto zakładkę z linii {label}"));
        } else {
            self.bookmarks.insert(line, String::new());
            self.set_notice(format!("Zakładka na linii {label} (F2 – następna)"));
        }
    }

    /// Goes to the next (or previous) bookmark after the cursor, wrapping.
    pub fn step_bookmark(&mut self, forward: bool) {
        let (Some(&first), Some(&last)) = (
            self.bookmarks.keys().next(),
            self.bookmarks.keys().next_back(),
        ) else {
            self.set_notice("Brak zakładek (Ctrl+B albo klik w numer linii dodaje)".into());
            return;
        };
        let from = self.view.cursor.or(self.view.top_line()).unwrap_or(0);
        let target = if forward {
            self.bookmarks
                .range(from + 1..)
                .next()
                .map_or(first, |(l, _)| *l)
        } else {
            self.bookmarks
                .range(..from)
                .next_back()
                .map_or(last, |(l, _)| *l)
        };
        self.view.go_to_line(target);
    }

    pub(super) fn bookmarks_section(&mut self, ui: &mut egui::Ui) {
        if self.bookmarks.is_empty() {
            ui.weak("Ctrl+B albo klik w numer linii dodaje zakładkę; F2 / Shift+F2 przechodzi między nimi.");
            return;
        }
        let lines: Vec<usize> = self
            .bookmarks
            .keys()
            .copied()
            .take(MAX_LISTED_BOOKMARKS)
            .collect();
        let mut go = None;
        let mut remove = None;
        egui::Grid::new(("bookmarks-grid", self.id))
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for line in lines {
                    let label = self.shown_line(line);
                    let preview: String = self
                        .file
                        .as_ref()
                        .and_then(|f| f.line(line))
                        .map(|b| {
                            String::from_utf8_lossy(&b[..b.len().min(400)])
                                .chars()
                                .take(80)
                                .collect()
                        })
                        .unwrap_or_default();
                    if ui
                        .link(label)
                        .on_hover_text(egui::RichText::new(&preview).monospace())
                        .clicked()
                    {
                        go = Some(line);
                    }
                    if let Some(note) = self.bookmarks.get_mut(&line) {
                        ui.add(
                            egui::TextEdit::singleline(note)
                                .hint_text("notatka")
                                .desired_width((ui.available_width() - 30.0).max(80.0)),
                        );
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Usuń zakładkę")
                        .clicked()
                    {
                        remove = Some(line);
                    }
                    ui.end_row();
                }
            });
        if self.bookmarks.len() > MAX_LISTED_BOOKMARKS {
            ui.weak(format!(
                "… i {} więcej",
                self.bookmarks.len() - MAX_LISTED_BOOKMARKS
            ));
        }
        if ui.button("Usuń wszystkie").clicked() {
            self.bookmarks.clear();
        }
        if let Some(line) = go {
            self.view.go_to_line(line);
        }
        if let Some(line) = remove {
            self.bookmarks.remove(&line);
        }
    }

    // ---- go to -----------------------------------------------------------------

    pub(super) fn goto_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.goto else { return };
        let mut submit = false;
        let mut close = false;
        egui::Window::new("Przejdź do")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 80.0))
            .show(ctx, |ui| {
                ui.label("Numer linii albo czas (RRRR-MM-DD GG:MM[:SS] lub GG:MM[:SS]):");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut dialog.text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(300.0),
                );
                if dialog.focus {
                    edit.request_focus();
                    dialog.focus = false;
                }
                if edit.lost_focus() {
                    let (enter, esc) =
                        ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                    submit |= enter;
                    close |= esc;
                }
                if let Some(e) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.horizontal(|ui| {
                    submit |= ui.button("Przejdź").clicked();
                    close |= ui.button("Anuluj").clicked();
                });
            });
        if close {
            self.goto = None;
            return;
        }
        if submit {
            let text = dialog.text.clone();
            match self.go_to_input(&text) {
                Ok(()) => self.goto = None,
                Err(e) => {
                    if let Some(d) = &mut self.goto {
                        d.error = Some(e);
                        d.focus = true;
                    }
                }
            }
        }
    }

    fn go_to_input(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim();
        let digits: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '\u{2009}')
            .collect();
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let n: usize = digits
                .parse()
                .map_err(|_| "Za duży numer linii".to_owned())?;
            let count = self.file.as_ref().map_or(0, |f| f.line_count());
            if count == 0 {
                return Err("Plik nie ma linii".into());
            }
            self.view.go_to_line(n.clamp(1, count) - 1);
            return Ok(());
        }
        let secs = match time::parse_user_time(text) {
            Some(s) => s,
            None => {
                let of_day = parse_time_of_day(text)
                    .ok_or("Nie rozpoznano numeru linii ani czasu".to_owned())?;
                let (min, _) = self
                    .records()
                    .and_then(|r| r.time_bounds())
                    .ok_or("Plik nie ma znaczników czasu".to_owned())?;
                min.div_euclid(86_400) * 86_400 + of_day
            }
        };
        self.go_to_time(secs)
    }

    /// Goes to the first record at or after `secs`.
    pub fn go_to_time(&mut self, secs: i64) -> Result<(), String> {
        let records = self
            .records()
            .ok_or("Format pliku jeszcze nie rozpoznany".to_owned())?;
        let record = records
            .find_time(secs)
            .ok_or("Plik nie ma znaczników czasu".to_owned())?;
        let line = records
            .record(record)
            .map(|r| r.lines.start)
            .ok_or("Rekord niedostępny".to_owned())?;
        self.view.go_to_line(line);
        Ok(())
    }

    // ---- timeline --------------------------------------------------------------

    /// (Re)starts the timeline once the records (and filter) are final.
    pub(super) fn update_timeline(&mut self) {
        let Some(records) = self.records() else {
            self.timeline = None;
            self.timeline_key = None;
            return;
        };
        if self.parse_job.as_ref().is_some_and(|j| j.is_running()) {
            return;
        }
        let filtered = self.is_filtered_view();
        let rows = match &self.job {
            Some(job) if filtered => {
                if job.is_running() {
                    return;
                }
                Some(job.rows_handle())
            }
            _ => None,
        };
        let key = TimelineKey {
            file_gen: self.file_gen,
            filter_gen: if filtered { self.filter_gen } else { 0 },
            filtered,
            records: records.record_count(),
            rows: rows.as_ref().map_or(0, |r| r.len()),
        };
        if self.timeline_key == Some(key) {
            return;
        }
        if self.timeline_key.is_some_and(|k| k.same_source(&key))
            && self
                .timeline_started
                .is_some_and(|t| t.elapsed() < TIMELINE_GROWTH_INTERVAL)
        {
            return;
        }
        self.timeline = Some(TimelineJob::start(
            records,
            rows,
            TIMELINE_BUCKETS,
            std::sync::Arc::clone(&self.notify),
        ));
        self.timeline_key = Some(key);
        self.timeline_started = Some(Instant::now());
    }

    pub(super) fn timeline_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(90.0)
            .min_height(50.0)
            .show(ctx, |ui| {
                let result = self.timeline.as_ref().and_then(TimelineJob::result);
                match result {
                    Some(t) if !t.counts.is_empty() => self.draw_timeline(ui, &t),
                    _ => {
                        let running = self.timeline.as_ref().is_some_and(TimelineJob::is_running)
                            || self.parse_job.as_ref().is_some_and(|j| j.is_running());
                        ui.centered_and_justified(|ui| {
                            ui.weak(if self.file.is_none() {
                                "Oś czasu"
                            } else if running || self.format.is_none() {
                                "Oś czasu: liczenie…"
                            } else {
                                "Oś czasu: brak znaczników czasu w pliku"
                            });
                        });
                    }
                }
            });
    }

    fn draw_timeline(&mut self, ui: &mut egui::Ui, t: &Timeline) {
        let visuals = ui.visuals().clone();
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let label_h = 14.0;
        let plot = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - label_h));
        let painter = ui.painter_at(rect);
        painter.rect_filled(plot, 2.0, visuals.extreme_bg_color);
        let width = plot.width().max(1.0);
        let span = t.end() - t.start;
        let x_to_time =
            |x: f32| t.start + (((x - plot.left()) / width).clamp(0.0, 1.0) * span as f32) as i64;
        let time_to_x = |s: i64| plot.left() + ((s - t.start) as f32 / span.max(1) as f32) * width;

        // Aggregate buckets to pixel columns.
        let columns = width as usize;
        let n = t.counts.len();
        let mut sums: Vec<[u32; 7]> = vec![[0; 7]; columns.max(1)];
        for (b, counts) in t.counts.iter().enumerate() {
            let c = (b * columns / n).min(columns.saturating_sub(1));
            for (s, v) in sums[c].iter_mut().zip(counts) {
                *s += v;
            }
        }
        let max = sums
            .iter()
            .map(|c| c.iter().sum::<u32>())
            .max()
            .unwrap_or(0)
            .max(1);
        // Columns narrower than a bucket: draw each column; wider: each bucket spans several.
        let col_w = (width / columns.max(1) as f32).max(1.0);
        for (c, counts) in sums.iter().enumerate() {
            let total: u32 = counts.iter().sum();
            if total == 0 {
                continue;
            }
            let x = plot.left() + c as f32 * col_w;
            let mut y = plot.bottom();
            for &slot in &STACK {
                let h = counts[slot] as f32 / max as f32 * (plot.height() - 2.0);
                if h <= 0.0 {
                    continue;
                }
                let r = Rect::from_min_max(pos2(x, y - h), pos2(x + col_w.max(1.0), y));
                painter.rect_filled(r, 0.0, level_color(slot_level(slot)).gamma_multiply(0.85));
                y -= h;
            }
        }

        // Current time filter.
        let spec = &self.filter_panel.spec;
        if spec.time_from.is_some() || spec.time_to.is_some() {
            let x0 = spec
                .time_from
                .map_or(plot.left(), time_to_x)
                .max(plot.left());
            let x1 = spec
                .time_to
                .map_or(plot.right(), time_to_x)
                .min(plot.right());
            let shade = Rect::from_min_max(pos2(x0, plot.top()), pos2(x1, plot.bottom()));
            painter.rect_filled(
                shade,
                0.0,
                Color32::from_rgba_unmultiplied(0x29, 0x79, 0xFF, 40),
            );
        }
        // Range being dragged.
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        if let (Some(x0), Some(p)) = (self.timeline_drag, pointer) {
            let r = Rect::from_min_max(
                pos2(x0.min(p.x), plot.top()),
                pos2(x0.max(p.x), plot.bottom()),
            );
            painter.rect_filled(
                r,
                0.0,
                Color32::from_rgba_unmultiplied(0x29, 0x79, 0xFF, 70),
            );
        }
        // Position of the top line of the view.
        if let Some(records) = self.records()
            && let Some(line) = self.view.top_line()
            && let Some(ts) = records.time_of_line(line)
        {
            let x = time_to_x(ts);
            painter.vline(
                x,
                plot.y_range(),
                Stroke::new(1.5_f32, visuals.strong_text_color()),
            );
        }

        // Axis labels.
        let font = FontId::proportional(11.0);
        let weak = visuals.weak_text_color();
        let ticks = ((width / 140.0) as i64).clamp(1, 12);
        for i in 0..=ticks {
            let s = t.start + span * i / ticks;
            let align = match i {
                0 => Align2::LEFT_TOP,
                _ if i == ticks => Align2::RIGHT_TOP,
                _ => Align2::CENTER_TOP,
            };
            painter.text(
                pos2(time_to_x(s), plot.bottom() + 1.0),
                align,
                short_time(s, span),
                font.clone(),
                weak,
            );
        }

        // Hover: counts of the bucket under the pointer.
        if let Some(p) = resp.hover_pos()
            && self.timeline_drag.is_none()
            && let Some(b) = t.bucket_of(x_to_time(p.x))
        {
            let (from, to) = t.bucket_range(b);
            let mut text = format!(
                "{} – {}\n{} rekordów",
                time::format_secs(from),
                time::format_secs(to),
                fmt::num(t.total(b) as u64)
            );
            for &slot in &STACK {
                let n = t.counts[b][slot];
                if n > 0 {
                    text.push_str(&format!(
                        "\n{}: {}",
                        slot_level(slot).map_or("brak", Level::name),
                        fmt::num(n as u64)
                    ));
                }
            }
            text.push_str("\n\nklik: przejdź, przeciągnij: filtr czasu");
            resp.clone().on_hover_text_at_pointer(text);
        }

        // Click: go to that moment. Drag: set the time filter.
        if resp.drag_started()
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.timeline_drag = Some(p.x);
        }
        if resp.drag_stopped()
            && let (Some(x0), Some(p)) = (self.timeline_drag.take(), resp.interact_pointer_pos())
        {
            let (a, b) = (x_to_time(x0.min(p.x)), x_to_time(x0.max(p.x)));
            if b > a {
                self.filter_panel.set_time_range(Some(a), Some(b));
                self.set_notice(format!(
                    "Filtr czasu: {} – {}",
                    time::format_secs(a),
                    time::format_secs(b)
                ));
            }
        } else if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && let Err(e) = self.go_to_time(x_to_time(p.x))
        {
            self.set_notice(e);
        }
        if t.untimed > 0 {
            painter.text(
                pos2(plot.right() - 4.0, plot.top() + 2.0),
                Align2::RIGHT_TOP,
                format!("bez czasu: {}", fmt::num(t.untimed)),
                font,
                weak,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_of_day() {
        assert_eq!(parse_time_of_day("12:30"), Some(45_000));
        assert_eq!(parse_time_of_day("00:00:05"), Some(5));
        assert_eq!(parse_time_of_day("24:00"), None);
        assert_eq!(parse_time_of_day("12"), None);
        assert_eq!(parse_time_of_day("a:b"), None);
    }

    #[test]
    fn short_labels() {
        let t = 1_767_225_600 + 3723; // 2026-01-01 01:02:03
        assert_eq!(short_time(t, 3600), "01:02:03");
        assert_eq!(short_time(t, 10 * 86_400), "01-01 01:02");
    }

    #[test]
    fn timeline_key_sources() {
        let a = TimelineKey {
            file_gen: 1,
            filter_gen: 2,
            filtered: true,
            records: 10,
            rows: 5,
        };
        assert!(a.same_source(&TimelineKey {
            records: 20,
            rows: 9,
            ..a
        }));
        assert!(!a.same_source(&TimelineKey { filter_gen: 3, ..a }));
    }
}
