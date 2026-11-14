//! Virtualised log view.
//!
//! egui's `ScrollArea` keeps the scroll offset in `f32` pixels, which loses
//! precision beyond a few million rows. This view keeps its position as a row
//! number, renders only the visible rows and draws its own scrollbars, so it
//! behaves the same for 100 rows and 500 M rows.
//!
//! The view shows rows of a [`Source`]: either every line of the file, or the
//! rows of a filter result (matches plus context lines). [`Decor`] adds color
//! rules, search highlighting and the marker bar on the scrollbar.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, Event, FontFamily, FontId, Key, Modifiers, Rect, Sense, Stroke,
    TextFormat, pos2, text::LayoutJob, vec2,
};
use logview_core::{
    CompiledFilter, CompiledFormat, DisplayBuilder, FilterJob, HighlightRule, Highlighter,
    LineHighlight, LogFile, MinimapJob, Pattern, Rgba, SEARCH_BIT, StyleId, display_cut,
};

use crate::fmt;
use crate::selection::Selection;

/// Copying stops after this many bytes.
const COPY_LIMIT_BYTES: usize = 64 << 20;
const SCROLLBAR: f32 = 16.0;
/// Vertical space added above the first row of a non-adjacent group.
const GAP: f32 = 5.0;
/// Style id of search hits in [`LineHighlight`] runs.
const SEARCH_STYLE: StyleId = StyleId::MAX;
/// Highlight results kept for recently drawn lines.
const HIGHLIGHT_CACHE: usize = 4096;

/// Colors of merged sources (gutter strip, legend), by source index.
const SOURCE_COLORS: [Color32; 8] = [
    Color32::from_rgb(0x42, 0xA5, 0xF5),
    Color32::from_rgb(0xFF, 0xA7, 0x26),
    Color32::from_rgb(0x66, 0xBB, 0x6A),
    Color32::from_rgb(0xAB, 0x47, 0xBC),
    Color32::from_rgb(0xEF, 0x53, 0x50),
    Color32::from_rgb(0x26, 0xC6, 0xDA),
    Color32::from_rgb(0xD4, 0xE1, 0x57),
    Color32::from_rgb(0x8D, 0x6E, 0x63),
];

pub fn source_color(index: usize) -> Color32 {
    SOURCE_COLORS[index % SOURCE_COLORS.len()]
}

const SEARCH_BG: Color32 = Color32::from_rgb(0xFF, 0xE0, 0x4D);
const CURRENT_HIT_BG: Color32 = Color32::from_rgb(0xFF, 0x8F, 0x00);
const MINIMAP_SEARCH: Color32 = Color32::from_rgb(0xFF, 0xD6, 0x00);
const BOOKMARK: Color32 = Color32::from_rgb(0x29, 0x79, 0xFF);
/// Bookmark ticks drawn on the scrollbar at most.
const MAX_BOOKMARK_TICKS: usize = 10_000;

/// What happened in the view during a frame.
#[derive(Default)]
pub struct ViewOutput {
    /// Status message (e.g. after copying).
    pub notice: Option<String>,
    /// The gutter of this file line was clicked: toggle its bookmark.
    pub toggle_bookmark: Option<usize>,
}

/// User preferences shared by all views.
pub struct ViewPrefs {
    pub wrap: bool,
    pub font_size: f32,
    /// Longer lines are cut in the list (full text is in the details panel).
    pub max_display_bytes: usize,
    pub tab_width: usize,
    /// Show parsed fields in columns (when the format has fields).
    pub columns: bool,
    pub hidden_columns: HashSet<String>,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        Self {
            wrap: false,
            font_size: 13.0,
            max_display_bytes: 4096,
            tab_width: 4,
            columns: false,
            hidden_columns: HashSet::new(),
        }
    }
}

/// Widest column (in characters) before its content is clipped; the last
/// column is never clipped.
const MAX_COLUMN_CHARS: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A line of the unfiltered file.
    Plain,
    /// A line matching the filter.
    Match,
    /// A context line around a match.
    Context,
}

struct Row<'a> {
    line: usize,
    kind: RowKind,
    /// The previous row is not the previous line (draw a group separator).
    gap_before: bool,
    bytes: &'a [u8],
}

/// What the view displays.
pub enum Source<'a> {
    /// Every line of the file; lines matching `highlight` get a marker.
    Full {
        file: &'a LogFile,
        highlight: Option<&'a CompiledFilter>,
    },
    /// Rows of a filter result.
    Filtered {
        file: &'a LogFile,
        job: &'a FilterJob,
    },
}

