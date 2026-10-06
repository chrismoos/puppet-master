//! Reading a repository for a review.
//!
//! This is the only place that touches git, and it runs on whichever
//! host holds the files. The controller reaches it through the same
//! encoded request either way: in-process for the local worker, over
//! the worker plane for a remote one. There is no second
//! implementation for the local case, so the two cannot drift.

use std::collections::BTreeSet;
use std::path::Path;

/// Untracked files above this are named as skipped rather than read,
/// so a stray core dump cannot flood a review.
pub const UNTRACKED_MAX_BYTES: u64 = 512 * 1024;
pub const UNTRACKED_IMAGE_MAX_BYTES: u64 = 10 * 1024 * 1024;

pub fn is_image_path(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    matches!(
        ext.as_deref(),
        Some("svg" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "avif")
    )
}

/// What the controller asks a host to do with a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoOp {
    /// Everything a snapshot needs in one round trip: the file set and
    /// the content of each file, minus whatever the controller already
    /// stores.
    Capture(CaptureOp),
    /// One file's bytes, for rendering a document rather than hunks.
    ReadFile {
        worktree: String,
        /// Empty reads the working tree.
        rev: String,
        path: String,
    },
    /// A revision to a SHA. The MCP contract requires resolved SHAs, so
    /// this exists for the CLI, which cannot resolve a ref against a
    /// worktree on another host.
    Resolve { worktree: String, rev: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureOp {
    pub worktree: String,
    /// Resolved SHA of the base side.
    pub base: String,
    /// Resolved SHA of the head side; empty captures the working tree.
    pub head: String,
    pub pathspec: Vec<String>,
    /// When set, the scope is exactly these paths and no enumeration
    /// runs. This is how an agent reviews something git would not list.
    pub files: Option<Vec<String>>,
    /// Blob SHAs the controller already holds. Their bytes are omitted
    /// from the answer, which is what keeps repeat snapshots cheap:
    /// most files do not change between rounds.
    pub known: BTreeSet<String>,
    /// Reads the base side instead of the head side. A review's base is
    /// immutable, so this is captured once and never re-read.
    pub at_base: bool,
    /// Paths to read whether or not enumeration lists them. A review
    /// keeps every file it holds a comment on, because the edit the
    /// comment asked for can stop that file differing at all. The base
    /// side has the same need in the other direction and meets it with
    /// a capture scoped to `files`, since a file that starts differing
    /// after the review opened is absent from a base already taken.
    pub extra: Vec<String>,
}

/// One file in a capture. `content` is absent when the controller said
/// it already had this SHA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoBlob {
    pub path: String,
    pub sha: String,
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureAnswer {
    pub files: Vec<RepoBlob>,
    /// Files present in the working tree but not tracked.
    pub untracked: Vec<String>,
    /// Path and why it was left out.
    pub skipped: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoAnswer {
    Capture(CaptureAnswer),
    ReadFile(Option<Vec<u8>>),
    Resolve(Option<String>),
}

pub type RepoResult = std::result::Result<RepoAnswer, String>;

/// Runs one operation against a repository on this host.
pub fn handle(op: &RepoOp) -> RepoResult {
    match op {
        RepoOp::Capture(c) => capture(c).map(RepoAnswer::Capture),
        RepoOp::ReadFile {
            worktree,
            rev,
            path,
        } => read_file(worktree, rev, path).map(RepoAnswer::ReadFile),
        RepoOp::Resolve { worktree, rev } => resolve(worktree, rev).map(RepoAnswer::Resolve),
    }
}

fn open(worktree: &str) -> std::result::Result<gix::Repository, String> {
    gix::open(worktree).map_err(|e| format!("{worktree}: {e}"))
}

pub fn blob_sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// A NUL byte early in the file is what git itself treats as binary.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

fn resolve(worktree: &str, rev: &str) -> std::result::Result<Option<String>, String> {
    let repo = open(worktree)?;
    Ok(repo
        .rev_parse_single(rev)
        .ok()
        .map(|id| id.detach().to_string()))
}

/// Bytes of `path` at `rev`, or from the working tree when `rev` is
/// empty.
fn read_file(
    worktree: &str,
    rev: &str,
    path: &str,
) -> std::result::Result<Option<Vec<u8>>, String> {
    if rev.is_empty() {
        let full = Path::new(worktree).join(path);
        return Ok(std::fs::read(full).ok());
    }
    let repo = open(worktree)?;
    blob_at(&repo, rev, path)
}

/// Bytes of `path` in the tree at `rev`. `Ok(None)` means the tree
/// does not hold that path, which is a file that was added after
/// `rev`; an error means the read itself failed and nothing is known
/// about the path.
fn blob_at(
    repo: &gix::Repository,
    rev: &str,
    path: &str,
) -> std::result::Result<Option<Vec<u8>>, String> {
    let tree = repo
        .rev_parse_single(rev)
        .map_err(|e| format!("{rev}: {e}"))?
        .object()
        .map_err(|e| format!("{rev}: {e}"))?
        .peel_to_tree()
        .map_err(|e| format!("{rev}: {e}"))?;
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .map_err(|e| format!("{rev}: {e}"))?
    else {
        return Ok(None);
    };
    Ok(Some(
        entry
            .object()
            .map_err(|e| format!("{rev}: {e}"))?
            .detach()
            .data,
    ))
}

fn capture(c: &CaptureOp) -> std::result::Result<CaptureAnswer, String> {
    let read_rev = if c.at_base { &c.base } else { &c.head };
    // An explicit file set read from the working tree needs no
    // repository, which is what lets a plain document be reviewed
    // outside any checkout.
    let needs_repo = c.files.is_none() || !read_rev.is_empty();
    let repo = if needs_repo {
        Some(open(&c.worktree)?)
    } else {
        None
    };
    let mut answer = CaptureAnswer::default();

    let (mut paths, untracked) = match (&c.files, &repo) {
        // An explicit file set is the scope, exactly. Nothing is
        // enumerated, so nothing the agent excluded creeps back in.
        (Some(files), _) => (files.clone(), Vec::new()),
        (None, Some(repo)) => enumerate(repo, c)?,
        (None, None) => (Vec::new(), Vec::new()),
    };
    // Untracked files are part of the review, so their content is read
    // like any other. Reading them is also what applies the size and
    // binary guards.
    paths.extend(untracked.iter().cloned());
    answer.untracked = untracked;
    for path in &c.extra {
        if !paths.iter().any(|p| p == path) {
            paths.push(path.clone());
        }
    }

    for path in paths {
        let bytes = if read_rev.is_empty() {
            match read_worktree_file(&c.worktree, &path) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => Vec::new(),
                Err(reason) => {
                    answer.skipped.push((path, reason));
                    continue;
                }
            }
        } else {
            match repo.as_ref().map(|repo| blob_at(repo, read_rev, &path)) {
                Some(Ok(Some(bytes))) => bytes,
                // A path the revision does not hold was added after it,
                // which is a legitimate empty side.
                Some(Ok(None)) | None => Vec::new(),
                // A read that failed says nothing about the file, so it
                // is named rather than passed off as an empty side that
                // would draw the whole file as added.
                Some(Err(reason)) => {
                    tracing::warn!(%path, rev = %read_rev, %reason, "review could not read a file at a revision");
                    answer.skipped.push((path, reason));
                    continue;
                }
            }
        };
        let sha = blob_sha(&bytes);
        let content = if c.known.contains(&sha) {
            None
        } else {
            Some(bytes)
        };
        answer.files.push(RepoBlob { path, sha, content });
    }
    answer.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(answer)
}

fn read_worktree_file(worktree: &str, path: &str) -> std::result::Result<Option<Vec<u8>>, String> {
    let full = Path::new(worktree).join(path);
    let meta = match std::fs::metadata(&full) {
        Ok(m) => m,
        // A path in the diff that is gone from the tree was deleted,
        // which is a legitimate empty right-hand side.
        Err(_) => return Ok(None),
    };
    if meta.len() > UNTRACKED_MAX_BYTES {
        let max_bytes = if is_image_path(path) {
            UNTRACKED_IMAGE_MAX_BYTES
        } else {
            UNTRACKED_MAX_BYTES
        };
        if meta.len() > max_bytes {
            return Err(format!(
                "{} bytes exceeds the {max_bytes} byte limit",
                meta.len()
            ));
        }
    }
    match std::fs::read(&full) {
        Ok(bytes) if looks_binary(&bytes) && !is_image_path(path) => Err("binary".into()),
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) => Err(format!("unreadable: {e}")),
    }
}

