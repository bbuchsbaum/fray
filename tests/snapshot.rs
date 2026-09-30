use fray::snapshot;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "fray-snapshot-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn git(root: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
fn write(root: &Path, path: &str, bytes: &[u8]) {
    let p = root.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}
fn repo() -> (Temp, PathBuf, PathBuf) {
    let t = Temp::new();
    let root = t.0.join("repo");
    let out = t.0.join("out");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&out).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "Snapshot Test"]);
    write(&root, "src/modified.txt", b"base");
    write(&root, "src/deleted.txt", b"delete");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "initial"]);
    (t, root, out)
}

#[test]
fn captures_worktree_states_and_reuses_stable_identity() {
    let (_t, root, out) = repo();
    write(&root, "src/modified.txt", b"modified");
    write(&root, "src/staged.txt", b"staged");
    git(&root, &["add", "src/staged.txt"]);
    git(&root, &["rm", "-q", "src/deleted.txt"]); // staged deletion remains in HEAD inventory
    write(&root, ".gitignore", b"ignored.txt\n");
    write(&root, "src/ignored.txt", b"ignored");
    for n in 0..22 {
        write(
            &root,
            &format!("src/untracked-{n:02}.bin"),
            if n == 0 { &[0, 255, 1] } else { b"untracked" },
        );
    }
    let executable = root.join("src/untracked-01.bin");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let paths = vec![PathBuf::from("src")];
    let one = snapshot::create(&root, &out, &paths).unwrap();
    let two = snapshot::create(&root, &out, &paths).unwrap();
    assert_eq!(one["manifest"], two["manifest"]);
    assert_eq!(one["files"], 25); // modified, staged, 22 untracked, tombstone
    let bundle = PathBuf::from(one["bundle"].as_str().unwrap());
    assert_eq!(
        fs::read(bundle.join("files/src/untracked-00.bin")).unwrap(),
        vec![0, 255, 1]
    );
    assert!(
        fs::metadata(bundle.join("files/src/untracked-01.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111
            != 0
    );
    assert!(!bundle.join("files/src/ignored.txt").exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["path"] == "src/deleted.txt")
            .unwrap()["state"],
        "tombstone"
    );
    assert_eq!(
        snapshot::verify(&bundle).unwrap()["manifest"],
        one["manifest"]
    );
}

#[test]
fn rejects_ignored_literals_symlinks_and_output_overlap() {
    let (_t, root, out) = repo();
    write(&root, ".gitignore", b"ignored.txt\n");
    write(&root, "ignored.txt", b"x");
    assert_eq!(
        snapshot::create(&root, &out, &[PathBuf::from("ignored.txt")])
            .unwrap_err()
            .code,
        "ignored_path"
    );
    write(&root, "-literal.txt", b"ok");
    assert!(snapshot::create(&root, &out, &[PathBuf::from("-literal.txt")]).is_ok());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("-literal.txt", root.join("link.txt")).unwrap();
    }
    assert_eq!(
        snapshot::create(&root, &out, &[PathBuf::from("link.txt")])
            .unwrap_err()
            .code,
        "invalid_file"
    );
    let nested_output = root.join("src/out");
    fs::create_dir(&nested_output).unwrap();
    assert_eq!(
        snapshot::create(&root, &nested_output, &[PathBuf::from("src")])
            .unwrap_err()
            .code,
        "output_overlap"
    );
    let (_t, root, out) = repo();
    assert!(snapshot::create(&root, &out, &[PathBuf::from(".")]).is_ok());
}

