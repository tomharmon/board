//! Local defaults shared by the CLI, worker, and native inspector.
use std::{
    env,
    path::{Path, PathBuf},
};

impl crate::Board {
    /// The actual main database opened by SQLite, not a guess based on current
    /// environment settings. In-memory and non-UTF-8 filenames have no text path.
    pub fn database_file(&self) -> Option<&Path> {
        self.conn
            .path()
            .map(Path::new)
            .filter(|path| path.is_absolute())
    }
}

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
    if let Some(path) = env::var_os("BOARD_DB")
        .filter(|p| !p.is_empty() && p.to_str().is_none_or(|s| !s.trim().is_empty()))
    {
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
        Some(profile) => format!("{}/{}", person_name(), profile.to_ascii_lowercase()),
        None => person_name(),
    }
}

pub fn profile_name(raw: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        raw.len() <= 32
            && raw.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        crate::errors::Kind::Invalid.error(
            "invalid_profile",
            "profile must start with a letter and contain up to 32 letters, digits, or hyphens"
        )
    );
    Ok(raw.to_ascii_lowercase())
}

/// Decode a structured-field item without treating quoted semicolons as parameters.
pub(crate) fn profile_item(input: &str) -> anyhow::Result<(String, Option<String>)> {
    use anyhow::{Context, ensure};
    use regex::Regex;
    use std::sync::LazyLock;
    const ITEM: &str = r#""(?:[\x20-\x21\x23-\x5b\x5d-\x7e]|\\["\\])*"|[A-Za-z*][!#$%&'*+.^_`|~0-9A-Za-z:/-]*|-?[0-9]{1,12}\.[0-9]{1,3}|-?[0-9]{1,15}|\?[01]|:[a-zA-Z0-9+/=]*:"#;
    static HEAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!("^(?:{ITEM})")).expect("item grammar"));
    static PARAM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!("^; *([a-z*][a-z0-9_.*-]*)(?:=({ITEM}))?")).expect("parameter grammar")
    });
    let invalid = || {
        crate::errors::Kind::Invalid.error("invalid_profile", "invalid structured profile header")
    };
    fn value(s: &str) -> String {
        if !s.starts_with('"') {
            return s.to_owned();
        }
        let mut chars = s[1..s.len() - 1].chars();
        let mut text = String::new();
        while let Some(c) = chars.next() {
            text.push(if c == '\\' {
                chars.next().expect("matched escaped character")
            } else {
                c
            });
        }
        text
    }
    let input = input.trim_matches(' ');
    let head = HEAD.find(input).context(invalid())?;
    let name = profile_name(&value(head.as_str()))?;
    let mut rest = &input[head.end()..];
    let mut avatar = None;
    while !rest.is_empty() {
        let p = PARAM.captures(rest).context(invalid())?;
        if &p[1] == "avatar" {
            let raw = p.get(2).context(invalid())?.as_str();
            ensure!(raw.starts_with('"'), invalid());
            avatar = Some(value(raw));
        }
        rest = &rest[p.get(0).unwrap().end()..];
    }
    if let Some(v) = &avatar {
        crate::actors::valid_avatar(v)?;
    }
    Ok((name, avatar))
}
