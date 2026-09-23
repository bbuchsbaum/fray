//! Embedded collaboration skills, installable into either supported host.
use crate::model::{Error, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{fs, io::Write, path::Path};

/// Backward-compatible alias for the default, general-purpose skill.
pub const CONTENT: &str = include_str!("../skills/fray/SKILL.md");
const SEAM_CONTENT: &str = include_str!("../skills/fray-seam/SKILL.md");
const REVIEW_CONTENT: &str = include_str!("../skills/fray-review/SKILL.md");

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Skill {
    pub name: &'static str,
    pub description: &'static str,
}

const CATALOG: &[Skill] = &[
    Skill {
        name: "fray",
        description: "Coordinate peer questions, evidence, handoffs, and reviews through Fray.",
    },
    Skill {
        name: "fray-seam",
        description: "Coordinate bounded cross-module implementation seams with evidence.",
    },
    Skill {
        name: "fray-review",
        description: "Perform SHA-bound peer review and report an explicit verdict.",
    },
];

pub fn catalog() -> &'static [Skill] {
    CATALOG
}

pub fn content(name: &str) -> Result<&'static str> {
    match name {
        "fray" => Ok(CONTENT),
        "fray-seam" => Ok(SEAM_CONTENT),
        "fray-review" => Ok(REVIEW_CONTENT),
        "all" => Err(Error::invalid("'all' is only valid with skill install")),
        _ => Err(Error::invalid(
            "skill must be fray, fray-seam, fray-review, or all with install",
        )),
    }
}

pub fn install(project: &Path, host: &str) -> Result<Value> {
    install_named(project, host, "fray")
}

pub fn install_named(project: &Path, host: &str, name: &str) -> Result<Value> {
    let hosts: &[&str] = match host {
        "codex" => &[".agents"],
        "claude" => &[".claude"],
        "both" => &[".agents", ".claude"],
        _ => return Err(Error::invalid("skill host must be codex, claude, or both")),
    };
    let skills: Vec<(&str, &str)> = match name {
        "all" => catalog()
            .iter()
            .map(|skill| {
                (
                    skill.name,
                    content(skill.name).expect("catalogued skill has content"),
                )
            })
            .collect(),
        _ => vec![(name, content(name)?)],
    };
    let project = fs::canonicalize(project)?;
    let mut targets = Vec::new();
    let mut unchanged = Vec::new();
    // Inspect every selected destination before creating anything. Never follow a
    // host configuration symlink or overwrite a project's customized instructions.
    for &host in hosts {
        for &(name, body) in &skills {
            let mut dir = project.clone();
            for component in [host, "skills", name] {
                dir.push(component);
                match fs::symlink_metadata(&dir) {
                    Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                    Ok(_) => return Err(Error::new("skill_conflict", format!("{} must be a real directory; install manually to preserve existing configuration", dir.display()))),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            let path = dir.join("SKILL.md");
            match fs::symlink_metadata(&path) {
                Ok(meta)
                    if meta.is_file()
                        && !meta.file_type().is_symlink()
                        && fs::read(&path)? == body.as_bytes() =>
                {
                    unchanged.push(path)
                }
                Ok(_) => {
                    return Err(Error::new(
                        "skill_conflict",
                        format!(
                        "{} already exists with different content; review and merge it manually",
                        path.display()
                    ),
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => targets.push((path, body)),
                Err(e) => return Err(e.into()),
            }
        }
    }
    let mut installed = Vec::new();
    for (path, body) in targets {
        fs::create_dir_all(path.parent().expect("skill path has a parent"))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(body.as_bytes())?;
        installed.push(path);
    }
    Ok(
        json!({"installed": installed, "unchanged": unchanged, "scope": "project", "hooks_installed": false}),
    )
}