#[test]
fn rejects_missing_paths_beneath_symlinks_and_gitlinks() {
    let (_t, root, out) = repo();
    let external = root.parent().unwrap().join("external");
    fs::create_dir(&external).unwrap();
    fs::remove_dir_all(root.join("src")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("src")).unwrap();
    assert_eq!(
        snapshot::create(&root, &out, &[PathBuf::from("src/deleted.txt")])
            .unwrap_err()
            .code,
        "invalid_file"
    );

    let (_t, root, out) = repo();
    let head = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let cacheinfo = format!(
        "160000,{},src/gitlink",
        String::from_utf8(head.stdout).unwrap().trim()
    );
    git(&root, &["update-index", "--add", "--cacheinfo", &cacheinfo]);
    assert_eq!(
        snapshot::create(&root, &out, &[PathBuf::from("src")])
            .unwrap_err()
            .code,
        "gitlink"
    );
}

#[test]
fn verification_rejects_tampered_and_unlisted_content() {
    let (_t, root, out) = repo();
    write(&root, "src/a.txt", b"a");
    let created = snapshot::create(&root, &out, &[PathBuf::from("src")]).unwrap();
    let bundle = PathBuf::from(created["bundle"].as_str().unwrap());
    fs::write(bundle.join("files/src/a.txt"), b"changed").unwrap();
    assert_eq!(snapshot::verify(&bundle).unwrap_err().code, "tampered");
}

#[test]
fn verification_rejects_bundle_root_symlink() {
    let (_t, root, out) = repo();
    write(&root, "src/a.txt", b"one");
    let first = snapshot::create(&root, &out, &[PathBuf::from("src")]).unwrap();
    write(&root, "src/a.txt", b"two");
    let second = snapshot::create(&root, &out, &[PathBuf::from("src")]).unwrap();
    let first = PathBuf::from(first["bundle"].as_str().unwrap());
    let second = PathBuf::from(second["bundle"].as_str().unwrap());
    fs::remove_dir_all(&first).unwrap();
    std::os::unix::fs::symlink(&second, &first).unwrap();
    assert_eq!(snapshot::verify(&first).unwrap_err().code, "invalid_bundle");
}

#[test]
fn verification_rejects_symlinked_manifest_before_reading() {
    let (_t, root, out) = repo();
    write(&root, "src/a.txt", b"a");
    let created = snapshot::create(&root, &out, &[PathBuf::from("src")]).unwrap();
    let bundle = PathBuf::from(created["bundle"].as_str().unwrap());
    let manifest = bundle.join("manifest.json");
    fs::remove_file(&manifest).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &manifest).unwrap();
    assert_eq!(snapshot::verify(&bundle).unwrap_err().code, "tampered");
}

#[test]
fn creates_missing_output_root_after_overlap_check() {
    let (_t, root, out) = repo();
    write(&root, "src/a.txt", b"a");
    let first_use = out.join("evidence/snapshots");
    assert!(!first_use.exists());
    let created = snapshot::create(&root, &first_use, &[PathBuf::from("src")]).unwrap();
    assert!(Path::new(created["bundle"].as_str().unwrap()).is_dir());

    let overlapping = root.join("src/evidence/snapshots");
    assert_eq!(
        snapshot::create(&root, &overlapping, &[PathBuf::from("src")])
            .unwrap_err()
            .code,
        "output_overlap"
    );
    assert!(!overlapping.exists());
}

#[test]
fn rejects_unmerged_selected_path_containing_a_tab() {
    let (_t, root, out) = repo();
    let path = "src/a\tb";
    write(&root, path, b"base");
    git(&root, &["add", path]);
    git(&root, &["commit", "-qm", "tab base"]);
    let branch = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["branch", "--show-current"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    git(&root, &["branch", "side"]);
    write(&root, path, b"main");
    git(&root, &["commit", "-am", "main"]);
    git(&root, &["checkout", "-q", "side"]);
    write(&root, path, b"side");
    git(&root, &["commit", "-am", "side"]);
    git(&root, &["checkout", "-q", &branch]);
    let merge = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["merge", "side", "--no-edit"])
        .output()
        .unwrap();
    assert!(!merge.status.success());
    assert_eq!(
        snapshot::create(&root, &out, &[PathBuf::from("src")])
            .unwrap_err()
            .code,
        "unmerged"
    );
}