impl<'a> Source<'a> {
    pub fn file(&self) -> &'a LogFile {
        match self {
            Self::Full { file, .. } | Self::Filtered { file, .. } => file,
        }
    }

    pub fn row_count(&self) -> usize {
        match self {
            Self::Full { file, .. } => file.line_count(),
            Self::Filtered { job, .. } => job.len(),
        }
    }

    pub fn line_of_row(&self, row: usize) -> Option<usize> {
        match self {
            Self::Full { file, .. } => (row < file.line_count()).then_some(row),
            Self::Filtered { job, .. } => job.get(row).map(|e| e.line),
        }
    }

    /// First row showing `line` or a later line.
    fn row_of_line(&self, line: usize) -> usize {
        match self {
            Self::Full { .. } => line,
            Self::Filtered { job, .. } => job.row_of_line(line),
        }
    }

    /// Whether [`Source::row_of_line`] is final for `line` (the filter may
    /// still be scanning earlier lines).
    fn can_locate(&self, line: usize) -> bool {
        match self {
            Self::Full { file, .. } => line < file.line_count() || !file.is_indexing(),
            Self::Filtered { job, .. } => job.processed_lines() > line || !job.is_running(),
        }
    }

    fn rows(&self, start: usize, count: usize) -> Vec<Row<'a>> {
        match *self {
            Self::Full { file, highlight } => file
                .lines(start..start.saturating_add(count))
                .into_iter()
                .map(|(line, bytes)| Row {
                    line,
                    kind: match highlight {
                        Some(f) if f.matches(bytes) => RowKind::Match,
                        _ => RowKind::Plain,
                    },
                    gap_before: false,
                    bytes,
                })
                .collect(),
            Self::Filtered { file, job } => {
                // Fetch one row before `start` to know if the first row starts a group.
                let from = start.saturating_sub(1);
                let entries = job.rows(from..start.saturating_add(count));
                let context = job.has_context();
                let mut prev = None;
                let mut out = Vec::with_capacity(count);
                for (i, entry) in entries.into_iter().enumerate() {
                    let gap = context && prev.is_some_and(|p: usize| p + 1 != entry.line);
                    prev = Some(entry.line);
                    if from + i < start {
                        continue;
                    }
                    out.push(Row {
                        line: entry.line,
                        kind: if entry.is_match {
                            RowKind::Match
                        } else {
                            RowKind::Context
                        },
                        gap_before: gap,
                        bytes: file.line(entry.line).unwrap_or_default(),
                    });
                }
                out
            }
        }
    }
}

/// Coloring and markers drawn on top of the rows.
pub struct Decor<'a> {
    pub highlighter: Option<&'a Highlighter>,
    /// The rules `highlighter` was compiled from (style ids index into it).
    pub rules: &'a [HighlightRule],
    pub search: Option<&'a Pattern>,
    /// Line of the current search hit (drawn stronger).
    pub current_hit: Option<usize>,
    pub minimap: Option<&'a MinimapJob>,
    /// Changes whenever `highlighter` or `search` change (cache key).
    pub generation: u64,
    pub bold_family: Option<&'a FontFamily>,
    /// Format used for the column view.
    pub format: Option<&'a CompiledFormat>,
    /// The file is followed: stick to the bottom unless scrolled up.
    pub follow: bool,
    /// Bookmarked file lines with their notes.
    pub bookmarks: &'a BTreeMap<usize, String>,
}

impl Decor<'_> {
    fn has_highlighting(&self) -> bool {
        self.highlighter.is_some_and(|h| !h.is_empty()) || self.search.is_some()
    }

    /// Highlight of a single column value (not cached; values are short).
    fn cell_highlight(&self, bytes: &[u8]) -> Option<LineHighlight> {
        if !self.has_highlighting() {
            return None;
        }
        let empty = Highlighter::default();
        let extra: Vec<(&Pattern, StyleId)> =
            self.search.map(|s| (s, SEARCH_STYLE)).into_iter().collect();
        Some(self.highlighter.unwrap_or(&empty).highlight(bytes, &extra))
    }
}

/// How a run of text is drawn.
struct TextStyle<'a> {
    font: &'a FontId,
    bold: Option<&'a FontId>,
    base: Color32,
    dim: bool,
    current_hit: bool,
    rules: &'a [HighlightRule],
}

/// Appends `hay` to `job`, styled by `highlight` (plain if `None`).
fn append_runs(
    job: &mut LayoutJob,
    builder: &mut DisplayBuilder,
    hay: &[u8],
    highlight: Option<&LineHighlight>,
    s: &TextStyle<'_>,
) {
    let Some(h) = highlight else {
        let text = builder.push(hay);
        job.append(&text, 0.0, TextFormat::simple(s.font.clone(), s.base));
        return;
    };
    for (range, id) in &h.runs {
        let text = builder.push(&hay[range.clone()]);
        let format = match id {
            None => TextFormat::simple(s.font.clone(), s.base),
            Some(SEARCH_STYLE) => TextFormat {
                font_id: s.font.clone(),
                color: Color32::BLACK,
                background: if s.current_hit {
                    CURRENT_HIT_BG
                } else {
                    SEARCH_BG
                },
                ..Default::default()
            },
            Some(id) => {
                let style = s
                    .rules
                    .get(*id as usize)
                    .map(|r| r.style)
                    .unwrap_or_default();
                let mut color = style.fg.map_or(s.base, rgba);
                let mut background = style.bg.map_or(Color32::TRANSPARENT, rgba);
                if s.dim {
                    color = color.gamma_multiply(0.55);
                    background = background.gamma_multiply(0.55);
                }
                TextFormat {
                    font_id: match (s.bold, style.bold) {
                        (Some(b), true) => b.clone(),
                        _ => s.font.clone(),
                    },
                    color,
                    background,
                    italics: style.italic,
                    ..Default::default()
                }
            }
        };
        job.append(&text, 0.0, format);
    }
}