/// The paths a review covers: what changed between the two sides, plus
/// untracked files when the head side is the working tree.
fn enumerate(
    repo: &gix::Repository,
    c: &CaptureOp,
) -> std::result::Result<(Vec<String>, Vec<String>), String> {
    let mut changed: BTreeSet<String> = BTreeSet::new();
    let mut untracked: Vec<String> = Vec::new();

    let base_tree = repo
        .rev_parse_single(c.base.as_str())
        .map_err(|e| format!("base {}: {e}", c.base))?
        .object()
        .map_err(|e| e.to_string())?
        .peel_to_tree()
        .map_err(|e| e.to_string())?;

    if c.head.is_empty() {
        // Against the working tree: git's own status, which already
        // knows about the index, ignores, and untracked files.
        let status = repo
            .status(gix::progress::Discard)
            .map_err(|e| e.to_string())?
            .index_worktree_options_mut(|opts| {
                opts.dirwalk_options
                    .as_mut()
                    .map(|d| d.set_emit_untracked(gix::dir::walk::EmissionMode::Matching));
            })
            .into_iter(None)
            .map_err(|e| e.to_string())?;
        for item in status {
            let item = item.map_err(|e| e.to_string())?;
            let path = item.location().to_string();
            match &item {
                gix::status::Item::IndexWorktree(
                    gix::status::index_worktree::Item::DirectoryContents { .. },
                ) => untracked.push(path),
                _ => {
                    changed.insert(path);
                }
            }
        }
        // Status compares the index to the worktree and HEAD, which is
        // not the same as comparing to an arbitrary base, so add what
        // differs between the base tree and HEAD as well.
        if let Ok(head) = repo.head_commit() {
            if let Ok(head_tree) = head.tree() {
                collect_tree_changes(repo, &base_tree, &head_tree, &mut changed)?;
            }
        }
    } else {
        let head_tree = repo
            .rev_parse_single(c.head.as_str())
            .map_err(|e| format!("head {}: {e}", c.head))?
            .object()
            .map_err(|e| e.to_string())?
            .peel_to_tree()
            .map_err(|e| e.to_string())?;
        collect_tree_changes(repo, &base_tree, &head_tree, &mut changed)?;
    }

    let keep = |p: &String| c.pathspec.is_empty() || c.pathspec.iter().any(|s| matches_spec(p, s));
    untracked.retain(&keep);
    let changed: Vec<String> = changed.into_iter().filter(|p| keep(p)).collect();
    untracked.sort();
    Ok((changed, untracked))
}

