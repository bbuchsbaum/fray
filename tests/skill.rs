use fray::{model::random_key, skill};
use std::{fs, os::unix::fs::symlink, path::PathBuf, process::Command};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fray-skill-test-{}", random_key().unwrap()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn installs_one_identical_skill_for_both_hosts_idempotently() {
    let project = Project::new();
    fs::create_dir(project.0.join(".claude")).unwrap();
    let settings = project.0.join(".claude/settings.json");
    fs::write(&settings, "{\"existing\":true}").unwrap();
    let installed = skill::install(&project.0, "both").unwrap();
    assert_eq!(installed["installed"].as_array().unwrap().len(), 2);
    for host in [".agents", ".claude"] {
        assert_eq!(
            fs::read_to_string(project.0.join(host).join("skills/fray/SKILL.md")).unwrap(),
            skill::CONTENT
        );
    }
    let again = skill::install(&project.0, "both").unwrap();
    assert_eq!(again["installed"].as_array().unwrap().len(), 0);
    assert_eq!(again["unchanged"].as_array().unwrap().len(), 2);
    assert_eq!(fs::read_to_string(settings).unwrap(), "{\"existing\":true}");
    assert!(!project.0.join(".fray").exists());
}

#[test]
fn single_host_install_only_touches_that_host() {
    for (host, installed, absent) in [
        ("codex", ".agents", ".claude"),
        ("claude", ".claude", ".agents"),
    ] {
        let project = Project::new();
        skill::install(&project.0, host).unwrap();
        assert!(project
            .0
            .join(installed)
            .join("skills/fray/SKILL.md")
            .is_file());
        assert!(!project.0.join(absent).exists());
    }
}

#[test]
fn catalog_and_named_content_are_available() {
    assert_eq!(
        skill::catalog()
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        vec!["fray", "fray-seam", "fray-review"]
    );
    assert_eq!(skill::content("fray").unwrap(), skill::CONTENT);
    assert!(skill::content("fray-seam")
        .unwrap()
        .contains("Fray seam collaboration"));
    assert!(skill::content("fray-review")
        .unwrap()
        .contains("Fray review"));
    assert_eq!(skill::content("all").unwrap_err().code, "invalid");
}

#[test]
fn named_install_can_install_every_bundled_skill() {
    let project = Project::new();
    let installed = skill::install_named(&project.0, "both", "all").unwrap();
    assert_eq!(installed["installed"].as_array().unwrap().len(), 6);
    for host in [".agents", ".claude"] {
        for name in ["fray", "fray-seam", "fray-review"] {
            assert_eq!(
                fs::read_to_string(
                    project
                        .0
                        .join(host)
                        .join("skills")
                        .join(name)
                        .join("SKILL.md")
                )
                .unwrap(),
                skill::content(name).unwrap()
            );
        }
    }
}

#[test]
fn selected_skill_conflict_preflights_all_destinations() {
    let project = Project::new();
    let custom = project.0.join(".claude/skills/fray-review/SKILL.md");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    fs::write(&custom, "custom review instructions").unwrap();
    assert_eq!(
        skill::install_named(&project.0, "both", "all")
            .unwrap_err()
            .code,
        "skill_conflict"
    );
    assert!(!project.0.join(".agents").exists());
    assert!(!project.0.join(".claude/skills/fray/SKILL.md").exists());
    assert_eq!(
        fs::read_to_string(custom).unwrap(),
        "custom review instructions"
    );
}

#[test]
fn existing_custom_skill_prevents_any_installation() {
    let project = Project::new();
    let path = project.0.join(".claude/skills/fray/SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "custom instructions").unwrap();
    assert_eq!(
        skill::install(&project.0, "both").unwrap_err().code,
        "skill_conflict"
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "custom instructions");
    assert!(!project.0.join(".agents").exists());
}

#[test]
fn symlinked_configuration_is_not_followed() {
    let project = Project::new();
    let elsewhere = Project::new();
    symlink(&elsewhere.0, project.0.join(".claude")).unwrap();
    assert_eq!(
        skill::install(&project.0, "both").unwrap_err().code,
        "skill_conflict"
    );
    assert!(!elsewhere.0.join("skills").exists());
    assert!(!project.0.join(".agents").exists());
}

#[test]
fn symlinked_skill_is_not_replaced_even_when_content_matches() {
    let project = Project::new();
    let original = project.0.join("original.md");
    fs::write(&original, skill::CONTENT).unwrap();
    let path = project.0.join(".agents/skills/fray/SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(&original, &path).unwrap();
    assert_eq!(
        skill::install(&project.0, "codex").unwrap_err().code,
        "skill_conflict"
    );
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
}

#[test]
fn cli_prints_and_installs_without_an_agent_or_daemon() {
    let project = Project::new();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_fray"))
            .current_dir(&project.0)
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_HOME")
            .args(args)
            .output()
            .unwrap()
    };
    let printed = run(&["skill"]);
    assert!(printed.status.success());
    assert_eq!(printed.stdout, skill::CONTENT.as_bytes());
    let installed = run(&["--json", "skill", "--install", "both"]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(result["installed"].as_array().unwrap().len(), 2);
    assert!(!project.0.join(".fray").exists());
    assert!(!project.0.join("AGENTS.md").exists());
    assert!(!project.0.join("CLAUDE.md").exists());
    let listed = run(&["--json", "skill", "--list"]);
    assert!(listed.status.success());
    let result: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(
        result["items"].as_array().unwrap().len(),
        skill::catalog().len()
    );
    for entry in skill::catalog() {
        let printed = run(&["skill", entry.name]);
        assert!(printed.status.success());
        assert_eq!(
            printed.stdout,
            skill::content(entry.name).unwrap().as_bytes()
        );
    }
    let bundle = run(&["--json", "skill", "all", "--install", "both"]);
    assert!(
        bundle.status.success(),
        "{}",
        String::from_utf8_lossy(&bundle.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&bundle.stdout).unwrap();
    assert_eq!(result["installed"].as_array().unwrap().len(), 4);
    assert_eq!(result["unchanged"].as_array().unwrap().len(), 2);
}
