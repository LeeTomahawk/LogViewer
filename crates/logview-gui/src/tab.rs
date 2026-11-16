//! One open file (a tab): the view, filters, format, search, coloring,
//! marker bar, live tail and the panels that edit them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, FontFamily, Key, Modifiers};
use logview_core::{
    CompiledFilter, CompiledFormat, Detection, ExportJob, FilterJob, FilterRule, FilterSpec,
    HighlightRule, Highlighter, LogFile, MinimapJob, Notify, ParseJob, Pattern, Profile, Records,
    TailStatus, ViewSettings, detect, rows_to_string,
};

mod convert;
mod extras;
mod ts_picker;

use crate::filter_panel::{FilterPanel, MetaInfo};
use crate::fmt;
use crate::format_panel::{FormatInfo, FormatPanel};
use crate::highlight_panel::HighlightPanel;
use crate::search::{SearchAction, SearchBar, Step, find_hit};
use crate::view::{Decor, LogView, Source, ViewPrefs, copy_limit, copy_notice, source_color};

/// The details panel shows at most this much of a record.
const DETAILS_LIMIT: usize = 64 << 10;
/// The details panel shows at most this many lines of a record.
const DETAILS_MAX_LINES: usize = 1000;
/// Lines from the start of the file used to detect and check the format.
const FORMAT_SAMPLE_LINES: usize = 500;
const NOTICE_TIME: Duration = Duration::from_secs(4);
/// Resolution of the scrollbar marker bar.
const MINIMAP_BUCKETS: usize = 4096;
/// Search hits checked per F3 press when looking for one visible in the
/// filtered view.
const MAX_SEARCH_STEPS: usize = 2_000_000;

/// What the current marker bar was computed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MinimapKey {
    file_gen: u64,
    highlight_gen: u64,
    search_gen: u64,
    filter_gen: u64,
    filtered: bool,
    rows: usize,
}

impl MinimapKey {
    /// Same rows (only their number may differ, e.g. a followed file grew).
    fn same_source(&self, other: &Self) -> bool {
        Self { rows: 0, ..*self } == Self { rows: 0, ..*other }
    }
}

/// Markers of a growing file are recomputed at most this often.
const MINIMAP_GROWTH_INTERVAL: Duration = Duration::from_secs(5);
/// Appended data kept in memory before the followed file is reopened.
const TAIL_MEMORY_LIMIT: u64 = 512 << 20;

enum StepResult {
    Found,
    /// The search is still running; try again later.
    Pending,
    NotFound,
}

struct Details {
    line: usize,
    /// Format generation the details were computed with.
    format_gen: u64,
    /// Lines of the record shown.
    lines: std::ops::Range<usize>,
    /// Record text (or pretty JSON), limited to [`DETAILS_LIMIT`].
    text: String,
    truncated: bool,
    total_bytes: usize,
    /// Parsed fields of the record's first line.
    fields: Vec<(String, String)>,
}

pub struct Tab {
    /// Unique per tab (for egui ids and closing).
    pub id: u64,
    pub file: Option<LogFile>,
    pub view: LogView,
    pub prefs: ViewPrefs,
    pub error: Option<String>,
    pub notice: Option<(String, Instant)>,
    details: Option<Details>,
    notify: Notify,

    pub filter_panel: FilterPanel,
    /// Last successfully compiled, active filter.
    compiled: Option<Arc<CompiledFilter>>,
    job: Option<FilterJob>,
    /// Show only filter results (otherwise the full file with match markers).
    pub show_filtered: bool,
    export: Option<ExportJob>,
    filter_gen: u64,
    file_gen: u64,

    pub highlight_panel: HighlightPanel,
    highlighter: Option<Arc<Highlighter>>,
    /// Rules `highlighter` was compiled from.
    applied_rules: Vec<HighlightRule>,
    highlight_gen: u64,
    bold_family: Option<FontFamily>,

    pub search: SearchBar,
    search_pattern: Option<Arc<Pattern>>,
    search_job: Option<FilterJob>,
    search_gen: u64,
    current_hit: Option<usize>,
    /// Navigation requested before the search had results (`true` = next).
    pending_step: Option<bool>,

    minimap: Option<MinimapJob>,
    minimap_key: Option<MinimapKey>,

    pub format_panel: FormatPanel,
    detection: Option<Detection>,
    pub format: Option<Arc<CompiledFormat>>,
    /// Fraction of the sample matching `format`.
    format_ratio: Option<f32>,
    format_gen: u64,
    parse_job: Option<ParseJob>,

    /// Profile loaded or saved last in this tab (to show whether the
    /// settings changed since).
    pub active_profile: Option<Profile>,

    /// Follow the end of the file (live tail).
    pub follow: bool,
    minimap_started: Option<Instant>,

    /// Bookmarked file lines with their notes.
    pub bookmarks: BTreeMap<usize, String>,
    goto: Option<extras::GotoDialog>,
    ts_picker: Option<ts_picker::TsPicker>,
    timeline: Option<logview_core::TimelineJob>,
    timeline_key: Option<extras::TimelineKey>,
    timeline_started: Option<Instant>,
    /// Start x of a range being dragged on the timeline.
    timeline_drag: Option<f32>,

    /// Encoding chosen for this tab.
    pub encoding: logview_core::TextEncoding,
    /// Conversion (decompression / transcoding) in progress.
    preparing: Option<logview_core::PrepareJob>,
}

