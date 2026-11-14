//! Profiles: named sets of settings saved as TOML.
//!
//! A [`Profile`] holds the line format, highlight rules, filters and view
//! settings. Profiles live as `*.toml` files in a directory managed by
//! [`ProfileStore`]. A profile with [`Profile::file_patterns`] is picked
//! automatically for files whose name matches one of the globs.
//!
//! The time range of the filter is not saved (it belongs to one file).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::filter::FilterSpec;
use crate::format::{FormatKind, LogFormat};
use crate::highlight::HighlightRule;

/// Extension of profile files.
pub const PROFILE_EXT: &str = "toml";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatMode {
    /// Detect from the file.
    #[default]
    Auto,
    /// A built-in format, by name.
    Builtin,
    /// A custom regex.
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileFormat {
    pub mode: FormatMode,
    /// Name of the built-in format (`mode = "builtin"`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Regex with named groups (`mode = "custom"`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub regex: String,
    /// chrono format of the `ts` field; empty = automatic.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub timestamp_format: String,
    /// Join continuation lines with their record.
    pub multiline: bool,
}

impl Default for ProfileFormat {
    fn default() -> Self {
        Self {
            mode: FormatMode::Auto,
            name: String::new(),
            regex: String::new(),
            timestamp_format: String::new(),
            multiline: true,
        }
    }
}

impl ProfileFormat {
    /// The format to use; `None` means automatic detection. An unknown
    /// built-in name also falls back to detection.
    pub fn resolve(&self) -> Option<LogFormat> {
        match self.mode {
            FormatMode::Auto => None,
            FormatMode::Builtin => LogFormat::builtins()
                .into_iter()
                .find(|f| f.name == self.name),
            FormatMode::Custom => Some(LogFormat {
                name: "Własny".into(),
                kind: FormatKind::Regex(self.regex.clone()),
                timestamp_format: self.timestamp_format.clone(),
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewSettings {
    pub wrap: bool,
    pub font_size: f32,
    pub max_display_bytes: usize,
    pub tab_width: usize,
    /// Column view.
    pub columns: bool,
    pub hidden_columns: Vec<String>,
    /// Show only filter results (otherwise the whole file with markers).
    pub only_matching: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            wrap: false,
            font_size: 13.0,
            max_display_bytes: 4096,
            tab_width: 4,
            columns: false,
            hidden_columns: Vec::new(),
            only_matching: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// File name globs (`*`, `?`, case-insensitive) choosing this profile
    /// automatically, e.g. `"*nginx*.log"`.
    pub file_patterns: Vec<String>,
    pub format: ProfileFormat,
    pub view: ViewSettings,
    pub filter: FilterSpec,
    pub highlight: Vec<HighlightRule>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: String::new(),
            file_patterns: Vec::new(),
            format: ProfileFormat::default(),
            view: ViewSettings::default(),
            filter: FilterSpec::default(),
            highlight: HighlightRule::defaults(),
        }
    }
}

impl Profile {
    pub fn to_toml(&self) -> Result<String, String> {
        // Not `to_string_pretty`: it splits even short arrays over lines.
        toml::to_string(self)
            .map(|body| format!("# Profil logview: {}\n\n{body}", self.name))
            .map_err(|e| e.to_string())
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut profile = Self::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if profile.name.trim().is_empty() {
            profile.name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "profil".into());
        }
        Ok(profile)
    }

    /// Whether `file_name` matches one of the profile's patterns.
    pub fn matches_file(&self, file_name: &str) -> bool {
        self.file_patterns
            .iter()
            .any(|p| !p.trim().is_empty() && glob_match(p.trim(), file_name))
    }

    /// Whether the settings (everything but name and patterns) are equal.
    pub fn same_settings(&self, other: &Profile) -> bool {
        self.format == other.format
            && self.view == other.view
            && self.filter == other.filter
            && self.highlight == other.highlight
    }
}

/// Case-insensitive glob match supporting `*` and `?`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // Iterative matching with backtracking to the last `*`.
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// File name (without extension) for a profile name: letters, digits,
/// `-`, `_` and spaces are kept, other characters become `_`.
pub fn file_stem_for(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        "profil".into()
    } else {
        stem
    }
}

/// Writes `contents` to `path` via a temporary file and rename, so a crash
/// never leaves a half-written file.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Profiles stored as TOML files in one directory.
#[derive(Clone, Debug)]
pub struct ProfileStore {
    dir: PathBuf,
}

/// A profile file found in the store.
#[derive(Clone, Debug)]
pub struct StoredProfile {
    pub path: PathBuf,
    /// The profile, or why it could not be read.
    pub profile: Result<Profile, String>,
}

impl ProfileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path_for(&self, name: &str) -> PathBuf {
        self.dir
            .join(format!("{}.{PROFILE_EXT}", file_stem_for(name)))
    }

    /// All profile files, sorted by profile name.
    pub fn list(&self) -> Vec<StoredProfile> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut found: Vec<StoredProfile> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(PROFILE_EXT))
            })
            .map(|path| StoredProfile {
                profile: Profile::load(&path),
                path,
            })
            .collect();
        // Readable profiles by name, then unreadable files by path.
        found.sort_by_key(|s| match &s.profile {
            Ok(p) => (false, p.name.to_lowercase()),
            Err(_) => (true, s.path.display().to_string()),
        });
        found
    }

    /// Saves under the file name derived from the profile name (replacing a
    /// profile with the same name).
    pub fn save(&self, profile: &Profile) -> Result<PathBuf, String> {
        let path = self.path_for(&profile.name);
        let text = profile.to_toml()?;
        write_atomic(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }

    pub fn delete(&self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The first profile whose patterns match `file_name`.
    pub fn find_for_file(&self, file_name: &str) -> Option<Profile> {
        self.list()
            .into_iter()
            .filter_map(|s| s.profile.ok())
            .find(|p| p.matches_file(file_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{FilterRule, IncludeMode};
    use crate::level::{Level, LevelSet};

    fn sample() -> Profile {
        let mut levels = LevelSet(0);
        levels.set(Some(Level::Error), true);
        levels.set(None, true);
        Profile {
            name: "Test ą".into(),
            file_patterns: vec!["*app*.log".into()],
            format: ProfileFormat {
                mode: FormatMode::Custom,
                regex: r"^(?P<ts>\S+) (?P<msg>.*)$".into(),
                timestamp_format: "%d.%m.%Y".into(),
                multiline: false,
                ..Default::default()
            },
            view: ViewSettings {
                columns: true,
                hidden_columns: vec!["thread".into()],
                ..Default::default()
            },
            filter: FilterSpec {
                rules: vec![
                    FilterRule::include("err").regex(),
                    FilterRule::exclude("ping"),
                ],
                mode: IncludeMode::Any,
                context_before: 2,
                levels,
                time_from: Some(5),
                ..Default::default()
            },
            highlight: HighlightRule::defaults(),
        }
    }

    #[test]
    fn toml_round_trip() {
        let profile = sample();
        let text = profile.to_toml().unwrap();
        assert!(text.contains("#F44336"), "colors as hex:\n{text}");
        assert!(text.contains(r#"levels = ["ERROR", "none"]"#), "{text}");
        let back = Profile::from_toml(&text).unwrap();
        // The time range is not saved.
        let mut expected = profile;
        expected.filter.time_from = None;
        assert_eq!(back, expected);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let p = Profile::from_toml("name = \"mini\"").unwrap();
        assert_eq!(p.name, "mini");
        assert_eq!(p.format.mode, FormatMode::Auto);
        assert!(p.format.multiline);
        assert!(p.filter.levels.is_all());
        assert_eq!(p.highlight, HighlightRule::defaults());
        assert!(p.view.only_matching);
    }

    #[test]
    fn bad_values_are_reported() {
        let err = Profile::from_toml("[[highlight]]\npattern = \"x\"\nstyle = { fg = \"red\" }")
            .unwrap_err();
        assert!(err.contains("kolor"), "{err}");
        let err = Profile::from_toml("[filter]\nlevels = [\"LOUD\"]").unwrap_err();
        assert!(err.contains("poziom"), "{err}");
    }

    #[test]
    fn example_profiles_parse() {
        for (name, text) in [
            ("ogolny", include_str!("../../../profiles/ogolny.toml")),
            ("nginx", include_str!("../../../profiles/nginx.toml")),
            ("jsonl", include_str!("../../../profiles/jsonl.toml")),
            ("java", include_str!("../../../profiles/java.toml")),
        ] {
            let p = Profile::from_toml(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            crate::highlight::Highlighter::compile(&p.highlight)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            p.filter
                .compile()
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            if p.format.mode != FormatMode::Auto {
                let format = p
                    .format
                    .resolve()
                    .unwrap_or_else(|| panic!("{name}: format"));
                format
                    .compile(&[])
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
            }
        }
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.log", "app.LOG"));
        assert!(glob_match("*nginx*", "access-nginx-2026.log"));
        assert!(glob_match("app-??.log", "app-01.log"));
        assert!(!glob_match("app-??.log", "app-1.log"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
        assert!(glob_match("*.jsonl", "dziennik-żółw.jsonl"));
    }

    #[test]
    fn matches_file_ignores_empty_patterns() {
        let p = Profile {
            file_patterns: vec!["".into(), " *.err ".into()],
            ..Default::default()
        };
        assert!(p.matches_file("x.err"));
        assert!(!p.matches_file("x.log"));
    }

    #[test]
    fn store_save_list_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(dir.path());
        let a = Profile {
            name: "B/zły:znak".into(),
            file_patterns: vec!["*.b".into()],
            ..Default::default()
        };
        let b = Profile {
            name: "a".into(),
            ..Default::default()
        };
        let path_a = store.save(&a).unwrap();
        store.save(&b).unwrap();
        assert_eq!(path_a.file_name().unwrap(), "B_zły_znak.toml");
        std::fs::write(dir.path().join("broken.toml"), "name = [").unwrap();
        let listed = store.list();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].profile.as_ref().unwrap().name, "a");
        assert!(listed.iter().any(|s| s.profile.is_err()));
        assert_eq!(store.find_for_file("x.B").unwrap().name, a.name);
        assert!(store.find_for_file("x.c").is_none());
        store.delete(&path_a).unwrap();
        assert_eq!(store.list().len(), 2);
    }

    #[test]
    fn file_stems() {
        assert_eq!(file_stem_for("  nginx prod "), "nginx prod");
        assert_eq!(file_stem_for("a/b\\c"), "a_b_c");
        assert_eq!(file_stem_for(""), "profil");
    }
}
