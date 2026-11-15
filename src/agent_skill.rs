//! The KBorg skill for AI assistants: an Agent Skills folder (SKILL.md plus
//! references) built into the binary, so `kborg skill install` writes the
//! version that matches the installed KBorg. Claude Code, GitHub Copilot,
//! OpenAI Codex and Gemini CLI read such folders; chat apps get the same text
//! as one Markdown file from `kborg skill print`. The files live in
//! `skills/kborg/` in the repository; tests parse every example they mark and
//! check that the commands they name exist.
use crate::{Result, safety};
use std::path::{Path, PathBuf};

/// `(relative path, content)` of every file of the skill, SKILL.md first.
pub const FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../skills/kborg/SKILL.md")),
    (
        "references/investigate-a-cluster.md",
        include_str!("../skills/kborg/references/investigate-a-cluster.md"),
    ),
    (
        "references/keys.md",
        include_str!("../skills/kborg/references/keys.md"),
    ),
    (
        "references/themes-and-looks.md",
        include_str!("../skills/kborg/references/themes-and-looks.md"),
    ),
    (
        "references/tables-and-views.md",
        include_str!("../skills/kborg/references/tables-and-views.md"),
    ),
    (
        "references/clusters-and-safety.md",
        include_str!("../skills/kborg/references/clusters-and-safety.md"),
    ),
    (
        "references/dashboards.md",
        include_str!("../skills/kborg/references/dashboards.md"),
    ),
    (
        "references/using-the-tui.md",
        include_str!("../skills/kborg/references/using-the-tui.md"),
    ),
    (
        "references/integrations.md",
        include_str!("../skills/kborg/references/integrations.md"),
    ),
    (
        "references/install-and-troubleshoot.md",
        include_str!("../skills/kborg/references/install-and-troubleshoot.md"),
    ),
];

/// Assistants that read personal Agent Skills folders:
/// `(name, folder under HOME that marks it installed, label)`.
pub const TARGETS: &[(&str, &str, &str)] = &[
    ("claude", ".claude", "Claude Code"),
    ("copilot", ".copilot", "GitHub Copilot"),
    ("codex", ".codex", "OpenAI Codex"),
    ("gemini", ".gemini", "Gemini CLI"),
    ("agents", ".agents", "the shared ~/.agents folder"),
];

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            "HOME is not set. Next: name a folder, kborg skill install /path/to/kborg".into()
        })
}

/// The assistant's own home folder (Codex honors `CODEX_HOME`).
fn base(home: &Path, name: &str, folder: &str) -> PathBuf {
    match (name, std::env::var_os("CODEX_HOME")) {
        ("codex", Some(codex)) if Path::new(&codex).is_absolute() => PathBuf::from(codex),
        _ => home.join(folder),
    }
}

/// The skill folder of one named assistant.
pub fn target_dir(name: &str) -> Result<PathBuf> {
    let (name, folder, _) = TARGETS
        .iter()
        .find(|(target, ..)| *target == name)
        .ok_or_else(|| {
            format!(
                "no skill folder for {}; use claude, copilot, codex, gemini, agents or an absolute folder. Next: for ChatGPT, Gemini or another chat app, run kborg skill print and attach the output",
                safety::safe_text(name, 40)
            )
        })?;
    Ok(base(&home()?, name, folder).join("skills/kborg"))
}

/// Installs into every assistant found on this machine (its home folder
/// exists) and always into the shared `~/.agents/skills`, which several
/// assistants read. Each result is reported; one failure does not stop the rest.
pub fn install_everywhere() -> Result<Vec<(&'static str, Result<PathBuf>)>> {
    let home = home()?;
    let mut results = Vec::new();
    for (name, folder, label) in TARGETS {
        let base = base(&home, name, folder);
        if *name == "agents" || base.is_dir() {
            results.push((*label, install(&base.join("skills/kborg"))));
        }
    }
    Ok(results)
}

/// A SKILL.md of the KBorg skill (any version), also one that another
/// installer such as `gh skill install` rewrote: its frontmatter names it.
fn is_ours(skill: &Path) -> bool {
    std::fs::read_to_string(skill).is_ok_and(|text| {
        let text = text.replace("\r\n", "\n");
        text.strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---"))
            .is_some_and(|(front, _)| front.lines().any(|line| line.trim() == "name: kborg"))
    })
}

