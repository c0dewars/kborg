//! `kborg config check`: every user file in the KBorg config folder, checked
//! with the same strict loaders the TUI uses at start and on live reload, so
//! a person (or an assistant writing files for them) learns about a mistake
//! before KBorg keeps the last good settings and shows a notice.
use crate::{Result, safety};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    /// Not there: KBorg uses its built-in defaults.
    Absent,
    Invalid,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    /// Path relative to the config folder.
    pub file: String,
    pub status: Status,
    /// The loader's error for an invalid file, empty otherwise.
    pub detail: String,
}

/// Files in a folder with one extension, sorted; at most 256.
fn listed(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .take(256)
        .collect();
    files.sort();
    files
}

fn finding(file: String, result: Result<()>) -> Finding {
    match result {
        Ok(()) => Finding {
            file,
            status: Status::Ok,
            detail: String::new(),
        },
        Err(error) => Finding {
            file,
            status: Status::Invalid,
            detail: safety::safe_text(&error, 600),
        },
    }
}

/// Checks the files of `dir`, in the order KBorg applies them.
pub fn check_in(dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut single = |name: &str, check: &dyn Fn(&Path) -> Result<()>| {
        let path = dir.join(name);
        findings.push(if path.exists() {
            finding(name.into(), check(&path))
        } else {
            Finding {
                file: name.into(),
                status: Status::Absent,
                detail: String::new(),
            }
        });
    };
    single("ui.yaml", &|path| {
        crate::ui_config::load_from(path).map(drop)
    });
    single("aliases.yaml", &|path| {
        crate::aliases::load_from(path).map(drop)
    });
    single("views.yaml", &|path| {
        crate::views::load_from(path).map(drop)
    });
    // Keys are checked together: a hotkey may not take an action's key.
    let hotkeys = crate::hotkeys::load_from(&dir.join("hotkeys.yaml"));
    let keymap = crate::keymap::load_from(&dir.join("keymap.yaml"));
    single("hotkeys.yaml", &|_| {
        hotkeys.as_ref().map(drop).map_err(Clone::clone)
    });
    single("keymap.yaml", &|_| match (&keymap, &hotkeys) {
        (Err(error), _) => Err(error.clone()),
        (Ok(keymap), Ok(hotkeys)) => keymap.check_hotkeys(hotkeys),
        (Ok(_), Err(_)) => Ok(()),
    });
    for path in listed(&dir.join("themes"), "json") {
        let name = format!(
            "themes/{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
        );
        findings.push(finding(name, crate::themes::load_file(&path).map(drop)));
    }
    let contexts = dir.join(crate::context_config::DIR_NAME);
    for path in listed(&contexts, "yaml") {
        let name = format!(
            "{}/{}",
            crate::context_config::DIR_NAME,
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
        );
        findings.push(finding(
            name,
            crate::context_config::check_file(&path).map(drop),
        ));
    }
    let extensions = dir.join("extensions");
    if extensions.exists()
        && let (Ok(keymap), Ok(hotkeys)) = (&keymap, &hotkeys)
    {
        let catalog = crate::plugins::Catalog::load_in(&extensions, keymap, hotkeys);
        findings.push(finding(
            "extensions/".into(),
            catalog.error.map_or(Ok(()), Err),
        ));
    }
    findings
}

/// The installed config folder and its findings.
pub fn check() -> Result<(PathBuf, Vec<Finding>)> {
    let dir = safety::config_dir()?;
    let findings = check_in(&dir);
    Ok((dir, findings))
}

pub fn text(dir: &Path, findings: &[Finding]) -> String {
    let mut out = format!(
        "KBorg config folder: {}\n",
        safety::safe_text(&dir.display().to_string(), 512)
    );
    for finding in findings {
        out.push_str(&match finding.status {
            Status::Ok => format!("ok       {}\n", finding.file),
            Status::Absent => format!("absent   {} (built-in defaults)\n", finding.file),
            Status::Invalid => format!("INVALID  {}: {}\n", finding.file, finding.detail),
        });
    }
    let invalid = findings
        .iter()
        .filter(|finding| finding.status == Status::Invalid)
        .count();
    out.push_str(&if invalid == 0 {
        "All files are valid. A running KBorg applies a saved change within a second.".into()
    } else {
        format!(
            "{invalid} invalid file{}. A running KBorg keeps the last good settings until it is fixed.",
            if invalid == 1 { "" } else { "s" }
        )
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> Result<PathBuf> {
        let dir =
            std::env::temp_dir().join(format!("kborg-config-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|_| "fixture")?;
        dir.canonicalize().map_err(|_| "fixture".into())
    }
    fn status<'a>(findings: &'a [Finding], file: &str) -> Option<&'a Finding> {
        findings.iter().find(|finding| finding.file == file)
    }

    #[test]
    fn an_empty_folder_is_all_defaults() -> Result<()> {
        let dir = dir("empty")?;
        let findings = check_in(&dir);
        assert!(findings.iter().all(|f| f.status == Status::Absent));
        assert!(text(&dir, &findings).ends_with(
            "All files are valid. A running KBorg applies a saved change within a second."
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn each_file_is_checked_by_its_own_loader_and_errors_name_the_problem() -> Result<()> {
        let dir = dir("mixed")?;
        let write = |name: &str, text: &str| -> Result<()> {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|_| "fixture")?;
            }
            std::fs::write(path, text).map_err(|_| "fixture".into())
        };
        write("keymap.yaml", "keys:\n  logs: L\n")?;
        write(
            "views.yaml",
            "views:\n  v1/pods:\n    colums: [NAME, AGE]\n",
        )?;
        write("aliases.yaml", "aliases:\n  pp: v1/pods\n")?;
        write(
            "contexts/prod.yaml",
            "version: 1\nread_only: true\nlabel: PROD\n",
        )?;
        write("contexts/bad.yaml", "version: 1\nreadonly: true\n")?;
        let findings = check_in(&dir);
        assert_eq!(
            status(&findings, "keymap.yaml").map(|f| f.status),
            Some(Status::Ok)
        );
        assert_eq!(
            status(&findings, "aliases.yaml").map(|f| f.status),
            Some(Status::Ok)
        );
        assert_eq!(
            status(&findings, "contexts/prod.yaml").map(|f| f.status),
            Some(Status::Ok)
        );
        assert_eq!(
            status(&findings, "ui.yaml").map(|f| f.status),
            Some(Status::Absent)
        );
        for file in ["views.yaml", "contexts/bad.yaml"] {
            let found = status(&findings, file).ok_or("missing finding")?;
            assert_eq!(found.status, Status::Invalid, "{file}");
            assert!(!found.detail.is_empty(), "{file}");
        }
        // A hotkey may not take a key the keymap gave an action.
        write("keymap.yaml", "keys:\n  logs: F5\n")?;
        write(
            "hotkeys.yaml",
            "hotKeys:\n  f5:\n    shortCut: F5\n    command: no\n",
        )?;
        let findings = check_in(&dir);
        assert_eq!(
            status(&findings, "hotkeys.yaml").map(|f| f.status),
            Some(Status::Ok)
        );
        assert_eq!(
            status(&findings, "keymap.yaml").map(|f| f.status),
            Some(Status::Invalid)
        );
        assert!(text(&dir, &findings).contains("invalid files."));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
