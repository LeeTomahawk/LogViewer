//! Core of the log viewer. No GUI dependencies.
//!
//! * [`source::MappedFile`] – read-only memory mapping of a log file.
//! * [`index::LineIndex`] – compact index of line start offsets (~4 B per line).
//! * [`file::LogFile`] – a mapped file plus an index built on a background thread.
//! * [`text::display_line`] – turns raw line bytes into displayable text.
//! * [`filter`] – filter rules (include/exclude, text/regex) and matching.
//! * [`filter_job::FilterJob`] – parallel background filtering with context lines.
//! * [`export::ExportJob`] – writes a filter result to a file in the background.
//! * [`pattern::Pattern`] – compiled text/regex pattern shared by all of the above.
//! * [`highlight::Highlighter`] – color rules for fragments and whole lines.
//! * [`minimap::MinimapJob`] – scrollbar markers computed in the background.

pub mod compact;
pub mod encoding;
pub mod export;
pub mod file;
pub mod filter;
pub mod filter_job;
pub mod format;
pub mod highlight;
pub mod index;
pub mod level;
pub mod merge;
pub mod minimap;
pub mod pattern;
pub mod prepare;
pub mod profile;
pub mod records;
pub mod source;
pub mod tail;
pub mod text;
pub mod time;
pub mod timeline;
pub mod ts_locate;

pub use encoding::{Codec, TextEncoding};
pub use export::{ExportJob, rows_to_string};
pub use file::{LogFile, Notify, OpenError};
pub use filter::{CompiledFilter, FilterRule, FilterSpec, IncludeMode, PatternKind, RuleError};
pub use filter_job::{FilterJob, FilterRows, RowEntry};
pub use format::{CompiledFormat, Detection, FormatKind, LineMeta, LogFormat, detect};
pub use highlight::{HighlightRule, Highlighter, LineHighlight, Rgba, Scope, Style, StyleId};
pub use index::LineIndex;
pub use level::{Level, LevelSet};
pub use merge::{MergeSource, merge_files};
pub use minimap::{MinimapJob, SEARCH_BIT};
pub use pattern::Pattern;
pub use prepare::{Plan, PrepareJob, Prepared};
pub use profile::{FormatMode, Profile, ProfileFormat, ProfileStore, StoredProfile, ViewSettings};
pub use records::{ParseJob, Records};
pub use tail::TailStatus;
pub use text::{DisplayBuilder, DisplayLine, display_cut, display_line};
pub use timeline::{Timeline, TimelineJob};