/// Writes the skill into `dir`, creating it. An existing folder is updated
/// only when it holds an earlier KBorg skill; anything else is left alone.
pub fn install(dir: &Path) -> Result<PathBuf> {
    if !dir.is_absolute() {
        return Err("the skill folder must be an absolute path".into());
    }
    safety::checked_path(dir)?;
    let skill = dir.join("SKILL.md");
    let shown = || safety::safe_text(&dir.display().to_string(), 512);
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if !meta.is_dir() => {
            return Err(format!("{} exists and is not a folder", shown()));
        }
        Ok(_) => {
            let empty = std::fs::read_dir(dir)
                .map_err(|_| "cannot read the skill folder")?
                .next()
                .is_none();
            if !empty && !is_ours(&skill) {
                return Err(format!(
                    "{} already holds something that is not the KBorg skill; nothing was written. Next: choose another folder",
                    shown()
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("cannot inspect the skill folder".into()),
    }
    // References an earlier version shipped and this one does not.
    if let Ok(entries) = std::fs::read_dir(dir.join("references")) {
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = format!(
                "references/{}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
            );
            if path.extension().is_some_and(|ext| ext == "md")
                && !FILES.iter().any(|(file, _)| *file == relative)
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    for (relative, content) in FILES {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "cannot create the skill folder")?;
        }
        safety::atomic_write(&path, content.as_bytes())?;
    }
    Ok(skill)
}

/// The whole skill as one Markdown file, for chat assistants without skill
/// folders (ChatGPT, Gemini, Claude or Copilot in a browser): paste it into
/// a project's or custom assistant's instructions, or attach it.
pub fn bundle() -> String {
    let (_, skill) = FILES[0];
    let body = skill
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(skill, |(_, body)| body);
    let mut out = format!(
        "<!-- KBorg skill for KBorg {}, from `kborg skill print`. Use it as instructions for an AI assistant. -->\n\
         \n\
         > You are an assistant helping a person use KBorg. Follow the instructions below. \
         The reference sections they link to (references/NAME.md) are included further down in this same document.\n",
        env!("CARGO_PKG_VERSION")
    );
    out.push_str(body);
    for (relative, content) in &FILES[1..] {
        out.push_str(&format!("\n\n---\n\n<!-- {relative} -->\n\n{content}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Examples marked `<!-- FILE: NAME -->` just above their fenced block.
    fn examples() -> Vec<(&'static str, String, String)> {
        let mut found = Vec::new();
        for (source, text) in FILES {
            let lines: Vec<&str> = text.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                let Some(name) = line
                    .strip_prefix("<!-- FILE: ")
                    .and_then(|rest| rest.strip_suffix(" -->"))
                else {
                    continue;
                };
                let fence = lines.get(index + 1).copied().unwrap_or_default();
                assert!(
                    fence.starts_with("```"),
                    "{source}: the line after the FILE marker for {name} is not a code fence"
                );
                let body: Vec<&str> = lines[index + 2..]
                    .iter()
                    .take_while(|line| !line.starts_with("```"))
                    .copied()
                    .collect();
                found.push((*source, name.to_owned(), body.join("\n")));
            }
        }
        found
    }

    fn scratch(name: &str) -> Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!("kborg-skill-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|_| "fixture")?;
        dir.canonicalize().map_err(|_| "fixture".into())
    }

    #[test]
    fn every_example_in_the_skill_parses_with_kborgs_own_loader() -> Result<()> {
        let dir = scratch("examples")?;
        let mut seen = std::collections::BTreeSet::new();
        for (source, name, text) in examples() {
            let result = match name.as_str() {
                "keymap.yaml" => crate::keymap::parse(&text).map(drop),
                "hotkeys.yaml" => crate::hotkeys::parse(&text).map(drop),
                "aliases.yaml" => crate::aliases::parse(&text).map(drop),
                "views.yaml" => crate::views::parse(&text).map(drop),
                "ui.yaml" => crate::ui_config::parse(&text).map(drop),
                name if name.starts_with("contexts/") => {
                    crate::context_config::parse(&text).map(drop)
                }
                name if name.starts_with("themes/") => {
                    let path = dir.join(name.trim_start_matches("themes/"));
                    std::fs::write(&path, &text).map_err(|_| "fixture")?;
                    crate::themes::load_file(&path).map(drop)
                }
                name if name.ends_with(".json") => {
                    let path = dir.join(name);
                    std::fs::write(&path, &text).map_err(|_| "fixture")?;
                    crate::config::load(&path).map(drop)
                }
                other => Err(format!("no loader for example file {other}")),
            };
            if let Err(error) = result {
                return Err(format!("{source}: example {name} does not parse: {error}"));
            }
            seen.insert(
                name.split('/')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(".json")
                    .to_owned(),
            );
        }
        for file in [
            "keymap.yaml",
            "hotkeys.yaml",
            "aliases.yaml",
            "views.yaml",
            "ui.yaml",
            "contexts",
            "themes",
            "kborg-team",
        ] {
            assert!(
                seen.contains(file),
                "the skill has no checked example for {file}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn every_kborg_command_the_skill_names_exists() {
        let help = include_str!("../docs/CLI_HELP.txt");
        let known = |word: &str| {
            if word.starts_with("--") {
                return help.contains(word);
            }
            help.contains(&format!("kborg {word}")) || help.contains(&format!("\n  {word} "))
        };
        for (source, text) in FILES {
            for (index, _) in text.match_indices("`kborg ") {
                let word: String = text[index + 7..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if word.is_empty() || word.starts_with(char::is_uppercase) {
                    continue;
                }
                assert!(
                    known(&word),
                    "{source} names `kborg {word}`, which does not exist"
                );
            }
        }
    }

    #[test]
    fn the_skill_has_agent_skills_frontmatter_and_ships_every_reference_it_links() {
        let (_, skill) = FILES[0];
        // .gitattributes keeps the skill LF on every checkout, so every
        // platform's binary embeds the same bytes.
        for (relative, content) in FILES {
            assert!(!content.contains('\r'), "{relative} has CR line endings");
        }
        assert!(skill.starts_with("---\nname: kborg\ndescription: "));
        let head = skill[4..]
            .find("\n---\n")
            .map(|end| &skill[..end + 4])
            .unwrap_or_default();
        let description = head
            .lines()
            .find_map(|line| line.strip_prefix("description: "))
            .unwrap_or_default();
        assert!(
            (50..=1024).contains(&description.len()),
            "description must be 50..1024 characters, is {}",
            description.len()
        );
        for (relative, _) in &FILES[1..] {
            assert!(
                skill.contains(relative),
                "SKILL.md does not link {relative}"
            );
        }
        // Every link in any file points to a shipped file.
        for (source, text) in FILES {
            for (index, _) in text.match_indices("references/") {
                let name: String = text[index..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || "/-_.".contains(*c))
                    .collect();
                let name = name.trim_end_matches('.');
                if name.ends_with(".md") {
                    assert!(
                        FILES.iter().any(|(file, _)| *file == name),
                        "{source} links {name}, which is not shipped"
                    );
                }
            }
        }
    }

    #[test]
    fn the_bundle_holds_every_file_once_without_frontmatter() {
        let bundle = bundle();
        assert!(bundle.contains(env!("CARGO_PKG_VERSION")));
        assert!(!bundle.contains("\nname: kborg\n"));
        assert!(bundle.contains("## Ground rules"));
        for (relative, content) in &FILES[1..] {
            assert_eq!(bundle.matches(&format!("<!-- {relative} -->")).count(), 1);
            assert!(bundle.contains(content.trim()));
        }
    }

    #[test]
    fn install_writes_updates_and_never_takes_a_foreign_folder() -> Result<()> {
        let base = scratch("install")?;
        let dir = base.join("skills/kborg");
        let skill = install(&dir)?;
        for (relative, content) in FILES {
            assert_eq!(
                std::fs::read_to_string(dir.join(relative)).ok().as_deref(),
                Some(*content)
            );
        }
        // An older copy is replaced, and a reference it had that is gone is removed.
        std::fs::write(&skill, "---\nname: kborg\ndescription: old\n---\nold\n")
            .map_err(|_| "fixture")?;
        std::fs::write(dir.join("references/config-files.md"), "old").map_err(|_| "fixture")?;
        install(&dir)?;
        assert_eq!(
            std::fs::read_to_string(&skill).ok().as_deref(),
            Some(FILES[0].1)
        );
        assert!(!dir.join("references/config-files.md").exists());
        // A copy another installer rewrote (keys sorted, metadata added) is still ours.
        std::fs::write(
            &skill,
            "---\ndescription: old\nmetadata:\n    github-repo: c0dewars/kborg\n    github-ref: v0.7.0\n    github-sha: abc\n    local-path: /x\nname: kborg\n---\nold\n",
        )
        .map_err(|_| "fixture")?;
        install(&dir)?;
        assert_eq!(
            std::fs::read_to_string(&skill).ok().as_deref(),
            Some(FILES[0].1)
        );
        // A Windows copy with CRLF line endings is ours too.
        std::fs::write(
            &skill,
            "---\r\nname: kborg\r\ndescription: old\r\n---\r\nold\r\n",
        )
        .map_err(|_| "fixture")?;
        install(&dir)?;
        assert_eq!(
            std::fs::read_to_string(&skill).ok().as_deref(),
            Some(FILES[0].1)
        );
        // Someone else's folder is never written into.
        let other = base.join("other");
        std::fs::create_dir_all(&other).map_err(|_| "fixture")?;
        std::fs::write(other.join("SKILL.md"), "---\nname: other\n---\n").map_err(|_| "fixture")?;
        let refused = install(&other).err().unwrap_or_default();
        assert!(refused.contains("not the KBorg skill"), "{refused}");
        assert!(!other.join("references").exists());
        assert!(install(Path::new("relative/kborg")).is_err());
        assert!(target_dir("chatgpt").is_err());
        let _ = std::fs::remove_dir_all(&base);
        Ok(())
    }
}