fn collect_tree_changes(
    repo: &gix::Repository,
    from: &gix::Tree<'_>,
    to: &gix::Tree<'_>,
    out: &mut BTreeSet<String>,
) -> std::result::Result<(), String> {
    let _ = repo;
    let mut changes = from.changes().map_err(|e| e.to_string())?;
    changes
        .for_each_to_obtain_tree(to, |change| {
            // The walk reports the directories it descends through as
            // well as the files in them.
            if change.entry_mode().is_blob_or_symlink() {
                out.insert(change.location().to_string());
            }
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Git pathspec matching, reduced to the two forms a review actually
/// uses: a directory or path prefix, and a trailing-glob suffix. Full
/// pathspec magic is deliberately out of scope.
pub fn matches_spec(path: &str, spec: &str) -> bool {
    let spec = spec.trim_end_matches('/');
    if spec.is_empty() {
        return true;
    }
    if let Some(prefix) = spec.strip_suffix("/**") {
        return path.starts_with(&format!("{prefix}/"));
    }
    if let Some(ext) = spec.rsplit_once("*.").map(|(_, e)| e) {
        if spec.contains('*') {
            return path.ends_with(&format!(".{ext}"));
        }
    }
    path == spec || path.starts_with(&format!("{spec}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_spec_matches_its_contents_and_nothing_else() {
        assert!(matches_spec("crates/pm/src/main.rs", "crates"));
        assert!(matches_spec("crates/pm/src/main.rs", "crates/pm"));
        assert!(!matches_spec("web/src/App.tsx", "crates"));
        // A prefix that is not a path boundary must not match.
        assert!(!matches_spec("cratesfoo/x.rs", "crates"));
    }

    #[test]
    fn an_exact_path_matches_only_itself() {
        assert!(matches_spec("README.md", "README.md"));
        assert!(!matches_spec("README.md.bak", "README.md"));
    }

    #[test]
    fn a_suffix_glob_matches_by_extension() {
        assert!(matches_spec("src/deep/main.rs", "src/**/*.rs"));
        assert!(!matches_spec("src/deep/main.ts", "src/**/*.rs"));
    }

    #[test]
    fn an_empty_spec_matches_everything() {
        assert!(matches_spec("anything", ""));
    }

    #[test]
    fn a_nul_byte_marks_content_binary() {
        assert!(looks_binary(b"\x7fELF\0\0"));
        assert!(!looks_binary(b"plain text\n"));
    }
}

#[cfg(test)]
mod repo_tests {
    use super::*;
    use std::process::Command;

    fn git(args: &[&str], cwd: &Path) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo() -> (tempfile::TempDir, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        git(&["init", "-q", "-b", "master"], &root);
        git(&["config", "user.email", "r@t"], &root);
        git(&["config", "user.name", "r"], &root);
        git(&["config", "commit.gpgsign", "false"], &root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "one\ntwo\n").unwrap();
        std::fs::write(root.join("keep.txt"), "unchanged\n").unwrap();
        git(&["add", "-A"], &root);
        git(&["commit", "-qm", "base"], &root);
        let base = String::from_utf8_lossy(
            &Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        (tmp, root.display().to_string(), base)
    }

    #[test]
    fn resolve_turns_a_ref_into_a_sha_and_reports_an_unknown_one() {
        let (_tmp, root, base) = repo();
        let RepoAnswer::Resolve(sha) = handle(&RepoOp::Resolve {
            worktree: root.clone(),
            rev: "HEAD".into(),
        })
        .unwrap() else {
            panic!("wrong answer");
        };
        assert_eq!(sha.unwrap(), base);

        let RepoAnswer::Resolve(missing) = handle(&RepoOp::Resolve {
            worktree: root,
            rev: "no-such-ref".into(),
        })
        .unwrap() else {
            panic!("wrong answer");
        };
        assert!(missing.is_none());
    }

    #[test]
    fn capture_against_the_working_tree_finds_edits_and_untracked_files() {
        let (_tmp, root, base) = repo();
        std::fs::write(Path::new(&root).join("src/lib.rs"), "one\nTWO\n").unwrap();
        std::fs::write(Path::new(&root).join("src/new.rs"), "brand new\n").unwrap();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base,
            head: String::new(),
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };

        let paths: Vec<&str> = answer.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/lib.rs"), "{paths:?}");
        // New work is visible rather than silently missing.
        assert!(
            answer.untracked.contains(&"src/new.rs".to_string()),
            "{answer:?}"
        );
        // A file nobody touched is not part of the review.
        assert!(!paths.contains(&"keep.txt"), "{paths:?}");
    }

    #[test]
    fn capture_omits_bytes_the_controller_already_holds() {
        let (_tmp, root, base) = repo();
        std::fs::write(Path::new(&root).join("src/lib.rs"), "one\nTWO\n").unwrap();

        let first = match handle(&RepoOp::Capture(CaptureOp {
            worktree: root.clone(),
            base: base.clone(),
            head: String::new(),
            ..Default::default()
        }))
        .unwrap()
        {
            RepoAnswer::Capture(a) => a,
            _ => panic!("wrong answer"),
        };
        assert!(first.files.iter().all(|f| f.content.is_some()));

        // Repeating the capture with the SHAs already stored sends the
        // manifest but none of the bytes, which is what keeps a
        // remote review cheap round after round.
        let known: BTreeSet<String> = first.files.iter().map(|f| f.sha.clone()).collect();
        let second = match handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base,
            head: String::new(),
            known,
            ..Default::default()
        }))
        .unwrap()
        {
            RepoAnswer::Capture(a) => a,
            _ => panic!("wrong answer"),
        };
        assert_eq!(second.files.len(), first.files.len());
        assert!(
            second.files.iter().all(|f| f.content.is_none()),
            "{second:?}"
        );
        // The manifest still names every file and its SHA.
        assert_eq!(
            second.files.iter().map(|f| &f.sha).collect::<Vec<_>>(),
            first.files.iter().map(|f| &f.sha).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn an_explicit_file_set_is_the_scope_exactly() {
        let (_tmp, root, base) = repo();
        std::fs::write(Path::new(&root).join("src/lib.rs"), "one\nTWO\n").unwrap();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base,
            head: String::new(),
            // A file git would not list, because nothing changed in it.
            files: Some(vec!["keep.txt".into()]),
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };
        assert_eq!(answer.files.len(), 1);
        assert_eq!(answer.files[0].path, "keep.txt");
        assert!(answer.untracked.is_empty());
    }

    #[test]
    fn capture_at_base_reads_the_immutable_side() {
        let (_tmp, root, base) = repo();
        std::fs::write(Path::new(&root).join("src/lib.rs"), "one\nTWO\n").unwrap();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base,
            head: String::new(),
            files: Some(vec!["src/lib.rs".into()]),
            at_base: true,
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };
        // The base side still has the original content, whatever the
        // working tree now says.
        assert_eq!(answer.files[0].content.as_deref(), Some(&b"one\ntwo\n"[..]));
    }

    #[test]
    fn a_file_missing_from_the_base_reads_as_empty_rather_than_as_a_failure() {
        let (_tmp, root, base) = repo();
        std::fs::write(Path::new(&root).join("src/added.rs"), "brand new\n").unwrap();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base,
            head: String::new(),
            files: Some(vec!["src/added.rs".into()]),
            at_base: true,
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };
        // The base simply does not hold it, which is a real empty side.
        assert!(answer.skipped.is_empty(), "{answer:?}");
        assert_eq!(answer.files[0].content.as_deref(), Some(&b""[..]));
    }

    #[test]
    fn a_base_read_that_fails_is_named_rather_than_passed_off_as_empty() {
        let (_tmp, root, base) = repo();
        // The blob is gone from the object store, so nothing can be
        // said about what the file held at the base.
        let sha = String::from_utf8_lossy(
            &Command::new("git")
                .args(["rev-parse", &format!("{base}:src/lib.rs")])
                .current_dir(&root)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        let (dir, rest) = sha.split_at(2);
        std::fs::remove_file(Path::new(&root).join(".git/objects").join(dir).join(rest)).unwrap();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root.clone(),
            base: base.clone(),
            head: String::new(),
            files: Some(vec!["src/lib.rs".into()]),
            at_base: true,
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };
        // Not an empty file, which would draw the whole thing as added.
        assert!(answer.files.is_empty(), "{answer:?}");
        assert_eq!(answer.skipped.len(), 1, "{answer:?}");
        assert_eq!(answer.skipped[0].0, "src/lib.rs");

        // The same read through the one-file path reports it too.
        let err = handle(&RepoOp::ReadFile {
            worktree: root,
            rev: base,
            path: "src/lib.rs".into(),
        })
        .unwrap_err();
        assert!(!err.is_empty(), "{err}");
    }

    #[test]
    fn a_base_that_does_not_resolve_is_named_rather_than_read_as_empty() {
        let (_tmp, root, _base) = repo();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base: "0000000000000000000000000000000000000000".into(),
            head: String::new(),
            files: Some(vec!["src/lib.rs".into()]),
            at_base: true,
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };
        assert!(answer.files.is_empty(), "{answer:?}");
        assert_eq!(answer.skipped.len(), 1, "{answer:?}");
    }

    #[test]
    fn an_oversized_or_binary_file_is_skipped_with_a_reason() {
        let (_tmp, root, base) = repo();
        std::fs::write(
            Path::new(&root).join("huge.bin"),
            "x".repeat(UNTRACKED_MAX_BYTES as usize + 1),
        )
        .unwrap();
        std::fs::write(Path::new(&root).join("bin.dat"), b"a\0b").unwrap();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base,
            head: String::new(),
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };
        let skipped: Vec<&str> = answer.skipped.iter().map(|(p, _)| p.as_str()).collect();
        assert!(skipped.contains(&"huge.bin"), "{answer:?}");
        assert!(skipped.contains(&"bin.dat"), "{answer:?}");
        assert!(answer
            .skipped
            .iter()
            .any(|(p, r)| p == "huge.bin" && r.contains("exceeds")));
    }

    #[test]
    fn a_frozen_range_compares_two_commits_and_ignores_the_working_tree() {
        let (_tmp, root, base) = repo();
        let rootp = Path::new(&root);
        std::fs::write(rootp.join("src/lib.rs"), "one\nTWO\n").unwrap();
        git(&["commit", "-aqm", "second"], rootp);
        let head = String::from_utf8_lossy(
            &Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(rootp)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        // An uncommitted edit that must not appear in a frozen range.
        std::fs::write(rootp.join("src/lib.rs"), "one\nTHREE\n").unwrap();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base,
            head,
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };
        assert_eq!(answer.files.len(), 1);
        assert_eq!(answer.files[0].content.as_deref(), Some(&b"one\nTWO\n"[..]));
        assert!(answer.untracked.is_empty());
    }

    #[test]
    fn a_pathspec_narrows_both_changed_and_untracked_files() {
        let (_tmp, root, base) = repo();
        let rootp = Path::new(&root);
        std::fs::write(rootp.join("src/lib.rs"), "one\nTWO\n").unwrap();
        std::fs::write(rootp.join("keep.txt"), "edited\n").unwrap();
        std::fs::write(rootp.join("src/new.rs"), "new\n").unwrap();
        std::fs::write(rootp.join("other.txt"), "new too\n").unwrap();

        let RepoAnswer::Capture(answer) = handle(&RepoOp::Capture(CaptureOp {
            worktree: root,
            base,
            head: String::new(),
            pathspec: vec!["src".into()],
            ..Default::default()
        }))
        .unwrap() else {
            panic!("wrong answer");
        };
        let paths: Vec<&str> = answer.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/lib.rs"), "{paths:?}");
        assert!(!paths.contains(&"keep.txt"), "{paths:?}");
        assert_eq!(answer.untracked, vec!["src/new.rs".to_string()]);
    }
}

