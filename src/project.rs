//! Strict project definitions, validated before creating a session endpoint.
use serde::Deserialize;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = 64 * 1024;
pub(crate) const MAX_COMMAND: usize = 4096;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Project {
    pub windows: Vec<Window>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Window {
    pub name: String,
    pub panes: Vec<Pane>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pane {
    #[serde(default = "default_directory")]
    pub cwd: PathBuf,
    pub command: Option<String>,
    #[serde(default)]
    pub split: Split,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Split {
    #[default]
    Right,
    Down,
}
fn default_directory() -> PathBuf {
    PathBuf::from(".")
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub(crate) fn load(
    path: &Path,
    rows: u16,
    columns: u16,
) -> io::Result<crate::persistence::Snapshot> {
    let path = fs::canonicalize(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("project layout must be a regular file"));
    }
    let mut text = String::new();
    file.take(MAX_FILE + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_FILE {
        return Err(invalid("project layout exceeds 64 KiB"));
    }
    crate::persistence::Snapshot::from_project(parse(&text, path.parent().unwrap())?, rows, columns)
}

fn parse(text: &str, base: &Path) -> io::Result<Project> {
    let mut project: Project = toml::from_str(text).map_err(io::Error::other)?;
    if project.windows.is_empty() || project.windows.len() > crate::terminal::MAX_WINDOWS {
        return Err(invalid("project requires 1–16 windows"));
    }
    if project.windows.iter().map(|w| w.panes.len()).sum::<usize>() > 128 {
        return Err(invalid("project exceeds 128 panes"));
    }
    for window in &mut project.windows {
        if window.name.is_empty()
            || window.name.len() > 128
            || window.name.chars().any(char::is_control)
            || window.panes.is_empty()
        {
            return Err(invalid(
                "project windows require a valid name and at least one pane",
            ));
        }
        for pane in &mut window.panes {
            pane.cwd = fs::canonicalize(base.join(&pane.cwd)).map_err(|error| {
                invalid(format!(
                    "layout directory '{}': {error}",
                    pane.cwd.display()
                ))
            })?;
            if !pane.cwd.is_dir() || pane.cwd.to_str().is_none() {
                return Err(invalid(
                    "project directories must be existing UTF-8 directories",
                ));
            }
            validate_command(pane.command.as_deref())?;
        }
    }
    Ok(project)
}

pub(crate) fn validate_command(command: Option<&str>) -> io::Result<()> {
    if command.is_some_and(|s| s.trim().is_empty() || s.contains('\0') || s.len() > MAX_COMMAND) {
        return Err(invalid("startup commands require 1–4096 bytes without NUL"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_directories_and_rejects_invalid_projects_before_launch() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        let project=parse("[[windows]]\nname='dev'\n[[windows.panes]]\ncwd='src'\ncommand='echo ready'\n[[windows.panes]]\nsplit='down'",root.path()).unwrap();
        assert_eq!(
            project.windows[0].panes[0].cwd,
            fs::canonicalize(root.path().join("src")).unwrap()
        );
        for text in [
            "windows=[]",
            "[[windows]]\nname='dev'\npanes=[]",
            "[[windows]]\nname='dev'\n[[windows.panes]]\ncwd='missing'",
            "[[windows]]\nname='dev'\n[[windows.panes]]\nsplit='diagonal'",
            "[[windows]]\nname='dev'\n[[windows.panes]]\ncommand=''",
            "[[windows]]\nname='dev'\n[[windows.panes]]\nunknown=1",
        ] {
            assert!(parse(text, root.path()).is_err(), "{text}");
        }
    }
    #[test]
    fn geometry_and_file_bounds_are_checked_before_process_creation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("project.toml");
        fs::write(
            &path,
            "[[windows]]\nname='dev'\n[[windows.panes]]\n[[windows.panes]]",
        )
        .unwrap();
        assert!(load(&path, 24, 4).is_err());
        assert!(load(&path, 40, 120).is_ok());
        fs::write(&path, " ".repeat(MAX_FILE as usize + 1)).unwrap();
        assert!(load(&path, 40, 120).is_err());
        assert!(validate_command(Some(&"x".repeat(MAX_COMMAND + 1))).is_err());
        assert!(validate_command(Some("echo\0bad")).is_err());
    }
}