#[derive(Default)]
pub struct LogView {
    /// First visible row.
    top: usize,
    /// Wheel movement not yet converted into whole rows.
    scroll_px: f32,
    h_offset: f32,
    /// Rows fully visible in the last frame.
    page_rows: usize,
    /// Pointer offset within the thumb while dragging the vertical scrollbar.
    vgrab: Option<f32>,
    /// Selected rows (row numbers of the current source).
    pub selection: Selection,
    anchor: Option<usize>,
    /// File line last clicked; shown in the details panel.
    pub cursor: Option<usize>,
    /// Line to bring to the top once the source can locate it.
    pending_line: Option<usize>,
    /// Line to select and bring into view once the source can locate it.
    pending_goto: Option<usize>,
    /// File line shown in the top row in the last frame.
    top_line: Option<usize>,
    /// File lines drawn in the last frame, `(first, last)`.
    visible: Option<(usize, usize)>,
    highlight_cache: HashMap<usize, Arc<LineHighlight>>,
    highlight_cache_key: (u64, usize),
    /// Column view: names of the shown columns and their widths in
    /// characters. Widths only grow while scrolling (no jitter) and reset
    /// when the columns change.
    col_names: Vec<String>,
    col_widths: Vec<usize>,
    /// Live tail: keep the last rows in view.
    stick: bool,
}

impl LogView {
    /// First and last file line drawn in the last frame.
    pub fn visible_lines(&self) -> Option<(usize, usize)> {
        self.visible
    }

    /// File line in the top row (or the pending target, if any).
    pub fn top_line(&self) -> Option<usize> {
        self.pending_line.or(self.top_line)
    }

    /// Scrolls so that `line` (or the first row after it) is at the top, as
    /// soon as the source can locate it.
    pub fn scroll_to_line(&mut self, line: usize) {
        self.pending_line = Some(line);
    }

    /// Selects `line` and scrolls it into view (centered if it was not
    /// visible).
    /// Live tail: scroll to the end and keep following it.
    pub fn stick_to_bottom(&mut self) {
        self.stick = true;
    }

    /// Whether the view follows the end (false after scrolling up).
    pub fn is_stuck(&self) -> bool {
        self.stick
    }

    pub fn go_to_line(&mut self, line: usize) {
        self.stick = false;
        self.pending_goto = Some(line);
        self.pending_line = None;
    }

    /// Called when the rows change meaning (other source or new filter).
    pub fn source_changed(&mut self) {
        self.selection.clear();
        self.anchor = None;
        if let Some(line) = self.top_line() {
            self.pending_line = Some(line);
        }
        self.top = 0;
    }

    fn line_highlight(
        &mut self,
        decor: &Decor<'_>,
        line: usize,
        hay: &[u8],
        max_bytes: usize,
    ) -> Option<Arc<LineHighlight>> {
        if decor.highlighter.is_none_or(Highlighter::is_empty) && decor.search.is_none() {
            return None;
        }
        let key = (decor.generation, max_bytes);
        if self.highlight_cache_key != key || self.highlight_cache.len() > HIGHLIGHT_CACHE {
            self.highlight_cache.clear();
            self.highlight_cache_key = key;
        }
        let entry = self.highlight_cache.entry(line).or_insert_with(|| {
            let empty = Highlighter::default();
            let highlighter = decor.highlighter.unwrap_or(&empty);
            let extra: Vec<(&Pattern, StyleId)> = decor
                .search
                .map(|s| (s, SEARCH_STYLE))
                .into_iter()
                .collect();
            Arc::new(highlighter.highlight(hay, &extra))
        });
        Some(Arc::clone(entry))
    }