// ---- wire form ----
//
// The op and its answer travel as encoded protobuf. The controller
// encodes once and either hands the bytes to this module in-process or
// ships them to a worker, so both paths carry byte-identical requests.

use pm_protocol::wire;
use prost::Message;

impl RepoOp {
    pub fn encode(&self) -> Vec<u8> {
        let op = match self {
            RepoOp::Capture(c) => wire::repo_op::Op::Capture(wire::RepoCapture {
                worktree: c.worktree.clone(),
                base: c.base.clone(),
                head: c.head.clone(),
                pathspec: c.pathspec.clone(),
                files: c.files.clone().unwrap_or_default(),
                files_given: c.files.is_some(),
                known: c.known.iter().cloned().collect(),
                at_base: c.at_base,
                extra: c.extra.clone(),
            }),
            RepoOp::ReadFile {
                worktree,
                rev,
                path,
            } => wire::repo_op::Op::ReadFile(wire::RepoReadFile {
                worktree: worktree.clone(),
                rev: rev.clone(),
                path: path.clone(),
            }),
            RepoOp::Resolve { worktree, rev } => wire::repo_op::Op::Resolve(wire::RepoResolve {
                worktree: worktree.clone(),
                rev: rev.clone(),
            }),
        };
        wire::RepoOp { op: Some(op) }.encode_to_vec()
    }