impl Tab {
    pub fn new(id: u64, notify: Notify, bold_family: Option<FontFamily>) -> Self {
        Self {
            id,
            file: None,
            view: LogView::default(),
            prefs: ViewPrefs::default(),
            error: None,
            notice: None,
            details: None,
            notify,
            filter_panel: FilterPanel::default(),
            compiled: None,
            job: None,
            show_filtered: true,
            export: None,
            filter_gen: 0,
            file_gen: 0,
            highlight_panel: HighlightPanel::default(),
            highlighter: None,
            applied_rules: Vec::new(),
            highlight_gen: 0,
            bold_family,
            search: SearchBar::default(),
            search_pattern: None,
            search_job: None,
            search_gen: 0,
            current_hit: None,
            pending_step: None,
            minimap: None,
            minimap_key: None,
            format_panel: FormatPanel::default(),
            detection: None,
            format: None,
            format_ratio: None,
            format_gen: 0,
            parse_job: None,
            active_profile: None,
            follow: false,
            minimap_started: None,
            bookmarks: BTreeMap::new(),
            goto: None,
            ts_picker: None,
            timeline: None,
            timeline_key: None,
            timeline_started: None,
            timeline_drag: None,
            encoding: logview_core::TextEncoding::Auto,
            preparing: None,
        }
    }