    /// Draws the view into the remaining space of `ui`. Returns a status
    /// message to show to the user, if any.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        source: &Source<'_>,
        decor: &Decor<'_>,
        prefs: &mut ViewPrefs,
    ) -> ViewOutput {
        let mut notice = None;
        let mut toggle_bookmark = None;
        let file = source.file();
        let count = source.row_count();
        let font = FontId::monospace(prefs.font_size);
        let bold_font = decor
            .bold_family
            .map(|f| FontId::new(prefs.font_size, f.clone()));
        let (row_h, char_w) = ui.fonts(|f| (f.row_height(&font) + 2.0, f.glyph_width(&font, '0')));
        let visuals = ui.visuals().clone();

        // ---- layout -------------------------------------------------------
        let full = ui.available_rect_before_wrap();
        let hsb_h = if prefs.wrap { 0.0 } else { SCROLLBAR };
        let body = Rect::from_min_max(full.min, pos2(full.max.x - SCROLLBAR, full.max.y - hsb_h));
        let vsb_rect =
            Rect::from_min_max(pos2(body.max.x, full.min.y), pos2(full.max.x, body.max.y));
        let hsb_rect =
            Rect::from_min_max(pos2(full.min.x, body.max.y), pos2(body.max.x, full.max.y));

        let body_resp = ui.allocate_rect(body, Sense::click());
        let vsb_resp = ui.allocate_rect(vsb_rect, Sense::click_and_drag());
        let hsb_resp = (!prefs.wrap).then(|| ui.allocate_rect(hsb_rect, Sense::click_and_drag()));

        let digits = file.line_count().max(1).to_string().len().max(4);
        let gutter_w = digits as f32 * char_w + 14.0;
        let gutter = Rect::from_min_max(body.min, pos2(body.min.x + gutter_w, body.max.y));
        let text_rect = Rect::from_min_max(pos2(gutter.max.x + 6.0, body.min.y), body.max);

        // Column view: visible columns (index into the format's fields, name).
        let columns: Vec<(usize, String)> = match decor.format {
            Some(format) if prefs.columns => format
                .columns()
                .iter()
                .enumerate()
                .filter(|(_, n)| !prefs.hidden_columns.contains(*n))
                .map(|(i, n)| (i, n.clone()))
                .collect(),
            _ => Vec::new(),
        };
        let column_mode = !columns.is_empty();
        let names: Vec<String> = columns.iter().map(|(_, n)| n.clone()).collect();
        if self.col_names != names {
            self.col_widths = names.iter().map(|n| n.chars().count().max(3)).collect();
            self.col_names = names;
        }
        let header_h = if column_mode { row_h + 4.0 } else { 0.0 };

        let rows_fit = (((body.height() - header_h) / row_h).floor() as usize).max(1);
        let page = if prefs.wrap || matches!(source, Source::Filtered { .. }) {
            self.page_rows.max(1)
        } else {
            rows_fit
        };
        // With wrapping or group gaps, rows have variable height, so allow
        // scrolling until the last row is at the top.
        let variable_height =
            prefs.wrap || matches!(source, Source::Filtered { job, .. } if job.has_context());
        let max_top = if variable_height {
            count.saturating_sub(1)
        } else {
            count.saturating_sub(rows_fit)
        };

        // ---- input --------------------------------------------------------
        let mut delta: isize = 0;
        let mut jump: Option<usize> = None;
        let hovered = body_resp.hovered()
            || vsb_resp.hovered()
            || hsb_resp.as_ref().is_some_and(|r| r.hovered());
        if hovered {
            let (scroll, zoom, shift) =
                ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.shift));
            if zoom != 1.0 {
                prefs.font_size = (prefs.font_size * zoom).clamp(6.0, 48.0);
            } else {
                let (dx, dy) = if shift && scroll.x == 0.0 {
                    (scroll.y, 0.0)
                } else {
                    (scroll.x, scroll.y)
                };
                self.scroll_px -= dy;
                let whole = (self.scroll_px / row_h).trunc();
                self.scroll_px -= whole * row_h;
                delta += whole as isize;
                self.h_offset -= dx;
            }
        }

        let mut copy = false;
        if !ui.ctx().wants_keyboard_input() {
            ui.input_mut(|i| {
                let none = Modifiers::NONE;
                let cmd = Modifiers::COMMAND;
                if i.consume_key(none, Key::ArrowDown) {
                    delta += 1;
                }
                if i.consume_key(none, Key::ArrowUp) {
                    delta -= 1;
                }
                if i.consume_key(none, Key::PageDown) {
                    delta += page as isize;
                }
                if i.consume_key(none, Key::PageUp) {
                    delta -= page as isize;
                }
                if i.consume_key(none, Key::Home) || i.consume_key(cmd, Key::Home) {
                    jump = Some(0);
                }
                if i.consume_key(none, Key::End) || i.consume_key(cmd, Key::End) {
                    jump = Some(count.saturating_sub(page));
                }
                if i.consume_key(none, Key::ArrowRight) {
                    self.h_offset += char_w * 8.0;
                }
                if i.consume_key(none, Key::ArrowLeft) {
                    self.h_offset -= char_w * 8.0;
                }
                if i.consume_key(cmd, Key::A) && count > 0 {
                    self.selection.select_range(0, count - 1);
                }
                if i.consume_key(none, Key::Escape) {
                    self.selection.clear();
                    self.cursor = None;
                }
                copy = i.events.iter().any(|e| matches!(e, Event::Copy));
            });
        }

        let user_scrolled = delta != 0 || jump.is_some() || vsb_resp.is_pointer_button_down_on();
        let mut top = match jump {
            Some(row) => row,
            None => self.top.saturating_add_signed(delta),
        };
        if user_scrolled {
            self.pending_line = None;
        } else if let Some(line) = self.pending_line
            && source.can_locate(line)
        {
            top = source.row_of_line(line);
            self.pending_line = None;
        }
        if let Some(line) = self.pending_goto
            && source.can_locate(line)
        {
            let row = source.row_of_line(line);
            if source.line_of_row(row) == Some(line) {
                self.selection.select_only(row);
                self.anchor = Some(row);
            }
            self.cursor = Some(line);
            let visible = self.page_rows.max(1);
            if row < top || row >= top + visible {
                top = row.saturating_sub(visible / 2);
            }
            self.pending_goto = None;
        }
        // Live tail: the last rows (for variable row heights, the rows that
        // fitted in the last frame) stay in view until the user scrolls up;
        // scrolling back to the end sticks again.
        let bottom = if variable_height {
            count.saturating_sub(self.page_rows.max(1))
        } else {
            max_top
        };
        if decor.follow {
            if user_scrolled {
                self.stick = top >= bottom;
            }
            if self.stick && self.pending_goto.is_none() && self.pending_line.is_none() {
                top = bottom;
            }
        }
        if top > max_top {
            top = max_top;
            self.scroll_px = self.scroll_px.min(0.0);
        }
        if top == 0 {
            self.scroll_px = self.scroll_px.max(0.0);
        }

        // Vertical scrollbar dragging.
        let track = vsb_rect.shrink(2.0);
        let thumb_h = if count == 0 {
            track.height()
        } else {
            (track.height() * (rows_fit as f64 / count as f64).min(1.0) as f32)
                .max(24.0)
                .min(track.height())
        };
        let thumb_frac = |top: usize| {
            if max_top == 0 {
                0.0
            } else {
                top as f32 / max_top as f32
            }
        };
        let thumb_top = |top: usize| track.top() + (track.height() - thumb_h) * thumb_frac(top);
        if vsb_resp.is_pointer_button_down_on() {
            if let Some(p) = vsb_resp.interact_pointer_pos() {
                let grab = *self.vgrab.get_or_insert_with(|| {
                    let t = thumb_top(top);
                    if (t..=t + thumb_h).contains(&p.y) {
                        p.y - t
                    } else {
                        thumb_h / 2.0
                    }
                });
                let free = (track.height() - thumb_h).max(1.0);
                let frac = ((p.y - grab - track.top()) / free).clamp(0.0, 1.0) as f64;
                top = (frac * max_top as f64).round() as usize;
            }
        } else {
            self.vgrab = None;
        }
        self.top = top;

        // ---- rows ---------------------------------------------------------
        let painter = ui.painter_at(body);
        painter.rect_filled(body, 0.0, visuals.extreme_bg_color);
        painter.rect_filled(gutter, 0.0, visuals.faint_bg_color);
        let text_painter = ui.painter_at(text_rect);
        let text_color = visuals.text_color();
        let weak = visuals.weak_text_color();
        let match_marker = visuals.warn_fg_color;
        let hover_fill = if visuals.dark_mode {
            Color32::from_white_alpha(10)
        } else {
            Color32::from_black_alpha(10)
        };
        let hover = body_resp.hover_pos();
        let h_offset = if prefs.wrap { 0.0 } else { self.h_offset };
        let mark_matches = match source {
            Source::Full { highlight, .. } => highlight.is_some(),
            Source::Filtered { job, .. } => job.has_context(),
        };
        let wrap_width = if prefs.wrap {
            text_rect.width().max(char_w * 10.0)
        } else {
            f32::INFINITY
        };

        // Column x offsets (relative to the text area) from the widths
        // measured so far; widths grown in this frame apply from the next.
        let col_gap = char_w * 2.0;
        let widths = self.col_widths.clone();
        let mut col_x = Vec::with_capacity(widths.len());
        let mut x = 0.0;
        for w in &widths {
            col_x.push(x);
            x += *w as f32 * char_w + col_gap;
        }

        // (row index, file line, rect)
        let mut drawn: Vec<(usize, usize, Rect)> = Vec::with_capacity(rows_fit + 1);
        let mut max_w: f32 = 0.0;
        let mut y = body.top() + header_h;
        for (offset, row) in source.rows(top, rows_fit + 1).into_iter().enumerate() {
            if y >= body.bottom() {
                break;
            }
            let row_index = top + offset;
            if row.gap_before {
                let sep_y = y + GAP / 2.0;
                painter.hline(
                    body.x_range(),
                    sep_y,
                    Stroke::new(1.0_f32, weak.gamma_multiply(0.5)),
                );
                y += GAP;
            }
            let dim = row.kind == RowKind::Context;
            let cut = display_cut(row.bytes, prefs.max_display_bytes);
            let hay = &row.bytes[..cut];
            let highlight = self.line_highlight(decor, row.line, hay, prefs.max_display_bytes);
            let line_style = highlight
                .as_ref()
                .and_then(|h| h.line_rule)
                .and_then(|id| decor.rules.get(id as usize))
                .map(|r| r.style);

            let mut base_color = line_style.and_then(|s| s.fg).map_or(text_color, rgba);
            if dim {
                base_color = base_color.gamma_multiply(0.55);
            }
            let style = TextStyle {
                font: &font,
                bold: bold_font.as_ref(),
                base: base_color,
                dim,
                current_hit: decor.current_hit == Some(row.line),
                rules: decor.rules,
            };
            let hidden = row.bytes.len() - cut;
            let add_hidden = |job: &mut LayoutJob| {
                if hidden > 0 {
                    job.append(
                        &format!("  …(+{} bajtów)", fmt::num(hidden as u64)),
                        0.0,
                        TextFormat {
                            font_id: font.clone(),
                            color: weak,
                            italics: true,
                            ..Default::default()
                        },
                    );
                }
            };

            // Galleys to draw: (x offset in the text area, galley, clip width).
            let mut pieces = Vec::with_capacity(columns.len().max(1));
            let fields = if column_mode {
                decor.format.and_then(|f| f.fields(hay))
            } else {
                None
            };
            match fields {
                Some(values) => {
                    for (k, (field, _)) in columns.iter().enumerate() {
                        let last = k + 1 == columns.len();
                        let bytes: &[u8] =
                            values.get(*field).and_then(|v| v.as_deref()).unwrap_or(b"");
                        let cell = decor.cell_highlight(bytes);
                        let mut job = LayoutJob::default();
                        let mut builder = DisplayBuilder::new(prefs.tab_width);
                        append_runs(&mut job, &mut builder, bytes, cell.as_ref(), &style);
                        if last {
                            add_hidden(&mut job);
                            job.wrap.max_width = if prefs.wrap {
                                (text_rect.width() - col_x[k]).max(char_w * 10.0)
                            } else {
                                f32::INFINITY
                            };
                        }
                        let galley = ui.fonts(|f| f.layout_job(job));
                        if !last {
                            let chars = (galley.size().x / char_w).ceil() as usize;
                            self.col_widths[k] =
                                self.col_widths[k].max(chars.min(MAX_COLUMN_CHARS));
                        }
                        let clip = (!last).then(|| widths[k] as f32 * char_w);
                        pieces.push((col_x[k], galley, clip));
                    }
                }
                None => {
                    // Whole line; in the column view a continuation line
                    // (e.g. stack trace) spans all columns, slightly indented.
                    let mut job = LayoutJob::default();
                    let mut builder = DisplayBuilder::new(prefs.tab_width);
                    append_runs(&mut job, &mut builder, hay, highlight.as_deref(), &style);
                    add_hidden(&mut job);
                    job.wrap.max_width = wrap_width;
                    let indent = if column_mode { char_w * 2.0 } else { 0.0 };
                    pieces.push((indent, ui.fonts(|f| f.layout_job(job)), None));
                }
            }
            let h = pieces
                .iter()
                .map(|(_, g, _)| g.size().y + 2.0)
                .fold(row_h, f32::max);
            let rect = Rect::from_min_max(pos2(body.left(), y), pos2(body.right(), y + h));

            if let Some(bg) = line_style.and_then(|s| s.bg) {
                let text_area = Rect::from_min_max(pos2(gutter.right(), y), rect.max);
                painter.rect_filled(text_area, 0.0, rgba(bg));
            }
            if self.selection.contains(row_index) {
                painter.rect_filled(rect, 0.0, visuals.selection.bg_fill);
            } else if hover.is_some_and(|p| rect.contains(p)) {
                painter.rect_filled(rect, 0.0, hover_fill);
            }
            if self.cursor == Some(row.line) {
                let marker = Rect::from_min_size(rect.min, vec2(3.0, h));
                painter.rect_filled(marker, 0.0, visuals.selection.stroke.color);
            }
            if mark_matches && row.kind == RowKind::Match {
                let marker = Rect::from_min_size(pos2(gutter.right() - 3.0, y), vec2(3.0, h));
                painter.rect_filled(marker, 0.0, match_marker);
            }
            if let Some(note) = decor.bookmarks.get(&row.line) {
                let cell = Rect::from_min_max(pos2(gutter.left(), y), pos2(gutter.right(), y + h));
                painter.rect_filled(cell, 0.0, BOOKMARK.gamma_multiply(0.45));
                painter.rect_filled(Rect::from_min_size(cell.min, vec2(3.0, h)), 0.0, BOOKMARK);
                if !note.is_empty() && hover.is_some_and(|p| cell.contains(p)) {
                    egui::show_tooltip_at_pointer(
                        ui.ctx(),
                        ui.layer_id(),
                        egui::Id::new("bookmark-note"),
                        |ui| {
                            ui.label(note);
                        },
                    );
                }
            }
            // Merged view: source color strip and the line number in the source.
            let origin = file.origin(row.line);
            if let Some((src, _)) = origin {
                let strip = Rect::from_min_size(pos2(gutter.left() + 4.0, y), vec2(4.0, h));
                painter.rect_filled(strip, 0.0, source_color(src));
            }
            let shown_line = origin.map_or(row.line, |(_, l)| l);
            painter.text(
                pos2(gutter.right() - 6.0, y + 1.0),
                Align2::RIGHT_TOP,
                (shown_line + 1).to_string(),
                font.clone(),
                weak,
            );
            for (x, galley, clip) in pieces {
                let pos = pos2(text_rect.left() + x - h_offset, y + 1.0);
                max_w = max_w.max(x + galley.size().x);
                match clip {
                    Some(w) => {
                        let cell = Rect::from_min_size(pos2(pos.x, y), vec2(w + col_gap * 0.5, h))
                            .intersect(text_rect);
                        ui.painter_at(cell).galley(pos, galley, base_color);
                    }
                    None => text_painter.galley(pos, galley, base_color),
                }
            }
            drawn.push((row_index, row.line, rect));
            y += h;
        }

        if column_mode {
            let header = Rect::from_min_max(body.min, pos2(body.right(), body.top() + header_h));
            painter.rect_filled(header, 0.0, visuals.faint_bg_color);
            let line_color = weak.gamma_multiply(0.4);
            painter.hline(
                body.x_range(),
                header.bottom(),
                Stroke::new(1.0_f32, line_color),
            );
            let names_area = Rect::from_min_max(
                pos2(text_rect.left(), header.top()),
                pos2(text_rect.right(), header.bottom()),
            );
            let hp = ui.painter_at(names_area);
            for (k, (_, name)) in columns.iter().enumerate() {
                let x = text_rect.left() + col_x[k] - h_offset;
                hp.text(
                    pos2(x, header.top() + 2.0),
                    Align2::LEFT_TOP,
                    name,
                    bold_font.clone().unwrap_or_else(|| font.clone()),
                    visuals.strong_text_color(),
                );
                if k > 0 {
                    hp.vline(
                        x - col_gap / 2.0,
                        header.y_range(),
                        Stroke::new(1.0_f32, line_color),
                    );
                }
            }
        }

        if count == 0 {
            let msg = match source {
                Source::Full { file, .. } if file.is_indexing() => "Indeksowanie…",
                Source::Full { .. } => "Plik jest pusty",
                Source::Filtered { job, .. } if job.is_running() => "Filtrowanie…",
                Source::Filtered { .. } => "Brak pasujących linii",
            };
            painter.text(
                body.center(),
                Align2::CENTER_CENTER,
                msg,
                FontId::proportional(16.0),
                weak,
            );
        }

        self.page_rows = drawn
            .iter()
            .filter(|(_, _, r)| r.bottom() <= body.bottom())
            .count()
            .max(1);
        self.top_line = drawn.first().map(|d| d.1).or(self.top_line);
        self.visible = drawn.first().zip(drawn.last()).map(|(a, b)| (a.1, b.1));

        // ---- clicks -------------------------------------------------------
        if body_resp.clicked()
            && let Some(p) = body_resp.interact_pointer_pos()
            && p.x < gutter.right()
            && let Some(&(_, line, _)) = drawn.iter().find(|(_, _, r)| r.contains(p))
        {
            // Click in the line-number gutter: bookmark.
            toggle_bookmark = Some(line);
        } else if body_resp.clicked()
            && let Some(p) = body_resp.interact_pointer_pos()
            && let Some(&(row, line, _)) = drawn.iter().find(|(_, _, r)| r.contains(p))
        {
            let mods = ui.input(|i| i.modifiers);
            match self.anchor {
                Some(anchor) if mods.shift => self.selection.select_range(anchor, row),
                _ if mods.command => {
                    self.selection.toggle(row);
                    self.anchor = Some(row);
                }
                _ => {
                    self.selection.select_only(row);
                    self.anchor = Some(row);
                }
            }
            self.cursor = Some(line);
        }

        // ---- horizontal scrollbar ------------------------------------------
        let max_h = (max_w + char_w * 2.0 - text_rect.width()).max(0.0);
        if let Some(hsb) = &hsb_resp {
            let track = hsb_rect.shrink(2.0);
            let thumb_w = if max_h == 0.0 {
                track.width()
            } else {
                (track.width() * text_rect.width() / (max_w + char_w * 2.0))
                    .max(24.0)
                    .min(track.width())
            };
            if hsb.is_pointer_button_down_on()
                && max_h > 0.0
                && let Some(p) = hsb.interact_pointer_pos()
            {
                let free = (track.width() - thumb_w).max(1.0);
                let frac = ((p.x - track.left() - thumb_w / 2.0) / free).clamp(0.0, 1.0);
                self.h_offset = frac * max_h;
            }
            self.h_offset = self.h_offset.clamp(0.0, max_h);
            let p = ui.painter();
            p.rect_filled(hsb_rect, 0.0, visuals.extreme_bg_color);
            if max_h > 0.0 {
                let x = track.left() + (track.width() - thumb_w) * (self.h_offset / max_h);
                let thumb =
                    Rect::from_min_size(pos2(x, track.top()), vec2(thumb_w, track.height()));
                p.rect_filled(
                    thumb,
                    4.0,
                    thumb_color(&visuals, hsb.hovered() || hsb.dragged()),
                );
            }
        } else {
            self.h_offset = 0.0;
        }

        // ---- vertical scrollbar with markers --------------------------------
        let p = ui.painter();
        p.rect_filled(vsb_rect, 0.0, visuals.extreme_bg_color);
        // The markers cover the rows that existed when they were computed;
        // while a followed file grows they fill the matching top part.
        if let Some(minimap) = decor.minimap
            && minimap.total_rows() <= count
            && count > 0
        {
            let fraction = minimap.total_rows() as f32 / count as f32;
            paint_markers(p, track, fraction, minimap, decor.rules);
        }
        // Bookmark ticks (full width, over the rule markers).
        if count > 0 {
            for &line in decor.bookmarks.keys().take(MAX_BOOKMARK_TICKS) {
                let row = source.row_of_line(line);
                if source.line_of_row(row) != Some(line) {
                    continue; // not shown in this view
                }
                let y = track.top() + track.height() * (row as f32 / count as f32);
                let tick = Rect::from_min_max(pos2(track.left(), y), pos2(track.right(), y + 2.0));
                p.rect_filled(tick, 0.0, BOOKMARK);
            }
        }
        if max_top > 0 {
            let thumb = Rect::from_min_size(
                pos2(track.left(), thumb_top(self.top)),
                vec2(track.width(), thumb_h),
            );
            let active = vsb_resp.hovered() || vsb_resp.is_pointer_button_down_on();
            let c = thumb_color(&visuals, active);
            let c = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 170);
            p.rect_filled(thumb, 4.0, c);
        }

        // ---- copy -----------------------------------------------------------
        if copy && !self.selection.is_empty() {
            let (text, copied, complete) = collect_selection(source, &self.selection);
            ui.ctx().copy_text(text);
            notice = Some(copy_notice(copied, complete));
        }

        ViewOutput {
            notice,
            toggle_bookmark,
        }
    }
}

