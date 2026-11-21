use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Window chrome only. All board content and drafts still live in SQLite.
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub sidebar: f32,
    pub thread: f32,
    pub collapsed: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar: 224.0,
            thread: 440.0,
            collapsed: false,
        }
    }
}

impl Layout {
    fn path() -> PathBuf {
        let mut name = board_local::local::database_path().into_os_string();
        name.push(".ui.json");
        PathBuf::from(name)
    }

    pub fn load() -> Self {
        let path = Self::path();
        let mut layout: Self = fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.is_file() && m.len() <= 1024)
            .and_then(|_| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        layout.sidebar = layout.sidebar.clamp(180.0, 400.0);
        layout.thread = layout.thread.clamp(320.0, 4096.0);
        layout
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::path();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(file.as_file_mut(), self)?;
        file.persist(path)?;
        Ok(())
    }
}
