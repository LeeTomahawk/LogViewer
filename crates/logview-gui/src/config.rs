//! Configuration folder: profiles and the saved session.
//!
//! On Windows the folder is `%APPDATA%\logview\config`, on Linux
//! `~/.config/logview`, on macOS `~/Library/Application Support/logview`.

use std::path::{Path, PathBuf};

use logview_core::profile::write_atomic;
use logview_core::{Profile, ProfileStore};
use serde::{Deserialize, Serialize};

/// Recent files kept in the session.
pub const MAX_RECENT: usize = 12;

/// Sample profiles copied into a new profile folder.
const EXAMPLES: &[(&str, &str)] = &[
    ("Ogólny.toml", include_str!("../../../profiles/ogolny.toml")),
    ("nginx.toml", include_str!("../../../profiles/nginx.toml")),
    (
        "JSON lines.toml",
        include_str!("../../../profiles/jsonl.toml"),
    ),
    ("Java.toml", include_str!("../../../profiles/java.toml")),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// One open tab in the saved session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionTab {
    pub path: PathBuf,
    /// Top line of the view.
    pub line: Option<usize>,
    pub follow: bool,
    /// Name of the tab's active profile.
    pub active_profile: Option<String>,
    /// The tab's settings.
    pub settings: Profile,
    pub bookmarks: Vec<SavedBookmark>,
    /// Encoding chosen for the file.
    pub encoding: logview_core::TextEncoding,
}

/// A bookmark in the saved session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedBookmark {
    pub line: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl Default for SessionTab {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            line: None,
            follow: false,
            active_profile: None,
            settings: Profile::default(),
            bookmarks: Vec::new(),
            encoding: logview_core::TextEncoding::Auto,
        }
    }
}

/// State restored on the next start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub recent_files: Vec<PathBuf>,
    pub theme: Theme,
    pub show_filter_panel: bool,
    pub show_details: bool,
    pub show_timeline: bool,
    /// Settings of the active tab; used for tabs opened later.
    pub current: Profile,
    /// Name of the active tab's profile.
    pub active_profile: Option<String>,
    /// Open files (merged views are not restored).
    pub tabs: Vec<SessionTab>,
    pub active_tab: usize,

    // Single-file sessions from before tabs (read only).
    #[serde(skip_serializing)]
    pub last_file: Option<PathBuf>,
    #[serde(skip_serializing)]
    pub last_line: Option<usize>,
    #[serde(skip_serializing)]
    pub follow: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            recent_files: Vec::new(),
            theme: Theme::System,
            show_filter_panel: true,
            show_details: true,
            show_timeline: true,
            current: Profile::default(),
            active_profile: None,
            tabs: Vec::new(),
            active_tab: 0,
            last_file: None,
            last_line: None,
            follow: false,
        }
    }
}

impl Session {
    /// Tabs to restore, also from an old single-file session.
    pub fn tabs_to_restore(&self) -> Vec<SessionTab> {
        if !self.tabs.is_empty() {
            return self.tabs.clone();
        }
        self.last_file
            .iter()
            .map(|path| SessionTab {
                path: path.clone(),
                line: self.last_line,
                follow: self.follow,
                active_profile: self.active_profile.clone(),
                settings: self.current.clone(),
                bookmarks: Vec::new(),
                encoding: logview_core::TextEncoding::Auto,
            })
            .collect()
    }

    /// Moves `path` to the front of the recent files.
    pub fn add_recent(recent: &mut Vec<PathBuf>, path: &Path) {
        recent.retain(|p| p != path);
        recent.insert(0, path.to_path_buf());
        recent.truncate(MAX_RECENT);
    }
}

pub struct Config {
    dir: PathBuf,
    pub store: ProfileStore,
}

impl Config {
    /// The per-user configuration folder, if the OS provides one.
    pub fn locate() -> Option<Self> {
        let dirs = directories::ProjectDirs::from("", "", "logview")?;
        Some(Self::at(dirs.config_dir()))
    }

    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            store: ProfileStore::new(dir.join("profiles")),
        }
    }

    fn session_path(&self) -> PathBuf {
        self.dir.join("session.toml")
    }

    /// Creates the profile folder with the sample profiles on first run.
    pub fn seed_examples(&self) -> Result<(), String> {
        let dir = self.store.dir();
        if dir.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (name, text) in EXAMPLES {
            write_atomic(&dir.join(name), text).map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(())
    }

    /// The saved session, or defaults if there is none or it cannot be read.
    pub fn load_session(&self) -> (Session, Option<String>) {
        let path = self.session_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(session) => (session, None),
                Err(e) => (
                    Session::default(),
                    Some(format!("Nie można odczytać sesji {}: {e}", path.display())),
                ),
            },
            Err(_) => (Session::default(), None),
        }
    }

    pub fn save_session(&self, session: &Session) -> Result<(), String> {
        let text = toml::to_string(session).map_err(|e| e.to_string())?;
        let path = self.session_path();
        write_atomic(
            &path,
            &format!("# Sesja logview (zapisywana automatycznie)\n\n{text}"),
        )
        .map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trip_and_seed() {
        let dir = std::env::temp_dir().join(format!("logview-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = Config::at(&dir);
        config.seed_examples().unwrap();
        let names: Vec<String> = config
            .store
            .list()
            .into_iter()
            .map(|s| s.profile.unwrap().name)
            .collect();
        assert_eq!(names, ["Java", "JSON lines", "nginx", "Ogólny"]);
        assert!(config.store.find_for_file("access.log").is_some());

        let (empty, error) = config.load_session();
        assert_eq!(empty, Session::default());
        assert!(error.is_none());

        let mut session = Session {
            tabs: vec![
                SessionTab {
                    path: dir.join("a.log"),
                    line: Some(42),
                    follow: true,
                    bookmarks: vec![SavedBookmark {
                        line: 5,
                        note: "tu się zaczyna".into(),
                    }],
                    ..Default::default()
                },
                SessionTab {
                    path: dir.join("b.log"),
                    ..Default::default()
                },
            ],
            active_tab: 1,
            theme: Theme::Dark,
            ..Default::default()
        };
        Session::add_recent(&mut session.recent_files, Path::new("x.log"));
        Session::add_recent(&mut session.recent_files, Path::new("y.log"));
        Session::add_recent(&mut session.recent_files, Path::new("x.log"));
        assert_eq!(
            session.recent_files,
            [PathBuf::from("x.log"), PathBuf::from("y.log")]
        );
        config.save_session(&session).unwrap();
        assert_eq!(config.load_session().0, session);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_single_file_session_becomes_a_tab() {
        let old: Session = toml::from_str(
            "last_file = 'C:/logs/app.log'\nlast_line = 7\nfollow = true\n[current]\nname = 'x'\n",
        )
        .unwrap();
        let tabs = old.tabs_to_restore();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].path, PathBuf::from("C:/logs/app.log"));
        assert_eq!(tabs[0].line, Some(7));
        assert!(tabs[0].follow);
        assert_eq!(tabs[0].settings.name, "x");
    }
}