    pub fn decode(bytes: &[u8]) -> std::result::Result<Self, String> {
        let op = wire::RepoOp::decode(bytes).map_err(|e| e.to_string())?;
        match op.op.ok_or("empty repo op")? {
            wire::repo_op::Op::Capture(c) => Ok(RepoOp::Capture(CaptureOp {
                worktree: c.worktree,
                base: c.base,
                head: c.head,
                pathspec: c.pathspec,
                files: c.files_given.then_some(c.files),
                known: c.known.into_iter().collect(),
                at_base: c.at_base,
                extra: c.extra,
            })),
            wire::repo_op::Op::ReadFile(r) => Ok(RepoOp::ReadFile {
                worktree: r.worktree,
                rev: r.rev,
                path: r.path,
            }),
            wire::repo_op::Op::Resolve(r) => Ok(RepoOp::Resolve {
                worktree: r.worktree,
                rev: r.rev,
            }),
        }
    }
}

impl RepoAnswer {
    pub fn encode(&self) -> Vec<u8> {
        let answer = match self {
            RepoAnswer::Capture(a) => wire::repo_answer::Answer::Capture(wire::RepoCaptureAnswer {
                files: a
                    .files
                    .iter()
                    .map(|f| wire::RepoBlob {
                        path: f.path.clone(),
                        sha: f.sha.clone(),
                        content: f.content.clone().unwrap_or_default(),
                        has_content: f.content.is_some(),
                    })
                    .collect(),
                untracked: a.untracked.clone(),
                skipped: a
                    .skipped
                    .iter()
                    .map(|(path, reason)| wire::RepoSkipped {
                        path: path.clone(),
                        reason: reason.clone(),
                    })
                    .collect(),
            }),
            RepoAnswer::ReadFile(content) => {
                wire::repo_answer::Answer::ReadFile(wire::RepoReadFileAnswer {
                    content: content.clone().unwrap_or_default(),
                    found: content.is_some(),
                })
            }
            RepoAnswer::Resolve(sha) => {
                wire::repo_answer::Answer::Resolve(wire::RepoResolveAnswer {
                    sha: sha.clone().unwrap_or_default(),
                    found: sha.is_some(),
                })
            }
        };
        wire::RepoAnswer {
            answer: Some(answer),
        }
        .encode_to_vec()
    }

