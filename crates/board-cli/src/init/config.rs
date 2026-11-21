use super::{Harness, Saved};
use crate::skill_install::{claude_dir, codex_dir};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
};
use toml_edit::Document;

fn read(path: &Path) -> Result<Option<String>> {
    ensure!(
        path.is_absolute(),
        "harness configuration directory must be absolute"
    );
    match fs::read_to_string(path) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}
fn command_matches(cmd: Option<&str>, exe: &Path) -> bool {
    cmd.map(Path::new) == Some(exe)
}
fn codex(path: &Path, exe: &Path) -> Result<Saved> {
    let Some(text) = read(path)? else {
        return Ok(Saved::Missing);
    };
    let doc =
        Document::parse(text).map_err(|_| anyhow::anyhow!("invalid TOML in {}", path.display()))?;
    let Some(servers) = doc.get("mcp_servers") else {
        return Ok(Saved::Missing);
    };
    let table = servers
        .as_table_like()
        .context("mcp_servers is not a table")?;
    let Some(server) = table.get("board") else {
        return Ok(Saved::Missing);
    };
    let Some(s) = server.as_table_like() else {
        return Ok(Saved::Conflict);
    };
    let matches = s
        .iter()
        .all(|(k, _)| matches!(k, "command" | "args" | "enabled" | "env" | "env_vars"))
        && command_matches(s.get("command").and_then(|v| v.as_str()), exe)
        && s.get("args")
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.len() == 1 && a.get(0).and_then(|v| v.as_str()) == Some("mcp"))
        && s.get("enabled").is_none_or(|v| v.as_bool() == Some(true))
        && s.get("env")
            .is_none_or(|v| v.as_table_like().is_some_and(|t| t.is_empty()))
        && s.get("env_vars")
            .is_none_or(|v| v.as_array().is_some_and(|a| a.is_empty()));
    Ok(if matches {
        Saved::Same
    } else {
        Saved::Conflict
    })
}
fn claude(path: &Path, exe: &Path) -> Result<Saved> {
    let Some(text) = read(path)? else {
        return Ok(Saved::Missing);
    };
    let doc: Value = serde_json::from_str(&text)
        .map_err(|_| anyhow::anyhow!("invalid JSON in {}", path.display()))?;
    let Some(servers) = doc
        .as_object()
        .context("Claude configuration is not an object")?
        .get("mcpServers")
    else {
        return Ok(Saved::Missing);
    };
    let Some(s) = servers
        .as_object()
        .context("mcpServers is not an object")?
        .get("board")
    else {
        return Ok(Saved::Missing);
    };
    let matches = s.as_object().is_some_and(|s| {
        s.keys()
            .all(|k| matches!(k.as_str(), "command" | "args" | "type" | "env"))
    }) && command_matches(s["command"].as_str(), exe)
        && s["args"] == serde_json::json!(["mcp"])
        && s.get("type").is_none_or(|t| t == "stdio")
        && s.get("url").is_none()
        && s.get("env")
            .is_none_or(|v| v.as_object().is_some_and(|t| t.is_empty()));
    Ok(if matches {
        Saved::Same
    } else {
        Saved::Conflict
    })
}
pub(super) fn registration(h: Harness, home: &Path, exe: &Path) -> Result<Saved> {
    match h {
        Harness::Codex => codex(&codex_dir(home).join("config.toml"), exe),
        Harness::Claude => {
            // Without an override, this file is beside .claude/, not inside it.
            let base = if env::var_os("CLAUDE_CONFIG_DIR").is_some() {
                claude_dir(home)
            } else {
                home.to_owned()
            };
            claude(&base.join(".claude.json"), exe)
        }
    }
}

pub(super) fn executable(path: &Path) -> bool {
    let Ok(m) = fs::metadata(path) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
pub(super) fn on_path(name: &str) -> impl Iterator<Item = PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path).filter(|p| p.is_absolute()) {
            #[cfg(windows)]
            let suffixes = [".exe", ".cmd", ".bat"];
            #[cfg(not(windows))]
            let suffixes = [""];
            paths.extend(
                suffixes
                    .iter()
                    .map(|ext| dir.join(format!("{name}{ext}")))
                    .filter(|p| executable(p)),
            );
        }
    }
    paths.into_iter()
}
pub(super) fn installed_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        ensure!(
            p.is_absolute() && executable(&p),
            "--board-bin must be an absolute executable path"
        );
        ensure!(
            p.to_str().is_some(),
            "the harness requires a UTF-8 executable path"
        );
        return Ok(p);
    }
    let own = env::current_exe()?.canonicalize()?;
    let by_name = on_path("board").find(|p| p.canonicalize().is_ok_and(|v| v == own));
    // Keep the package manager's stable symlink, not a versioned Cellar/Caskroom target.
    let path = by_name.unwrap_or(env::current_exe()?);
    ensure!(
        path.to_str().is_some(),
        "the harness requires a UTF-8 executable path"
    );
    ensure!(
        !path.components().any(|c| c.as_os_str() == "target")
            || !path
                .components()
                .any(|c| c.as_os_str() == "debug" || c.as_os_str() == "release"),
        "this Board binary is in a Cargo build directory; install Board first, or explicitly select an absolute --board-bin PATH"
    );
    Ok(path)
}
