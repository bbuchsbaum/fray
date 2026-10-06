//! Bounded, best-effort worktree snapshots.  This is deliberately not an
//! adversary-resistant or filesystem-atomic snapshot primitive.
#[path = "sha256.rs"]
mod sha256;

use crate::model::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    process::Command,
};

const VERSION: u32 = 1;
const MAX_FILES: usize = 16_384;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const FLUSH_WORKERS: usize = 4;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    base_head: String,
    scopes: Vec<String>,
    files: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    state: String,
    sha256: Option<String>,
    executable: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct SourceMeta {
    len: u64,
    modified: Option<std::time::SystemTime>,
    executable: bool,
}

fn err(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| err("git", e.to_string()))?;
    if !output.status.success() {
        return Err(err(
            "git",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(output.stdout)
}

fn nul_paths(bytes: &[u8]) -> Result<Vec<String>> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| {
            std::str::from_utf8(p)
                .map(str::to_owned)
                .map_err(|_| err("invalid_path", "Git returned a non-UTF-8 path"))
        })
        .collect()
}

fn checked_relative(path: &Path) -> Result<String> {
    let s = path
        .to_str()
        .ok_or_else(|| err("invalid_path", "paths must be UTF-8"))?;
    if s.is_empty() {
        return Err(err("invalid_path", "empty paths are not allowed"));
    }
    if s == "." {
        return Ok(s.to_string());
    }
    for c in path.components() {
        match c {
            Component::Normal(_) => {}
            _ => {
                return Err(err(
                    "invalid_path",
                    format!("path must be literal repo-relative: {s}"),
                ))
            }
        }
    }
    if s == ".git" || s.starts_with(".git/") {
        return Err(err("invalid_path", ".git cannot be snapshotted"));
    }
    Ok(s.to_string())
}