fn rgba([r, g, b, a]: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Paints one tick per pixel row of the scrollbar track: search hits on the
/// left half, the highest-priority marked rule on the right half.
fn paint_markers(
    p: &egui::Painter,
    track: Rect,
    fraction: f32,
    minimap: &MinimapJob,
    rules: &[HighlightRule],
) {
    let colors: Vec<Option<Color32>> = rules.iter().map(|r| r.marker_color().map(rgba)).collect();
    let mid = track.center().x;
    minimap.with_buckets(|buckets| {
        let n = buckets.len();
        let px = track.height().max(1.0) as usize;
        let shown = ((px as f32 * fraction).round() as usize).clamp(1, px);
        for y in 0..shown {
            let b0 = y * n / shown;
            let b1 = ((y + 1) * n / shown).clamp(b0 + 1, n);
            let mask = buckets[b0..b1].iter().fold(0, |acc, m| acc | m);
            if mask == 0 {
                continue;
            }
            let top = track.top() + y as f32;
            if mask & SEARCH_BIT != 0 {
                let r = Rect::from_min_max(pos2(track.left(), top), pos2(mid, top + 2.0));
                p.rect_filled(r, 0.0, MINIMAP_SEARCH);
            }
            let rule_bits = mask & !SEARCH_BIT;
            if rule_bits != 0
                && let Some(Some(color)) = colors.get(rule_bits.trailing_zeros() as usize)
            {
                let r = Rect::from_min_max(pos2(mid, top), pos2(track.right(), top + 2.0));
                p.rect_filled(r, 0.0, *color);
            }
        }
    });
}

/// Status message after copying `copied` lines.
pub fn copy_notice(copied: usize, complete: bool) -> String {
    if complete {
        format!("Skopiowano {} linii", fmt::num(copied as u64))
    } else {
        format!(
            "Skopiowano tylko {} linii (limit {})",
            fmt::num(copied as u64),
            fmt::bytes(COPY_LIMIT_BYTES as u64)
        )
    }
}

pub const fn copy_limit() -> usize {
    COPY_LIMIT_BYTES
}

fn thumb_color(visuals: &egui::Visuals, active: bool) -> Color32 {
    if active {
        visuals.widgets.active.bg_fill
    } else {
        visuals.widgets.inactive.bg_fill
    }
}

/// Selected rows joined with `\n`, up to [`COPY_LIMIT_BYTES`].
/// Returns the text, the number of lines copied and whether all were copied.
fn collect_selection(source: &Source<'_>, selection: &Selection) -> (String, usize, bool) {
    let file = source.file();
    let mut out = String::new();
    let mut copied = 0;
    for row in selection.iter() {
        let Some(bytes) = source.line_of_row(row).and_then(|l| file.line(l)) else {
            break;
        };
        if out.len() + bytes.len() > COPY_LIMIT_BYTES {
            return (out, copied, false);
        }
        out.push_str(&String::from_utf8_lossy(bytes));
        out.push('\n');
        copied += 1;
    }
    (out, copied, true)
}