    pub fn decode(bytes: &[u8]) -> std::result::Result<Self, String> {
        let answer = wire::RepoAnswer::decode(bytes).map_err(|e| e.to_string())?;
        match answer.answer.ok_or("empty repo answer")? {
            wire::repo_answer::Answer::Capture(a) => Ok(RepoAnswer::Capture(CaptureAnswer {
                files: a
                    .files
                    .into_iter()
                    .map(|f| RepoBlob {
                        path: f.path,
                        sha: f.sha,
                        content: f.has_content.then(|| f.content.to_vec()),
                    })
                    .collect(),
                untracked: a.untracked,
                skipped: a.skipped.into_iter().map(|s| (s.path, s.reason)).collect(),
            })),
            wire::repo_answer::Answer::ReadFile(r) => {
                Ok(RepoAnswer::ReadFile(r.found.then(|| r.content.to_vec())))
            }
            wire::repo_answer::Answer::Resolve(r) => {
                Ok(RepoAnswer::Resolve(r.found.then_some(r.sha)))
            }
        }
    }
}

/// Runs an encoded op and returns an encoded answer. This is what a
/// worker executes, and what the controller calls directly for its own
/// worker, so neither side has a path the other lacks.
pub fn handle_encoded(bytes: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let op = RepoOp::decode(bytes)?;
    handle(&op).map(|answer| answer.encode())
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn an_op_survives_a_round_trip_through_the_wire() {
        for op in [
            RepoOp::Capture(CaptureOp {
                worktree: "/repo".into(),
                base: "abc".into(),
                head: String::new(),
                pathspec: vec!["src".into()],
                extra: vec!["docs/only-commented.md".into()],
                files: Some(vec!["a.rs".into()]),
                known: ["sha1".to_string()].into_iter().collect(),
                at_base: true,
            }),
            // An absent file set must stay absent, because "no files
            // given" and "an empty scope" mean opposite things.
            RepoOp::Capture(CaptureOp {
                worktree: "/repo".into(),
                base: "abc".into(),
                ..Default::default()
            }),
            RepoOp::ReadFile {
                worktree: "/repo".into(),
                rev: "abc".into(),
                path: "a.rs".into(),
            },
            RepoOp::Resolve {
                worktree: "/repo".into(),
                rev: "HEAD".into(),
            },
        ] {
            assert_eq!(RepoOp::decode(&op.encode()).unwrap(), op);
        }
    }

    #[test]
    fn an_answer_survives_a_round_trip_and_keeps_absent_content_absent() {
        let answer = RepoAnswer::Capture(CaptureAnswer {
            files: vec![
                RepoBlob {
                    path: "a.rs".into(),
                    sha: "s1".into(),
                    content: Some(b"hello".to_vec()),
                },
                // The caller already had this one, so no bytes rode
                // along. That distinction is the whole point of the
                // dedup, so it must survive encoding.
                RepoBlob {
                    path: "b.rs".into(),
                    sha: "s2".into(),
                    content: None,
                },
            ],
            untracked: vec!["c.rs".into()],
            skipped: vec![("huge.bin".into(), "too big".into())],
        });
        assert_eq!(RepoAnswer::decode(&answer.encode()).unwrap(), answer);
    }

    #[test]
    fn a_missing_file_and_an_empty_file_stay_distinguishable() {
        let missing = RepoAnswer::ReadFile(None);
        let empty = RepoAnswer::ReadFile(Some(Vec::new()));
        assert_eq!(RepoAnswer::decode(&missing.encode()).unwrap(), missing);
        assert_eq!(RepoAnswer::decode(&empty.encode()).unwrap(), empty);
        assert_ne!(missing.encode(), empty.encode());
    }

    #[test]
    fn an_unresolvable_ref_stays_distinct_from_an_empty_sha() {
        let none = RepoAnswer::Resolve(None);
        assert_eq!(RepoAnswer::decode(&none.encode()).unwrap(), none);
    }

    #[test]
    fn garbage_bytes_are_refused_rather_than_guessed_at() {
        assert!(RepoOp::decode(b"not a proto at all \xff\xfe").is_err());
        assert!(handle_encoded(b"\xff\xfe\xfd").is_err());
    }
}