fn beneath(path: &str, scope: &str) -> bool {
    scope == "."
        || path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn source_meta(path: &Path) -> Result<SourceMeta> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Err(err(
            "invalid_file",
            format!("{} is not a regular file", path.display()),
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(err(
            "resource_limit",
            format!("{} exceeds per-file limit", path.display()),
        ));
    }
    Ok(SourceMeta {
        len: meta.len(),
        modified: meta.modified().ok(),
        executable: meta.permissions().mode() & 0o111 != 0,
    })
}

fn reject_source_symlink_traversal(root: &Path, relative: &str) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return Err(err("invalid_path", "invalid source path"));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(err(
                    "invalid_file",
                    format!("source path traverses symlink: {relative}"),
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn hash_reader<R: Read>(mut input: R) -> Result<String> {
    let mut hasher = sha256::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(sha256::hex(hasher.finalize()))
}
fn hash_file(path: &Path) -> Result<String> {
    hash_reader(File::open(path)?)
}
fn hash_bytes(bytes: &[u8]) -> Result<String> {
    hash_reader(std::io::Cursor::new(bytes))
}

fn copy_and_hash(source: &Path, destination: &Path, meta: &SourceMeta) -> Result<String> {
    let before = source_meta(source)?;
    if &before != meta {
        return Err(err(
            "snapshot_changed",
            format!("{} changed before copy", source.display()),
        ));
    }
    let mut input = File::open(source)?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut hasher = sha256::Sha256::new();
    let mut bytes = 0u64;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        out.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
    }
    if bytes != meta.len || source_meta(source)? != *meta {
        return Err(err(
            "snapshot_changed",
            format!("{} changed during copy", source.display()),
        ));
    }
    fs::set_permissions(
        destination,
        fs::Permissions::from_mode(if meta.executable { 0o755 } else { 0o644 }),
    )?;
    Ok(sha256::hex(hasher.finalize()))
}

fn flush_copied_files(paths: &[PathBuf]) -> Result<()> {
    let workers = FLUSH_WORKERS.min(paths.len());
    std::thread::scope(|scope| {
        let mut joins = Vec::with_capacity(workers);
        for worker in 0..workers {
            joins.push(scope.spawn(move || -> Result<()> {
                for path in paths.iter().skip(worker).step_by(workers) {
                    File::open(path)?.sync_all()?;
                }
                Ok(())
            }));
        }
        for join in joins {
            join.join()
                .map_err(|_| err("snapshot", "snapshot flush worker panicked"))??;
        }
        Ok(())
    })
}

fn git_state(root: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let head = git(root, &["rev-parse", "--verify", "HEAD"]).map_err(|_| {
        err(
            "no_head",
            "snapshotting requires a repository with an initial HEAD commit",
        )
    })?;
    Ok((head, git(root, &["ls-files", "-s", "-z"])?))
}

fn selected_inventory(root: &Path, scopes: &[String]) -> Result<BTreeSet<String>> {
    if !root.join(".git").exists() && git(root, &["rev-parse", "--is-inside-work-tree"]).is_err() {
        return Err(err("not_git", "root must be a Git working tree"));
    }
    let unmerged = nul_paths(&git(root, &["ls-files", "-u", "-z"])?)?;
    for raw in unmerged {
        let (_, p) = raw
            .split_once('\t')
            .ok_or_else(|| err("git", "invalid unmerged Git index entry"))?;
        if scopes.iter().any(|s| beneath(p, s)) {
            return Err(err("unmerged", format!("selected path is unmerged: {p}")));
        }
    }
    let sparse_bytes = git(root, &["ls-files", "-v", "-z"])?;
    let sparse = String::from_utf8_lossy(&sparse_bytes);
    for record in sparse.split('\0') {
        if let Some(p) = record.get(2..) {
            if record.starts_with('S') && scopes.iter().any(|s| beneath(p, s)) {
                return Err(err(
                    "sparse_checkout",
                    format!("selected path is absent due to sparse checkout: {p}"),
                ));
            }
        }
    }
    for args in [
        ["ls-files", "-s", "-z"].as_slice(),
        ["ls-tree", "-r", "-z", "HEAD"].as_slice(),
    ] {
        for record in nul_paths(&git(root, args)?)? {
            let (header, path) = record
                .split_once('\t')
                .ok_or_else(|| err("git", "invalid Git tree entry"))?;
            if header.starts_with("160000 ") && scopes.iter().any(|scope| beneath(path, scope)) {
                return Err(err(
                    "gitlink",
                    format!("selected path is a Git submodule: {path}"),
                ));
            }
        }
    }
    let mut all = BTreeSet::new();
    for args in [
        ["ls-files", "-z", "--cached"].as_slice(),
        ["ls-tree", "-r", "-z", "--name-only", "HEAD"].as_slice(),
        ["ls-files", "-z", "--others", "--exclude-standard"].as_slice(),
    ] {
        for p in nul_paths(&git(root, args)?)? {
            if scopes.iter().any(|s| beneath(&p, s)) {
                all.insert(p);
            }
        }
    }
    for scope in scopes {
        if !all.iter().any(|p| beneath(p, scope)) {
            let ignored = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["check-ignore", "-q", "--", scope])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            return Err(err(
                if ignored {
                    "ignored_path"
                } else {
                    "unmatched_path"
                },
                format!("selected path has no snapshottable files: {scope}"),
            ));
        }
    }
    if all.len() > MAX_FILES {
        return Err(err("resource_limit", "too many selected files"));
    }
    Ok(all)
}

fn staging(root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    let p = root.join(format!(".fray-snapshot-{}", crate::model::random_key()?));
    fs::DirBuilder::new().mode(0o700).create(&p)?;
    fs::create_dir(p.join("files"))?;
    Ok(p)
}

fn resolved_output_root(output_root: &Path) -> Result<PathBuf> {
    let mut candidate = if output_root.is_absolute() {
        output_root.to_path_buf()
    } else {
        std::env::current_dir()?.join(output_root)
    };
    let mut missing = Vec::new();
    while !candidate.exists() {
        let name = candidate
            .file_name()
            .ok_or_else(|| err("invalid_output", "output_root has no existing ancestor"))?
            .to_os_string();
        missing.push(name);
        candidate.pop();
    }
    let mut resolved = candidate
        .canonicalize()
        .map_err(|_| err("invalid_output", "cannot resolve output_root ancestor"))?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

pub fn create(root: &Path, output_root: &Path, paths: &[PathBuf]) -> Result<Value> {
    create_inner(root, output_root, paths, None)
}

fn create_inner(
    root: &Path,
    output_root: &Path,
    paths: &[PathBuf],
    mut after_capture: Option<&mut dyn FnMut()>,
) -> Result<Value> {
    if paths.is_empty() || paths.len() > 64 {
        return Err(err("invalid_path", "provide 1..64 explicit paths"));
    }
    let root = root
        .canonicalize()
        .map_err(|_| err("not_git", "root must exist"))?;
    let out = resolved_output_root(output_root)?;
    if out.exists() && !out.is_dir() {
        return Err(err("invalid_output", "output_root must be a directory"));
    }
    let mut scopes: Vec<String> = paths
        .iter()
        .map(|p| checked_relative(p))
        .collect::<Result<_>>()?;
    scopes.sort();
    scopes.dedup();
    for scope in &scopes {
        if out.starts_with(root.join(scope)) {
            return Err(err(
                "output_overlap",
                "output_root is inside selected input",
            ));
        }
    }
    fs::create_dir_all(&out)?;
    if !out.is_dir() {
        return Err(err("invalid_output", "output_root must be a directory"));
    }
    let before_git = git_state(&root)?;
    let base_head = String::from_utf8(before_git.0.clone())
        .map_err(|_| err("git", "HEAD is not UTF-8"))?
        .trim()
        .to_string();
    let inventory = selected_inventory(&root, &scopes)?;
    let stage = staging(&out)?;
    let result = (|| {
        let mut entries = Vec::new();
        let mut copied_files = Vec::new();
        let mut total = 0u64;
        for path in &inventory {
            let source = root.join(path);
            reject_source_symlink_traversal(&root, path)?;
            match fs::symlink_metadata(&source) {
                Ok(_) => {
                    let meta = source_meta(&source)?;
                    total = total
                        .checked_add(meta.len)
                        .ok_or_else(|| err("resource_limit", "snapshot too large"))?;
                    if total > MAX_TOTAL_BYTES {
                        return Err(err("resource_limit", "snapshot exceeds total size limit"));
                    }
                    let target = stage.join("files").join(path);
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let digest = copy_and_hash(&source, &target, &meta)?;
                    copied_files.push(target);
                    entries.push(Entry {
                        path: path.clone(),
                        state: "file".into(),
                        sha256: Some(digest),
                        executable: meta.executable,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => entries.push(Entry {
                    path: path.clone(),
                    state: "tombstone".into(),
                    sha256: None,
                    executable: false,
                }),
                Err(e) => return Err(e.into()),
            }
        }
        flush_copied_files(&copied_files)?;
        let manifest = Manifest {
            version: VERSION,
            base_head: base_head.clone(),
            scopes: scopes.clone(),
            files: entries,
        };
        let bytes = serde_json::to_vec(&manifest)?;
        let digest = hash_bytes(&bytes)?;
        fs::write(stage.join("manifest.json"), &bytes)?;
        if let Some(hook) = after_capture.as_mut() {
            hook();
        }
        if selected_inventory(&root, &scopes)? != inventory || git_state(&root)? != before_git {
            return Err(err(
                "snapshot_changed",
                "worktree Git inventory or index changed during snapshot",
            ));
        }
        for entry in &manifest.files {
            let source = root.join(&entry.path);
            reject_source_symlink_traversal(&root, &entry.path)?;
            if entry.state == "file" {
                let meta = source_meta(&source)?;
                if meta.executable != entry.executable
                    || hash_file(&source)? != entry.sha256.clone().unwrap()
                {
                    return Err(err(
                        "snapshot_changed",
                        format!("{} changed after copy", entry.path),
                    ));
                }
            } else if entry.state == "tombstone" {
                match fs::symlink_metadata(&source) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => {
                        return Err(err(
                            "snapshot_changed",
                            format!("{} was recreated after capture", entry.path),
                        ));
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
        let final_dir = out.join(&digest);
        if final_dir.exists() {
            let verified = verify(&final_dir)?;
            if verified["manifest"] != format!("manifest:{digest}") {
                return Err(err(
                    "invalid_bundle",
                    "existing bundle has the wrong identity",
                ));
            }
            fs::remove_dir_all(&stage)?;
        } else if let Err(e) = fs::rename(&stage, &final_dir) {
            if final_dir.exists() {
                let verified = verify(&final_dir)?;
                if verified["manifest"] != format!("manifest:{digest}") {
                    return Err(err(
                        "invalid_bundle",
                        "existing bundle has the wrong identity",
                    ));
                }
                fs::remove_dir_all(&stage)?;
            } else {
                return Err(e.into());
            }
        }
        Ok(
            json!({"version": VERSION, "bundle": final_dir, "manifest": format!("manifest:{digest}"), "base_head": base_head, "files": manifest.files.len()}),
        )
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

pub fn verify(bundle: &Path) -> Result<Value> {
    let requested =
        fs::symlink_metadata(bundle).map_err(|_| err("invalid_bundle", "bundle does not exist"))?;
    if requested.file_type().is_symlink() || !requested.is_dir() {
        return Err(err(
            "invalid_bundle",
            "bundle root must be a real directory",
        ));
    }
    let bundle = bundle
        .canonicalize()
        .map_err(|_| err("invalid_bundle", "bundle does not exist"))?;
    let manifest_path = bundle.join("manifest.json");
    let manifest_meta = fs::symlink_metadata(&manifest_path)?;
    if manifest_meta.file_type().is_symlink() || !manifest_meta.is_file() {
        return Err(err("tampered", "manifest must be a regular file"));
    }
    if manifest_meta.len() > MAX_MANIFEST_BYTES {
        return Err(err("resource_limit", "manifest exceeds size limit"));
    }
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.version != VERSION {
        return Err(err("invalid_bundle", "unsupported snapshot version"));
    }
    let digest = hash_bytes(&manifest_bytes)?;
    if bundle.file_name().and_then(|p| p.to_str()) != Some(&digest) {
        return Err(err(
            "invalid_bundle",
            "bundle directory does not match manifest identity",
        ));
    }
    let mut expected = BTreeMap::new();
    for entry in &manifest.files {
        let p = checked_relative(Path::new(&entry.path))?;
        if expected.insert(p, entry).is_some() {
            return Err(err("invalid_bundle", "duplicate manifest path"));
        }
    }
    let mut root_names = BTreeSet::new();
    for child in fs::read_dir(&bundle)? {
        let child = child?.path();
        let meta = fs::symlink_metadata(&child)?;
        if meta.file_type().is_symlink() || (!meta.is_file() && !meta.is_dir()) {
            return Err(err("tampered", "invalid bundle root entry"));
        }
        root_names.insert(
            child
                .file_name()
                .and_then(|p| p.to_str())
                .ok_or_else(|| err("invalid_path", "non-UTF-8 bundle path"))?
                .to_string(),
        );
    }
    if root_names != BTreeSet::from(["files".to_string(), "manifest.json".to_string()]) {
        return Err(err(
            "tampered",
            "bundle root inventory differs from manifest",
        ));
    }
    let files = bundle.join("files");
    let mut found = BTreeSet::new();
    fn walk(base: &Path, dir: &Path, found: &mut BTreeSet<String>) -> Result<()> {
        for child in fs::read_dir(dir)? {
            let child = child?.path();
            let meta = fs::symlink_metadata(&child)?;
            if meta.file_type().is_symlink() {
                return Err(err("tampered", "symlink in bundle"));
            }
            if meta.is_dir() {
                walk(base, &child, found)?;
            } else if meta.is_file() {
                found.insert(
                    child
                        .strip_prefix(base)
                        .map_err(|_| err("tampered", "invalid bundle path"))?
                        .to_str()
                        .ok_or_else(|| err("invalid_path", "non-UTF-8 bundle path"))?
                        .to_string(),
                );
            } else {
                return Err(err("tampered", "special file in bundle"));
            }
        }
        Ok(())
    }
    if files.exists() {
        walk(&files, &files, &mut found)?;
    }
    let expected_files: BTreeSet<String> = expected
        .values()
        .filter(|e| e.state == "file")
        .map(|e| e.path.clone())
        .collect();
    if found != expected_files {
        return Err(err(
            "tampered",
            "bundle file inventory differs from manifest",
        ));
    }
    for (path, entry) in expected {
        match entry.state.as_str() {
            "file" => {
                let meta = source_meta(&files.join(&path))?;
                if meta.executable != entry.executable
                    || Some(hash_file(&files.join(&path))?) != entry.sha256
                {
                    return Err(err("tampered", format!("bundle file differs: {path}")));
                }
            }
            "tombstone" if entry.sha256.is_none() && !entry.executable => {}
            _ => return Err(err("invalid_bundle", "invalid manifest entry")),
        }
    }
    Ok(
        json!({"version": manifest.version, "bundle": bundle, "manifest": format!("manifest:{digest}"), "base_head": manifest.base_head, "files": manifest.files.len()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn git(root: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn final_revalidation_rejects_deterministic_file_and_tombstone_mutations() {
        let temp = std::env::temp_dir().join(format!(
            "fray-snapshot-unit-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = temp.join("repo");
        let out = temp.join("out");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir(&out).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        git(&root, &["config", "user.name", "Snapshot Test"]);
        fs::write(root.join("src/file"), b"before").unwrap();
        fs::write(root.join("src/deleted"), b"gone").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "initial"]);
        git(&root, &["rm", "-q", "src/deleted"]);
        let paths = [PathBuf::from("src")];
        let mut change_file = || fs::write(root.join("src/file"), b"after").unwrap();
        let failure = create_inner(&root, &out, &paths, Some(&mut change_file)).unwrap_err();
        assert_eq!(failure.code, "snapshot_changed");
        assert!(failure.message.contains("src/file changed after copy"));
        fs::write(root.join("src/file"), b"before").unwrap();
        let mut recreate_tombstone = || fs::write(root.join("src/deleted"), b"recreated").unwrap();
        let failure = create_inner(&root, &out, &paths, Some(&mut recreate_tombstone)).unwrap_err();
        assert_eq!(failure.code, "snapshot_changed");
        assert!(failure
            .message
            .contains("src/deleted was recreated after capture"));
        assert!(!fs::read_dir(&out).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".fray-snapshot-")
        }));
        fs::remove_dir_all(temp).unwrap();
    }
}
