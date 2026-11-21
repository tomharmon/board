//! Local defaults shared by the CLI, worker, and native inspector.
use std::{env, path::PathBuf};

fn var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|s| !s.trim().is_empty())
}

fn board_dir() -> PathBuf {
    let base = var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("board")
}
pub fn named_database_path(name: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let n = name.trim().to_lowercase();
    let simple = !n.is_empty()
        && n.len() <= 64
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let stem = if simple {
        n
    } else {
        let readable = n
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let slug = readable.chars().take(48).collect::<String>();
        let digest = format!("{:x}", Sha256::digest(n.as_bytes()));
        format!(
            "{}.{}",
            if slug.is_empty() { "board" } else { &slug },
            &digest[..32]
        )
    };
    board_dir().join("boards").join(format!("{stem}.sqlite3"))
}
pub fn database_path() -> PathBuf {
    if let Some(path) = var("BOARD_DB") {
        return path.into();
    }
    if let Some(name) = var("BOARD_NAME") {
        return named_database_path(&name);
    }
    board_dir().join("board.sqlite3")
}

pub fn person_name() -> String {
    var("BOARD_USER")
        .or_else(|| var("USER"))
        .or_else(|| var("USERNAME"))
        .unwrap_or_else(|| "local".to_string())
}

pub fn actor_name() -> String {
    match var("BOARD_PROFILE") {
        Some(profile) => format!("{}/{}", person_name(), profile),
        None => person_name(),
    }
}