    /// Tab title: file name (or the merge name).
    pub fn title(&self) -> String {
        match &self.file {
            Some(f) => f.path().file_name().map_or_else(
                || f.path().display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            None => match self.preparing_name() {
                Some(name) => format!("{name} …"),
                None => "(pusta)".to_owned(),
            },
        }
    }

    /// Records of this tab's file (for merging and the details panel).
    pub fn records_handle(&self) -> Option<Records> {
        self.records()
    }

    /// Whether the file is ready to be merged with others: indexed, format
    /// known and records parsed (or caught up when followed).
    pub fn merge_ready(&self) -> bool {
        let Some(file) = &self.file else { return false };
        file.merged_sources().is_none()
            && self.format.is_some()
            && self
                .records()
                .is_some_and(|r| r.is_done() || r.is_caught_up())
    }

    /// Opens a new file in this tab, e.g. a merge result.
    pub fn set_file(&mut self, file: LogFile) {
        self.attach(file);
    }

    // ---- live tail ------------------------------------------------------------

    /// Turns following the end of the file on or off.
    fn toggle_follow(&mut self) {
        if self
            .file
            .as_ref()
            .is_some_and(|f| f.merged_sources().is_some())
        {
            self.set_notice("Widoku scalonego nie można śledzić".into());
            return;
        }
        self.follow = !self.follow;
        self.apply_follow();
    }

    /// Makes the open file follow (or not) according to `self.follow`.
    fn apply_follow(&mut self) {
        let Some(file) = &self.file else { return };
        if self.follow {
            file.start_follow(Arc::clone(&self.notify));
            self.view.stick_to_bottom();
        } else {
            file.stop_follow();
        }
    }

    /// Reopens the current file (keeping the follow state) after rotation,
    /// truncation, or when the appended data takes too much memory.
    fn reopen(&mut self, reason: &str) {
        let Some(path) = self.file.as_ref().map(|f| f.path().to_path_buf()) else {
            return;
        };
        if self.open(path) {
            self.apply_follow();
            self.set_notice(reason.to_owned());
        } else {
            // E.g. the file is gone for good: stop instead of retrying.
            self.follow = false;
        }
    }

    fn check_tail(&mut self) {
        if !self.follow {
            return;
        }
        let Some(file) = &self.file else { return };
        match file.tail_status() {
            TailStatus::Truncated => {
                // Line numbers no longer match the content.
                self.bookmarks.clear();
                self.reopen("Plik został obcięty – wczytano go od nowa");
            }
            TailStatus::Replaced => {
                self.bookmarks.clear();
                self.reopen("Plik został podmieniony (rotacja) – wczytano nowy");
            }
            _ if file.tail_memory() > TAIL_MEMORY_LIMIT => {
                self.reopen("Wczytano plik ponownie, aby zwolnić pamięć dopisanych danych");
            }
            _ => {}
        }
    }

    /// Shows `file` in this tab, resetting everything derived from the old
    /// file (filters and rules stay and are applied to the new one).
    fn attach(&mut self, file: LogFile) {
        self.cancel_export();
        // The format of the new file is detected once enough lines are
        // indexed; jobs that need records restart then.
        self.parse_job = None;
        self.detection = None;
        self.format = None;
        self.format_ratio = None;
        self.format_gen += 1;
        self.job = self
            .compiled
            .as_ref()
            .and_then(|f| self.start_job(&file, f));
        self.search_job = self
            .search_pattern
            .as_ref()
            .and_then(|_| self.start_search_job(&file));
        self.file = Some(file);
        self.file_gen += 1;
        self.minimap = None;
        self.timeline = None;
        self.timeline_key = None;
        self.current_hit = None;
        self.view = LogView::default();
        self.details = None;
        self.error = None;
    }

    // ---- format --------------------------------------------------------------

    /// Detects the format once the first lines are available.
    fn maybe_detect_format(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        if self.detection.is_some() {
            return;
        }
        if file.line_count() < FORMAT_SAMPLE_LINES && file.is_indexing() {
            return;
        }
        let sample = file.lines(0..FORMAT_SAMPLE_LINES);
        let sample: Vec<&[u8]> = sample.iter().map(|(_, b)| *b).collect();
        self.detection = Some(detect(&sample));
        if file.conversion().is_none()
            && self.encoding == logview_core::TextEncoding::Auto
            && sample
                .iter()
                .any(|l| !logview_core::encoding::looks_like_utf8(l))
        {
            self.set_notice(
                "Plik nie wygląda na UTF-8 – wybierz kodowanie na pasku stanu (np. Windows-1250)"
                    .into(),
            );
        }
        self.apply_format();
    }

    /// Compiles the chosen format and restarts parsing (and record-based
    /// filtering). On error the previous format stays in effect.
    fn apply_format(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        if self.detection.is_none() {
            // Not detected yet; `maybe_detect_format` calls back.
            return;
        }
        let format = self.format_panel.resolve(self.detection.as_ref());
        let sample = file.lines(0..FORMAT_SAMPLE_LINES);
        let sample: Vec<&[u8]> = sample.iter().map(|(_, b)| *b).collect();
        match format.compile(&sample) {
            Ok(compiled) => {
                self.format_panel.error = None;
                self.format_ratio = Some(compiled.match_ratio(&sample));
                let compiled = Arc::new(compiled);
                self.parse_job = Some(ParseJob::start(
                    file,
                    Arc::clone(&compiled),
                    self.format_panel.multiline,
                    Arc::clone(&self.notify),
                ));
                self.format = Some(compiled);
                self.format_gen += 1;
                self.details = None;
                self.apply_filters();
            }
            Err(e) => self.format_panel.error = Some(e),
        }
    }

    fn records(&self) -> Option<Records> {
        self.parse_job.as_ref().map(ParseJob::records)
    }

    fn meta_info(&self) -> MetaInfo {
        match self.records() {
            Some(r) => MetaInfo {
                level_counts: Some(r.level_counts()),
                time_bounds: r.time_bounds(),
            },
            None => MetaInfo::default(),
        }
    }

    // ---- highlighting ------------------------------------------------------

    fn apply_highlight(&mut self) {
        match Highlighter::compile(&self.highlight_panel.rules) {
            Ok(h) => {
                self.highlight_panel.clear_errors();
                self.highlighter = Some(Arc::new(h));
                self.applied_rules = self.highlight_panel.rules.clone();
                self.highlight_gen += 1;
            }
            Err(errors) => self.highlight_panel.set_errors(&errors),
        }
        self.highlight_panel.hit_center = None;
    }

    // ---- search ------------------------------------------------------------

    fn search_spec(&self) -> FilterSpec {
        FilterSpec {
            rules: vec![FilterRule {
                pattern: self.search.query.clone(),
                kind: self.search.kind,
                case_sensitive: self.search.case_sensitive,
                exclude: false,
                enabled: true,
            }],
            ..Default::default()
        }
    }

    fn start_search_job(&self, file: &LogFile) -> Option<FilterJob> {
        let filter = self.search_spec().compile().ok()?;
        Some(FilterJob::start(
            file.clone(),
            Arc::new(filter),
            0,
            0,
            // Search hits are lines, not records.
            None,
            Arc::clone(&self.notify),
        ))
    }

    fn apply_search(&mut self) {
        self.current_hit = None;
        self.search_gen += 1;
        if self.search.query.is_empty() {
            self.search.error = None;
            self.search_pattern = None;
            self.search_job = None;
            return;
        }
        match Pattern::new(
            &self.search.query,
            self.search.kind,
            self.search.case_sensitive,
        ) {
            Ok(pattern) => {
                self.search.error = None;
                self.search_pattern = Some(Arc::new(pattern));
                self.search_job = self.file.as_ref().and_then(|f| self.start_search_job(f));
            }
            Err(e) => {
                self.search.error = Some(e);
                self.search_pattern = None;
                self.search_job = None;
            }
        }
    }

    fn close_search(&mut self) {
        self.search.open = false;
        self.search_pattern = None;
        self.search_job = None;
        self.current_hit = None;
        self.pending_step = None;
        self.search_gen += 1;
    }

    /// Whether `line` is shown in the current view.
    fn line_visible_in_view(&self, line: usize) -> bool {
        match &self.job {
            Some(job) if self.show_filtered => job
                .get(job.row_of_line(line))
                .is_some_and(|e| e.line == line),
            _ => true,
        }
    }

    /// Moves to the next (`forward`) or previous search hit visible in the
    /// current view, wrapping around once.
    fn search_step(&mut self, forward: bool) -> StepResult {
        let Some(job) = &self.search_job else {
            return StepResult::NotFound;
        };
        // Start after the current hit / cursor; from the top line if neither.
        let (start, inclusive) = match self.current_hit.or(self.view.cursor) {
            Some(line) => (line, false),
            None => (self.view.top_line().unwrap_or(0), true),
        };
        // Row of the first hit after the start (forward) or the first hit at
        // or after the start, i.e. one past the candidates (backward).
        let start_row = match (forward, inclusive) {
            (true, true) | (false, false) => job.row_of_line(start),
            (true, false) | (false, true) => job.row_of_line(start + 1),
        };
        let step = find_hit(
            job.len(),
            job.is_running(),
            start_row,
            forward,
            |row| job.get(row).map(|e| e.line),
            |line| self.line_visible_in_view(line),
            MAX_SEARCH_STEPS,
        );
        match step {
            Step::Found { line, wrapped } => {
                self.current_hit = Some(line);
                self.view.go_to_line(line);
                if wrapped {
                    let msg = if forward {
                        "Wyszukiwanie od początku"
                    } else {
                        "Wyszukiwanie od końca"
                    };
                    self.set_notice(msg.into());
                }
                StepResult::Found
            }
            Step::Pending => StepResult::Pending,
            Step::NotFound => StepResult::NotFound,
        }
    }

    pub fn step_search(&mut self, forward: bool) {
        if self.search_job.is_none() {
            if self.search.query.is_empty() {
                self.search.open_and_focus();
            }
            return;
        }
        match self.search_step(forward) {
            StepResult::Found => self.pending_step = None,
            StepResult::Pending => self.pending_step = Some(forward),
            StepResult::NotFound => {
                self.pending_step = None;
                self.set_notice("Brak trafień w widoku".into());
            }
        }
    }

    fn search_status(&self) -> String {
        let Some(job) = &self.search_job else {
            return String::new();
        };
        let total = job.match_count();
        let more = if job.is_running() { "+" } else { "" };
        if total == 0 {
            return if job.is_running() {
                "szukam…".into()
            } else {
                "brak trafień".into()
            };
        }
        match self.current_hit {
            Some(line)
                if job
                    .get(job.row_of_line(line))
                    .is_some_and(|e| e.line == line) =>
            {
                format!(
                    "{} / {}{more}",
                    fmt::num(job.row_of_line(line) as u64 + 1),
                    fmt::num(total as u64)
                )
            }
            _ => format!("{}{more} trafień", fmt::num(total as u64)),
        }
    }

    fn search_bar(&mut self, ctx: &egui::Context) {
        if !self.search.open {
            return;
        }
        let status = self.search_status();
        let running = self.search_job.as_ref().is_some_and(FilterJob::is_running);
        let mut action = None;
        egui::TopBottomPanel::top("search").show(ctx, |ui| {
            action = self.search.show(ui, &status, running);
        });
        if self.search.take_due() {
            self.apply_search();
        }
        match action {
            Some(SearchAction::Next) => self.step_search(true),
            Some(SearchAction::Prev) => self.step_search(false),
            Some(SearchAction::Close) => self.close_search(),
            None => {}
        }
    }

    // ---- marker bar ----------------------------------------------------------

    /// What the marker bar should be computed for right now (rows left 0).
    fn minimap_source(&self) -> MinimapKey {
        let filtered = self.is_filtered_view();
        MinimapKey {
            file_gen: self.file_gen,
            highlight_gen: self.highlight_gen,
            search_gen: self.search_gen,
            filter_gen: if filtered { self.filter_gen } else { 0 },
            filtered,
            rows: 0,
        }
    }

    /// Whether the marker bar belongs to the rows currently shown.
    fn minimap_matches_view(&self) -> bool {
        let source = self.minimap_source();
        self.minimap_key.is_some_and(|k| k.same_source(&source))
    }

    /// (Re)starts the marker bar job once the rows of the view are final.
    fn update_minimap(&mut self) {
        let Some(file) = &self.file else {
            self.minimap = None;
            return;
        };
        let has_rules = self.highlighter.as_ref().is_some_and(|h| h.has_markers());
        if !has_rules && self.search_pattern.is_none() {
            self.minimap = None;
            self.minimap_key = None;
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
        if file.is_indexing() {
            return;
        }
        let key = MinimapKey {
            rows: rows.as_ref().map_or_else(|| file.line_count(), |r| r.len()),
            ..self.minimap_source()
        };
        if self.minimap_key == Some(key) {
            return;
        }
        // A followed file only grew: recompute at most every few seconds
        // (each run scans all rows); the old markers stay meanwhile.
        if self.minimap_key.is_some_and(|k| k.same_source(&key))
            && self
                .minimap_started
                .is_some_and(|t| t.elapsed() < MINIMAP_GROWTH_INTERVAL)
        {
            return;
        }
        self.minimap_started = Some(Instant::now());
        let highlighter = self
            .highlighter
            .clone()
            .unwrap_or_else(|| Arc::new(Highlighter::default()));
        self.minimap = Some(MinimapJob::start(
            file.clone(),
            rows,
            highlighter,
            self.search_pattern.clone(),
            MINIMAP_BUCKETS,
            Arc::clone(&self.notify),
        ));
        self.minimap_key = Some(key);
    }

    /// Starts filtering. Filters work on records when lines are joined into
    /// records or when they use level/time; that needs the parse job, so
    /// without it (format not detected yet) no job is started and
    /// `apply_format` restarts filtering later.
    fn start_job(&self, file: &LogFile, filter: &Arc<CompiledFilter>) -> Option<FilterJob> {
        let records = self.records();
        let by_records = match &records {
            Some(r) => r.multiline() || filter.needs_meta(),
            None if filter.needs_meta() => return None,
            None => false,
        };
        Some(FilterJob::start(
            file.clone(),
            Arc::clone(filter),
            self.filter_panel.spec.context_before,
            self.filter_panel.spec.context_after,
            if by_records { records } else { None },
            Arc::clone(&self.notify),
        ))
    }

    fn is_filtered_view(&self) -> bool {
        self.show_filtered && self.job.is_some()
    }

    /// Compiles the edited filter and restarts the scan. On compile errors the
    /// previous filter stays in effect and the errors are shown in the panel.
    fn apply_filters(&mut self) {
        let compiled = match self.filter_panel.spec.compile() {
            Ok(c) => c,
            Err(errors) => {
                self.filter_panel.set_errors(&errors);
                return;
            }
        };
        self.filter_panel.clear_errors();
        let was_filtered = self.is_filtered_view();
        self.cancel_export();
        self.filter_gen += 1;
        if compiled.is_active() {
            let compiled = Arc::new(compiled);
            self.job = self
                .file
                .as_ref()
                .and_then(|f| self.start_job(f, &compiled));
            self.compiled = Some(compiled);
        } else {
            self.job = None;
            self.compiled = None;
        }
        if was_filtered || self.is_filtered_view() {
            self.view.source_changed();
        }
    }

    pub fn toggle_filtered_view(&mut self) {
        self.show_filtered = !self.show_filtered;
        if self.job.is_some() {
            // Keep the cursor line in view if there is one, else the top line.
            if let Some(line) = self.view.cursor {
                self.view.source_changed();
                self.view.scroll_to_line(line);
            } else {
                self.view.source_changed();
            }
        }
    }

    fn cancel_export(&mut self) {
        if let Some(export) = self.export.take() {
            export.cancel();
        }
    }

    fn start_export(&mut self) {
        let (Some(file), Some(job)) = (&self.file, &self.job) else {
            return;
        };
        let stem = file
            .path()
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "log".into());
        let dialog = rfd::FileDialog::new()
            .set_title("Eksportuj wynik filtra")
            .set_file_name(format!("{stem}-filtr.log"));
        let dialog = match file.path().parent() {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        };
        if let Some(path) = dialog.save_file() {
            if path == file.path() {
                self.error = Some("Nie można nadpisać otwartego pliku.".into());
                return;
            }
            self.export = Some(ExportJob::start(
                file.clone(),
                job.rows_handle(),
                &path,
                Arc::clone(&self.notify),
            ));
        }
    }

    /// Reports a finished export.
    fn poll_export(&mut self) {
        let Some(export) = &self.export else { return };
        if export.is_running() {
            return;
        }
        if let Some(e) = export.error() {
            self.error = Some(format!(
                "Eksport do {} nie powiódł się: {e}",
                export.path().display()
            ));
        } else if !export.was_cancelled() {
            let text = format!(
                "Wyeksportowano {} linii do {}",
                fmt::num(export.rows_written() as u64),
                export.path().display()
            );
            self.set_notice(text);
        }
        self.export = None;
    }

    fn copy_result(&mut self, ctx: &egui::Context) {
        let (Some(file), Some(job)) = (&self.file, &self.job) else {
            return;
        };
        let (text, copied, complete) = rows_to_string(file, &job.rows_handle(), copy_limit());
        ctx.copy_text(text);
        self.set_notice(copy_notice(copied, complete));
    }

    fn filter_side_panel(&mut self, ctx: &egui::Context) {
        // Never let the panel take more than half of the window, whatever its
        // content asks for; the user can still resize it within that range.
        let max_width = (ctx.screen_rect().width() * 0.5).max(300.0);
        egui::SidePanel::left("filters")
            .resizable(true)
            .default_width(380.0)
            .min_width(260.0)
            .max_width(max_width)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .max_height(ui.available_height() - 150.0)
                    .show(ui, |ui| {
                        let title = if self.format_panel.error.is_some() {
                            "Format ⚠"
                        } else {
                            "Format"
                        };
                        egui::CollapsingHeader::new(egui::RichText::new(title).heading())
                            .id_salt("format")
                            .default_open(true)
                            .show(ui, |ui| {
                                let parse = self.records().map(|r| {
                                    let total = self.file.as_ref().map_or(0, LogFile::line_count);
                                    let p = if total == 0 {
                                        1.0
                                    } else {
                                        r.processed_lines() as f32 / total as f32
                                    };
                                    (r.record_count(), r.is_done(), p)
                                });
                                let info = FormatInfo {
                                    detected: self.detection.as_ref(),
                                    active: self.format.as_deref(),
                                    active_ratio: self.format_ratio,
                                    parse,
                                };
                                self.format_panel.show(
                                    ui,
                                    &info,
                                    &mut self.prefs.columns,
                                    &mut self.prefs.hidden_columns,
                                );
                            });
                        ui.add_space(6.0);
                        let meta = self.meta_info();
                        egui::CollapsingHeader::new(egui::RichText::new("Filtry").heading())
                            .default_open(true)
                            .show(ui, |ui| self.filter_panel.show(ui, &meta));
                        ui.add_space(6.0);
                        let title = if self.highlight_panel.has_errors() {
                            "Kolorowanie ⚠"
                        } else {
                            "Kolorowanie"
                        };
                        egui::CollapsingHeader::new(egui::RichText::new(title).heading())
                            .id_salt("highlight-rules")
                            .default_open(true)
                            .show(ui, |ui| self.highlight_panel.show(ui));
                        ui.add_space(6.0);
                        let title = format!("Zakładki ({})", self.bookmarks.len());
                        egui::CollapsingHeader::new(egui::RichText::new(title).heading())
                            .id_salt("bookmarks")
                            .default_open(true)
                            .show(ui, |ui| self.bookmarks_section(ui));
                    });
                ui.separator();
                self.filter_status(ui);
            });
    }

    fn filter_status(&mut self, ui: &mut egui::Ui) {
        let Some(file) = &self.file else {
            ui.weak("Otwórz plik, aby filtrować.");
            return;
        };
        let Some(job) = &self.job else {
            ui.weak("Brak aktywnych filtrów — widoczny cały plik.");
            return;
        };
        let total = file.line_count();
        let checked = if job.is_running() {
            job.processed_lines()
        } else {
            total
        };
        ui.label(format!(
            "Pasujących: {} z {} linii",
            fmt::num(job.match_count() as u64),
            fmt::num(checked as u64)
        ));
        if job.has_context() {
            ui.weak(format!(
                "Wierszy z kontekstem: {}",
                fmt::num(job.len() as u64)
            ));
        }
        if job.is_running() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                let p = if total == 0 {
                    0.0
                } else {
                    checked as f32 / total as f32
                };
                ui.add(egui::ProgressBar::new(p).show_percentage());
            });
        } else {
            ui.weak(format!(
                "Czas: {:.2} s, pamięć wyniku: {}",
                job.elapsed().as_secs_f64(),
                fmt::bytes(job.memory_bytes() as u64)
            ));
        }
        if self.filter_panel.has_errors() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Błędny filtr — obowiązuje poprzedni.",
            );
        }

        let mut toggle = false;
        let mut filtered = self.show_filtered;
        if ui
            .checkbox(&mut filtered, "Tylko pasujące (Ctrl+H)")
            .changed()
        {
            toggle = true;
        }

        let finished = !job.is_running();
        let mut export = false;
        let mut copy = false;
        ui.horizontal(|ui| {
            let exporting = self.export.is_some();
            export = ui
                .add_enabled(
                    finished && !exporting,
                    egui::Button::new("Eksportuj do pliku…"),
                )
                .on_disabled_hover_text("Dostępne po zakończeniu filtrowania")
                .clicked();
            copy = ui
                .add_enabled(finished, egui::Button::new("Kopiuj wynik"))
                .on_hover_text(format!(
                    "Do schowka, maks. {}",
                    fmt::bytes(copy_limit() as u64)
                ))
                .clicked();
        });
        let mut cancel = false;
        if let Some(e) = &self.export {
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(e.progress())
                        .desired_width(160.0)
                        .show_percentage(),
                );
                cancel = ui.button("Anuluj").clicked();
            });
            ui.weak(format!("Zapisano {}", fmt::bytes(e.bytes_written())));
        }

        if toggle {
            self.toggle_filtered_view();
        }
        if export {
            self.start_export();
        }
        if copy {
            self.copy_result(ui.ctx());
        }
        if cancel {
            self.cancel_export();
            self.set_notice("Eksport anulowany".into());
        }
    }

    pub fn set_notice(&mut self, text: String) {
        self.notice = Some((text, Instant::now()));
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let mut toggle_follow = false;
        let mut new_encoding = None;
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(file) = &self.file {
                    toggle_follow = self.follow_indicator(ui, file);
                    if let Some(sources) = file.merged_sources() {
                        ui.separator();
                        for (i, s) in sources.iter().enumerate() {
                            ui.colored_label(source_color(i), format!("■ {}", s.name))
                                .on_hover_text(s.file.path().display().to_string());
                        }
                    }
                    ui.separator();
                    let name = file
                        .path()
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.label(name)
                        .on_hover_text(file.path().display().to_string());
                    ui.separator();
                    ui.label(fmt::bytes(file.len_bytes()));
                    ui.separator();
                    ui.label(format!("{} linii", fmt::num(file.line_count() as u64)));
                    ui.separator();
                    if file.is_indexing() {
                        ui.add(egui::Spinner::new());
                        ui.add(
                            egui::ProgressBar::new(file.progress())
                                .desired_width(140.0)
                                .show_percentage(),
                        );
                    } else {
                        ui.label(format!(
                            "indeks: {:.2} s, {}",
                            file.index_elapsed().as_secs_f64(),
                            fmt::bytes(file.index_memory() as u64)
                        ));
                    }
                    ui.separator();
                    new_encoding = self.encoding_picker(ui, file);
                    if let Some(profile) = self.profile_label() {
                        ui.separator();
                        ui.label(format!("profil: {profile}"))
                            .on_hover_text("* = ustawienia zmienione od wczytania/zapisu profilu");
                    }
                    if let Some(format) = &self.format {
                        ui.separator();
                        ui.label(&format.name);
                        if self.parse_job.as_ref().is_some_and(ParseJob::is_running) {
                            ui.add(egui::Spinner::new().size(12.0))
                                .on_hover_text("Analiza rekordów (poziomy, czas)");
                        }
                    }
                    ui.separator();
                    if let Some((first, last)) = self.view.visible_lines() {
                        ui.label(format!(
                            "linie {}–{}",
                            fmt::num(first as u64 + 1),
                            fmt::num(last as u64 + 1)
                        ));
                    }
                    if let Some(job) = &self.job {
                        ui.separator();
                        let view = if self.show_filtered {
                            "widok filtra"
                        } else {
                            "cały plik"
                        };
                        ui.label(format!(
                            "{}: {} pasujących",
                            view,
                            fmt::num(job.match_count() as u64)
                        ));
                    }
                    let selected = self.view.selection.count();
                    if selected > 0 {
                        ui.separator();
                        ui.label(format!("zaznaczono {}", fmt::num(selected as u64)));
                    }
                } else {
                    ui.label("Brak otwartego pliku");
                }
                if let Some((text, _)) = &self.notice {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(text);
                    });
                }
            });
        });
        if toggle_follow {
            self.toggle_follow();
        }
        if let Some(encoding) = new_encoding {
            self.set_encoding(encoding);
        }
    }

    /// Live tail button and state in the status bar; returns whether it was
    /// clicked.
    fn follow_indicator(&self, ui: &mut egui::Ui, file: &LogFile) -> bool {
        let green = egui::Color32::from_rgb(0x4C, 0xAF, 0x50);
        let (text, color, hint) = match (self.follow, file.tail_status()) {
            (false, _) => (
                "○ śledzenie",
                ui.visuals().weak_text_color(),
                "Włącz śledzenie końca pliku (Ctrl+L)",
            ),
            (true, TailStatus::Missing) => (
                "● plik usunięty",
                ui.visuals().error_fg_color,
                "Plik zniknął – czekam, aż pojawi się ponownie",
            ),
            (true, _) if !self.view.is_stuck() => (
                "● śledzenie (wstrzymane)",
                ui.visuals().warn_fg_color,
                "Przewinięto w górę – End wraca na koniec",
            ),
            (true, _) => (
                "● śledzenie",
                green,
                "Nowe linie pojawiają się na bieżąco (Ctrl+L wyłącza)",
            ),
        };
        let clicked = ui
            .add(egui::Button::new(egui::RichText::new(text).color(color)).frame(false))
            .on_hover_text(hint)
            .clicked();
        if self.follow && file.appended_lines() > 0 {
            ui.weak(format!("+{} linii", fmt::num(file.appended_lines())))
                .on_hover_text(format!(
                    "Dopisane od włączenia śledzenia; w pamięci {}",
                    fmt::bytes(file.tail_memory())
                ));
        }
        clicked
    }

    /// Record containing `line`, its parsed fields and display text.
    fn build_details(&self, file: &LogFile, line: usize) -> Option<Details> {
        let lines = match self.records() {
            Some(r) if r.multiline() => {
                let rec = r.record(r.record_of_line(line));
                match rec {
                    Some(rec) if rec.lines.contains(&line) => rec.lines,
                    _ => line..line + 1,
                }
            }
            _ => line..line + 1,
        };
        let first = file.line(lines.start)?;
        let format = self.format.as_deref();
        let fields: Vec<(String, String)> = match format.and_then(|f| f.fields(first)) {
            Some(values) => format
                .map(CompiledFormat::columns)
                .unwrap_or_default()
                .iter()
                .zip(values)
                .filter_map(|(name, v)| {
                    v.map(|v| (name.clone(), String::from_utf8_lossy(&v).into_owned()))
                })
                .collect(),
            None => Vec::new(),
        };

        let mut total_bytes = 0;
        let mut text = String::new();
        let mut truncated = false;
        let shown_end = lines.end.min(lines.start + DETAILS_MAX_LINES);
        if lines.end > shown_end {
            truncated = true;
        }
        for (i, bytes) in file.lines(lines.start..shown_end) {
            total_bytes += bytes.len() + 1;
            // Pretty JSON for a JSON record's first line.
            let pretty = (i == lines.start)
                .then(|| format.and_then(|f| f.pretty(bytes)))
                .flatten();
            let piece = pretty.unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
            if text.len() + piece.len() > DETAILS_LIMIT {
                let room = DETAILS_LIMIT.saturating_sub(text.len());
                let cut = (0..=room)
                    .rev()
                    .find(|&c| piece.is_char_boundary(c))
                    .unwrap_or(0);
                text.push_str(&piece[..cut]);
                truncated = true;
                break;
            }
            if i > lines.start {
                text.push('\n');
            }
            text.push_str(&piece);
        }
        Some(Details {
            line,
            format_gen: self.format_gen,
            lines,
            text,
            truncated,
            total_bytes,
            fields,
        })
    }

    fn details_panel(&mut self, ctx: &egui::Context) {
        let (Some(file), Some(line)) = (self.file.clone(), self.view.cursor) else {
            return;
        };
        if self
            .details
            .as_ref()
            .is_none_or(|d| d.line != line || d.format_gen != self.format_gen)
        {
            self.details = self.build_details(&file, line);
        }
        let Some(details) = &self.details else { return };

        let mut copy_full = false;
        egui::TopBottomPanel::bottom("details")
            .resizable(true)
            .default_height(170.0)
            .min_height(60.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Merged view: which file and line the record comes from.
                    if let Some(file) = &self.file
                        && let (Some(sources), Some((src, line))) =
                            (file.merged_sources(), file.origin(details.lines.start))
                    {
                        ui.colored_label(
                            source_color(src),
                            format!(
                                "■ {} : linia {}",
                                sources[src].name,
                                fmt::num(line as u64 + 1)
                            ),
                        );
                        ui.separator();
                    }
                    let lines = &details.lines;
                    if lines.len() > 1 {
                        ui.strong(format!(
                            "Rekord: linie {}–{}",
                            fmt::num(lines.start as u64 + 1),
                            fmt::num(lines.end as u64)
                        ));
                    } else {
                        ui.strong(format!("Linia {}", fmt::num(lines.start as u64 + 1)));
                    }
                    ui.label(format!("{} bajtów", fmt::num(details.total_bytes as u64)));
                    if details.truncated {
                        ui.weak(format!(
                            "(pokazano początek: maks. {} linii / {})",
                            DETAILS_MAX_LINES,
                            fmt::bytes(DETAILS_LIMIT as u64)
                        ));
                    }
                    if ui.button("Kopiuj cały rekord").clicked() {
                        copy_full = true;
                    }
                });
                if !details.fields.is_empty() {
                    egui::CollapsingHeader::new("Pola")
                        .id_salt("details-fields")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new("fields-grid").striped(true).show(ui, |ui| {
                                for (name, value) in &details.fields {
                                    ui.strong(name);
                                    let shown: String = value.chars().take(300).collect();
                                    ui.monospace(shown);
                                    ui.end_row();
                                }
                            });
                        });
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("details-text")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        let mut text: &str = &details.text;
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .font(egui::FontId::monospace(self.prefs.font_size))
                                .desired_width(f32::INFINITY),
                        );
                    });
            });
        if copy_full {
            let lines = details.lines.clone();
            let mut out = String::new();
            let mut bytes = 0;
            for (_, line) in file.lines(lines) {
                if bytes + line.len() > copy_limit() {
                    break;
                }
                bytes += line.len() + 1;
                out.push_str(&String::from_utf8_lossy(line));
                out.push('\n');
            }
            ctx.copy_text(out);
            let text = format!("Skopiowano rekord ({})", fmt::bytes(bytes as u64));
            self.notice = Some((text, Instant::now()));
        }
    }

    // ---- per frame -------------------------------------------------------------

    /// Background work for every tab, also the hidden ones: debounced
    /// settings, format detection, tail checks, export and marker bar.
    pub fn frame(&mut self, ctx: &egui::Context) {
        self.poll_prepare();
        self.check_tail();
        self.maybe_detect_format();
        if self.format_panel.take_due() {
            self.apply_format();
        }
        if self.filter_panel.take_due() {
            self.apply_filters();
        }
        if self.highlight_panel.take_due() {
            self.apply_highlight();
        }
        if self.search.take_due() {
            self.apply_search();
        }
        for wait in [
            self.filter_panel.wait_time(),
            self.highlight_panel.wait_time(),
            self.format_panel.wait_time(),
            self.search.wait_time(),
        ]
        .into_iter()
        .flatten()
        {
            ctx.request_repaint_after(wait);
        }
        if let Some(forward) = self.pending_step {
            self.step_search(forward);
        }
        self.poll_export();
        if let Some((_, at)) = self.notice {
            let left = NOTICE_TIME.saturating_sub(at.elapsed());
            if left.is_zero() {
                self.notice = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
        self.update_rule_hits();
        self.update_minimap();
        self.update_timeline();
    }

    /// Recounts the hits of the highlight rules when the rules changed or
    /// the view moved away from the counted lines.
    fn update_rule_hits(&mut self) {
        use crate::highlight_panel::HIT_SAMPLE_LINES;
        let Some(file) = &self.file else { return };
        let top = self.view.top_line().unwrap_or(0);
        let count = file.line_count();
        let stale = match self.highlight_panel.hit_center {
            None => true,
            Some(center) => {
                center.abs_diff(top) > HIT_SAMPLE_LINES / 2
                    // A short file still being indexed: count again as it grows.
                    || (count < HIT_SAMPLE_LINES && file.is_indexing())
            }
        };
        if !stale {
            return;
        }
        let start = top.saturating_sub(HIT_SAMPLE_LINES / 2);
        let end = (start + HIT_SAMPLE_LINES).min(count);
        let start = end.saturating_sub(HIT_SAMPLE_LINES);
        let lines = file.lines(start..end);
        let sample: Vec<&[u8]> = lines.iter().map(|(_, b)| *b).collect();
        self.highlight_panel.count_hits(&sample);
        self.highlight_panel.hit_center = Some(top);
    }

    /// Shortcuts acting on the visible tab.
    pub fn handle_keys(&mut self, ctx: &egui::Context) {
        let (toggle, find, next, prev, columns, follow) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::H),
                i.consume_key(Modifiers::COMMAND, Key::F),
                i.consume_key(Modifiers::NONE, Key::F3),
                i.consume_key(Modifiers::SHIFT, Key::F3),
                i.consume_key(Modifiers::COMMAND, Key::T),
                i.consume_key(Modifiers::COMMAND, Key::L),
            )
        });
        if toggle {
            self.toggle_filtered_view();
        }
        if find {
            self.search.open_and_focus();
        }
        if next || prev {
            self.step_search(next);
        }
        if columns {
            self.prefs.columns = !self.prefs.columns;
        }
        if follow {
            self.toggle_follow();
        }
        self.handle_extra_keys(ctx);
    }

    /// Draws the visible tab: search bar, status bar, side and details
    /// panels, and the log view.
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        show_details: bool,
        show_filter_panel: bool,
        show_timeline: bool,
    ) {
        self.search_bar(ctx);
        self.status_bar(ctx);
        if show_timeline {
            self.timeline_panel(ctx);
        }
        if show_details {
            self.details_panel(ctx);
        }
        if show_filter_panel {
            self.filter_side_panel(ctx);
        }

        let minimap_ok = self.minimap_matches_view();
        let mut notice = None;
        let mut toggle_bookmark = None;
        let mut preparing_ui = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| match &self.file {
                Some(file) => {
                    let source = match &self.job {
                        Some(job) if self.show_filtered => Source::Filtered { file, job },
                        // Markers come from text rules only (level/time
                        // need records, which the full view does not use).
                        _ => Source::Full {
                            file,
                            highlight: self.compiled.as_deref().filter(|f| f.has_text_rules()),
                        },
                    };
                    let decor = Decor {
                        highlighter: self.highlighter.as_deref(),
                        rules: &self.applied_rules,
                        search: self.search_pattern.as_deref(),
                        current_hit: self.current_hit,
                        minimap: self.minimap.as_ref().filter(|_| minimap_ok),
                        generation: (self.highlight_gen << 32) ^ self.search_gen,
                        bold_family: self.bold_family.as_ref(),
                        format: self.format.as_deref(),
                        follow: file.is_following(),
                        bookmarks: &self.bookmarks,
                    };
                    let output = self.view.show(ui, &source, &decor, &mut self.prefs);
                    notice = output.notice;
                    toggle_bookmark = output.toggle_bookmark;
                }
                None if self.is_preparing() => preparing_ui = true,
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new(
                                "Przeciągnij pliki logów na okno\nalbo Plik → Otwórz… (Ctrl+O)",
                            )
                            .size(18.0)
                            .weak(),
                        );
                    });
                }
            });
        if let Some(text) = notice {
            self.set_notice(text);
        }
        if let Some(line) = toggle_bookmark {
            self.toggle_bookmark(line);
        }
        if preparing_ui {
            egui::Area::new(egui::Id::new(("preparing", self.id)))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| self.preparing_ui(ui));
        }
        self.goto_window(ctx);
        self.ts_picker_window(ctx);
    }

    // ---- profiles ----------------------------------------------------------------

    /// Current settings as a profile (named after the active profile).
    pub fn snapshot_profile(&self) -> Profile {
        let (name, file_patterns) = self
            .active_profile
            .as_ref()
            .map(|p| (p.name.clone(), p.file_patterns.clone()))
            .unwrap_or_default();
        let mut hidden: Vec<String> = self.prefs.hidden_columns.iter().cloned().collect();
        hidden.sort();
        Profile {
            name,
            file_patterns,
            format: self.format_panel.profile_format(),
            view: ViewSettings {
                wrap: self.prefs.wrap,
                font_size: self.prefs.font_size,
                max_display_bytes: self.prefs.max_display_bytes,
                tab_width: self.prefs.tab_width,
                columns: self.prefs.columns,
                hidden_columns: hidden,
                only_matching: self.show_filtered,
            },
            filter: self.filter_panel.saved_spec(),
            highlight: self.highlight_panel.rules.clone(),
        }
    }

    /// Loads the settings of `profile` (applied on the next frame).
    pub fn apply_profile(&mut self, profile: &Profile) {
        self.format_panel.set_profile_format(&profile.format);
        self.filter_panel.set_spec(profile.filter.clone());
        self.highlight_panel.set_rules(profile.highlight.clone());
        let v = &profile.view;
        self.prefs.wrap = v.wrap;
        self.prefs.font_size = v.font_size.clamp(6.0, 48.0);
        self.prefs.max_display_bytes = v.max_display_bytes.clamp(80, 1_000_000);
        self.prefs.tab_width = v.tab_width.clamp(1, 16);
        self.prefs.columns = v.columns;
        self.prefs.hidden_columns = v.hidden_columns.iter().cloned().collect();
        if self.show_filtered != v.only_matching {
            self.toggle_filtered_view();
        }
    }

    /// Applies `profile` and remembers it as this tab's active profile.
    pub fn load_profile(&mut self, profile: Profile) {
        self.apply_profile(&profile);
        self.set_notice(format!("Wczytano profil „{}”", profile.name));
        self.active_profile = Some(profile);
    }

    /// Name of the active profile, with `*` if the settings differ from it.
    pub fn profile_label(&self) -> Option<String> {
        let active = self.active_profile.as_ref()?;
        let modified = !active.same_settings(&self.snapshot_profile());
        Some(format!(
            "{}{}",
            active.name,
            if modified { " *" } else { "" }
        ))
    }

    pub fn set_follow(&mut self, follow: bool) {
        self.follow = follow;
        self.apply_follow();
    }
}
