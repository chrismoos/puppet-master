//! The diff and anchoring engine.
//!
//! Nothing here touches git or the filesystem: it works on the bytes a
//! capture returned, which is what lets a review compare two of its own
//! snapshots and follow a comment forward through edits.

use similar::{Algorithm, DiffOp, TextDiff};
use std::collections::BTreeMap;

pub use crate::review_repo::looks_binary;

// ---- diffing ----

/// One changed region, in the shape a unified diff header describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
}

/// Renders a unified diff for one file. `context` is the number of
/// unchanged lines kept around each change.
pub fn unified_diff(path: &str, old: &str, new: &str, context: usize) -> String {
    unified_diff_anchored(path, old, new, context, &[])
}

/// A unified diff that also keeps a window of context around each
/// anchored line of the new side.
///
/// A comment survives the change it asked for: once the edit lands the
/// file may differ from the base nowhere at all, and a diff of only
/// what changed then has nothing to hang the comment on. Anchors name
/// the lines that must be rendered whether or not anything there
/// changed.
pub fn unified_diff_anchored(
    path: &str,
    old: &str,
    new: &str,
    context: usize,
    anchors: &[u32],
) -> String {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_lines(old, new);
    let groups = diff.grouped_ops(context.max(1));

    let mut sections: Vec<Section> = groups
        .iter()
        .enumerate()
        .map(|(n, group)| {
            let (_, (ns, nl)) = group_ranges(group);
            Section {
                new_start: ns as u32 + 1,
                new_end: (ns + nl) as u32,
                group: Some(n),
            }
        })
        .collect();

    let new_lines: Vec<&str> = new.lines().collect();
    let window = context.max(1) as u32;
    for anchor in anchors {
        let anchor = (*anchor).clamp(1, new_lines.len().max(1) as u32);
        // A hunk that already covers the line renders it with its own
        // context, so there is nothing to add.
        if sections
            .iter()
            .any(|s| s.group.is_some() && anchor >= s.new_start && anchor <= s.new_end)
        {
            continue;
        }
        let mut lo = anchor.saturating_sub(window).max(1);
        let mut hi = (anchor + window).min(new_lines.len() as u32);
        // A window never reaches into a hunk, which would print those
        // lines a second time.
        for held in sections.iter().filter(|s| s.group.is_some()) {
            if held.new_end < anchor && held.new_end >= lo {
                lo = held.new_end + 1;
            }
            if held.new_start > anchor && held.new_start <= hi {
                hi = held.new_start - 1;
            }
        }
        if lo > hi {
            continue;
        }
        // Merging into a neighbour it already touches keeps one region
        // from being rendered as two headers over adjacent lines.
        match sections
            .iter_mut()
            .find(|s| s.group.is_none() && lo <= s.new_end + 1 && hi + 1 >= s.new_start)
        {
            Some(held) => {
                held.new_start = held.new_start.min(lo);
                held.new_end = held.new_end.max(hi);
            }
            None => sections.push(Section {
                new_start: lo,
                new_end: hi,
                group: None,
            }),
        }
    }
    if sections.is_empty() {
        return String::new();
    }
    sections.sort_by_key(|s| s.new_start);

    // Only a context-only section needs the reverse mapping, and it
    // costs a second diff, so it is not computed for the common case.
    let reverse = sections
        .iter()
        .any(|s| s.group.is_none())
        .then(|| hunks(old, new));

    let mut out = String::new();
    out.push_str(&format!("diff --git a/{path} b/{path}\n"));
    if !old.is_empty() && new.is_empty() {
        out.push_str("deleted file\n");
    }
    out.push_str(&format!("--- a/{path}\n"));
    out.push_str(&format!("+++ b/{path}\n"));
    for section in sections {
        let Some(n) = section.group else {
            let count = section.new_end - section.new_start + 1;
            let old_start = reverse
                .as_ref()
                .map(|h| old_line_for(section.new_start, h))
                .unwrap_or(section.new_start);
            out.push_str(&format!(
                "@@ -{old_start},{count} +{},{count} @@\n",
                section.new_start
            ));
            for line in section.new_start..=section.new_end {
                out.push(' ');
                out.push_str(new_lines[(line - 1) as usize]);
                out.push('\n');
            }
            continue;
        };
        let group = &groups[n];
        let (old_range, new_range) = group_ranges(group);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_range.0 + 1,
            old_range.1,
            new_range.0 + 1,
            new_range.1
        ));
        for op in group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                    similar::ChangeTag::Equal => ' ',
                };
                out.push(sign);
                out.push_str(change.value());
                if !change.value().ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// One rendered region of the new side. `group` names the diff group it
/// came from, or is absent when the region exists only to carry an
/// anchor.
struct Section {
    new_start: u32,
    new_end: u32,
    group: Option<usize>,
}

/// The old-side line matching a new-side line that no change touched.
/// The mirror of `remap_line`, which walks the other way.
fn old_line_for(new_line: u32, hunks: &[Hunk]) -> u32 {
    let mut delta: i64 = 0;
    for h in hunks {
        if new_line < h.new_start {
            break;
        }
        if new_line < h.new_start + h.new_lines.max(1) && h.new_lines > 0 {
            return h.old_start;
        }
        if new_line >= h.new_start + h.new_lines {
            delta += h.old_lines as i64 - h.new_lines as i64;
        }
    }
    (new_line as i64 + delta).max(1) as u32
}

fn group_ranges(group: &[DiffOp]) -> ((usize, usize), (usize, usize)) {
    let first = group
        .first()
        .expect("grouped_ops never yields an empty group");
    let last = group
        .last()
        .expect("grouped_ops never yields an empty group");
    let old_start = first.old_range().start;
    let new_start = first.new_range().start;
    (
        (old_start, last.old_range().end - old_start),
        (new_start, last.new_range().end - new_start),
    )
}

/// The hunks between two texts, with no context, which is what line
/// remapping needs.
pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_lines(old, new);
    diff.grouped_ops(0)
        .into_iter()
        .map(|group| {
            let ((os, ol), (ns, nl)) = group_ranges(&group);
            Hunk {
                old_start: os as u32 + 1,
                old_lines: ol as u32,
                new_start: ns as u32 + 1,
                new_lines: nl as u32,
            }
        })
        .collect()
}

/// Where a line ends up after the edits described by `hunks`.
///
/// A line before every change keeps its number; a line after them
/// shifts by the running delta; a line inside a changed region was
/// itself edited, which the reviewer needs to know because their
/// comment may no longer describe the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remap {
    Same(u32),
    Moved(u32),
    Changed(u32),
}

pub fn remap_line(line: u32, hunks: &[Hunk]) -> Remap {
    let mut delta: i64 = 0;
    for h in hunks {
        if line < h.old_start {
            break;
        }
        if line < h.old_start + h.old_lines.max(1) && h.old_lines > 0 {
            let landing = (h.new_start as i64).max(1) as u32;
            return Remap::Changed(landing);
        }
        if line >= h.old_start + h.old_lines {
            delta += h.new_lines as i64 - h.old_lines as i64;
        }
    }
    let mapped = (line as i64 + delta).max(1) as u32;
    if mapped == line {
        Remap::Same(mapped)
    } else {
        Remap::Moved(mapped)
    }
}

/// A numbered window of the current file around `line`, with a marker
/// on the anchored line, so an agent sees today's code beside the
/// original comment.
pub fn excerpt_around(text: &str, line: u32, context: u32) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let line = line.clamp(1, lines.len() as u32);
    let lo = line.saturating_sub(context).max(1);
    let hi = (line + context).min(lines.len() as u32);
    (lo..=hi)
        .map(|i| {
            let marker = if i == line { '>' } else { ' ' };
            format!("{marker} {i:>5}  {}", lines[(i - 1) as usize])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Diffs two stored manifests, rendering every file that differs. This
/// is how a revision comparison is produced without touching git.
///
/// Every path either side holds must appear on both, with an empty
/// entry where the file did not exist. A path only one side holds is
/// reported as unreadable rather than diffed against nothing.
pub fn diff_manifests(
    old: &BTreeMap<String, Vec<u8>>,
    new: &BTreeMap<String, Vec<u8>>,
    context: usize,
    anchors: &BTreeMap<String, Vec<u32>>,
) -> String {
    let mut paths: Vec<&String> = old.keys().chain(new.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut out = String::new();
    let none: Vec<u32> = Vec::new();
    for path in paths {
        let held = anchors.get(path).unwrap_or(&none);
        // A side with no entry for the path is a side nobody managed to
        // read. Treating it as an empty file would draw an ordinary
        // edit as a whole new file and say nothing about why, so a
        // caller that means "this file was not there" passes an empty
        // entry and absence is reported as what it is.
        let (a, b) = match (old.get(path), new.get(path)) {
            (Some(a), Some(b)) => (a, b),
            (a, _) => {
                let side = if a.is_none() { "base" } else { "working" };
                out.push_str(&format!("diff --git a/{path} b/{path}\n"));
                out.push_str(&format!("Unreadable: {side} side\n"));
                continue;
            }
        };
        // A file whose change was undone still belongs in the view when
        // a comment sits on it, or the reader loses the conversation
        // along with the diff.
        if a == b && held.is_empty() {
            continue;
        }
        if looks_binary(a) || looks_binary(b) {
            out.push_str(&format!("diff --git a/{path} b/{path}\n"));
            if a.is_empty() && !b.is_empty() {
                out.push_str("new file\n");
            } else if b.is_empty() && !a.is_empty() {
                out.push_str("deleted file\n");
            }
            out.push_str("Binary files differ\n");
            continue;
        }
        let chunk = unified_diff_anchored(
            path,
            &String::from_utf8_lossy(a),
            &String::from_utf8_lossy(b),
            context,
            held,
        );
        if chunk.is_empty() && !held.is_empty() {
            out.push_str(&format!("diff --git a/{path} b/{path}\n"));
            out.push_str("deleted file\n");
        } else {
            out.push_str(&chunk);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_before_every_change_keeps_its_number() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nb\nc\nd\nE\n";
        let h = hunks(old, new);
        assert_eq!(remap_line(2, &h), Remap::Same(2));
    }

    #[test]
    fn a_line_after_an_insertion_shifts_by_the_delta() {
        let old = "a\nb\nc\n";
        let new = "a\nNEW\nNEW2\nb\nc\n";
        let h = hunks(old, new);
        assert_eq!(remap_line(3, &h), Remap::Moved(5));
    }

    #[test]
    fn a_line_that_was_itself_edited_reports_changed() {
        let old = "a\nb\nc\n";
        let new = "a\nBBB\nc\n";
        let h = hunks(old, new);
        assert!(matches!(remap_line(2, &h), Remap::Changed(_)));
    }

    #[test]
    fn a_deletion_pulls_later_lines_up() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nd\n";
        let h = hunks(old, new);
        assert_eq!(remap_line(4, &h), Remap::Moved(2));
    }

    #[test]
    fn unified_diff_renders_a_header_and_both_sides() {
        let out = unified_diff("src/a.rs", "one\ntwo\n", "one\nTWO\n", 3);
        assert!(out.contains("--- a/src/a.rs"));
        assert!(out.contains("+++ b/src/a.rs"));
        assert!(out.contains("-two"));
        assert!(out.contains("+TWO"));
    }

    #[test]
    fn an_unchanged_file_renders_no_diff() {
        assert_eq!(unified_diff("a", "same\n", "same\n", 3), "");
    }

    #[test]
    fn excerpt_marks_the_anchored_line_and_clamps_to_the_file() {
        let text = "one\ntwo\nthree\n";
        let out = excerpt_around(text, 2, 1);
        assert!(out.contains(">     2  two"));
        assert!(out.contains("      1  one"));
        // Asking past the end lands on the last line rather than failing.
        assert!(excerpt_around(text, 99, 0).contains("three"));
    }

    #[test]
    fn a_nul_byte_marks_content_binary() {
        assert!(looks_binary(b"\x7fELF\0\0"));
        assert!(!looks_binary(b"plain text\n"));
    }

    #[test]
    fn manifests_diff_added_removed_and_changed_files() {
        let mut old = BTreeMap::new();
        old.insert("keep".to_string(), b"same\n".to_vec());
        old.insert("gone".to_string(), b"bye\n".to_vec());
        old.insert("edit".to_string(), b"before\n".to_vec());
        old.insert("added".to_string(), Vec::new());
        let mut new = BTreeMap::new();
        new.insert("keep".to_string(), b"same\n".to_vec());
        new.insert("edit".to_string(), b"after\n".to_vec());
        new.insert("added".to_string(), b"hello\n".to_vec());
        new.insert("gone".to_string(), Vec::new());

        let out = diff_manifests(&old, &new, 3, &BTreeMap::new());
        assert!(out.contains("a/edit"));
        assert!(out.contains("-before"));
        assert!(out.contains("+after"));
        assert!(out.contains("a/gone"));
        assert!(out.contains("a/added"));
        // An identical file contributes nothing.
        assert!(!out.contains("a/keep"));
    }

    #[test]
    fn a_side_that_is_missing_a_file_is_named_rather_than_drawn_as_empty() {
        let mut old = BTreeMap::new();
        old.insert("kept".to_string(), b"one\n".to_vec());
        let mut new = BTreeMap::new();
        new.insert("kept".to_string(), b"one\n".to_vec());
        new.insert("nobase".to_string(), b"one\ntwo\nthree\n".to_vec());

        let out = diff_manifests(&old, &new, 3, &BTreeMap::new());
        assert!(out.contains("diff --git a/nobase b/nobase"), "{out}");
        assert!(out.contains("Unreadable: base side"), "{out}");
        // The reader is never shown three invented additions.
        assert!(!out.contains("+one"), "{out}");
    }

    #[test]
    fn an_anchor_keeps_its_lines_in_a_file_that_stopped_changing() {
        let text = "one\ntwo\nthree\nfour\n";
        // Nothing differs, so an unanchored render says nothing at all.
        assert_eq!(unified_diff("f", text, text, 3), "");

        let out = unified_diff_anchored("f", text, text, 1, &[2]);
        assert!(out.contains("diff --git a/f b/f"), "{out}");
        assert!(out.contains("@@ -1,3 +1,3 @@"), "{out}");
        assert!(out.contains(" one"), "{out}");
        assert!(out.contains(" two"), "{out}");
        // Context only: there is genuinely no change to mark. Read
        // past the header, whose own lines start with + and -.
        let body = &out[out.find("@@").expect("a section header")..];
        assert!(!body.contains("\n+"), "{out}");
        assert!(!body.contains("\n-"), "{out}");
    }

    #[test]
    fn an_anchor_inside_a_rendered_hunk_adds_no_second_section() {
        let old = "one\ntwo\nthree\n";
        let new = "one\nTWO\nthree\n";
        let out = unified_diff_anchored("f", old, new, 3, &[2]);
        assert_eq!(out.matches("@@ -").count(), 1, "{out}");
    }

    #[test]
    fn an_anchored_section_numbers_the_old_side_from_the_edits_before_it() {
        // Two lines are inserted at the top, so line 6 of the new side
        // is line 4 of the old one.
        let old: String = (1..=8).map(|n| format!("l{n}\n")).collect();
        let new = format!("x\ny\n{old}");
        let out = unified_diff_anchored("f", &old, &new, 1, &[6]);
        assert!(out.contains("@@ -3,3 +5,3 @@"), "{out}");
        assert!(out.contains(" l4"), "{out}");
    }

    #[test]
    fn two_anchors_in_one_neighbourhood_render_as_one_section() {
        let text: String = (1..=20).map(|n| format!("l{n}\n")).collect();
        let out = unified_diff_anchored("f", &text, &text, 2, &[8, 10]);
        assert_eq!(out.matches("@@ -").count(), 1, "{out}");
        assert!(out.contains(" l6") && out.contains(" l12"), "{out}");
    }

    #[test]
    fn a_manifest_keeps_an_unchanged_file_only_when_a_comment_sits_on_it() {
        let mut side = BTreeMap::new();
        side.insert("quiet".to_string(), b"one\ntwo\n".to_vec());
        side.insert("commented".to_string(), b"one\ntwo\n".to_vec());

        let bare = diff_manifests(&side, &side, 3, &BTreeMap::new());
        assert_eq!(bare, "", "nothing changed and nothing was said");

        let mut anchors = BTreeMap::new();
        anchors.insert("commented".to_string(), vec![1]);
        let out = diff_manifests(&side, &side, 3, &anchors);
        assert!(out.contains("a/commented"), "{out}");
        assert!(!out.contains("a/quiet"), "{out}");
    }

    #[test]
    fn a_commented_file_that_does_not_exist_on_either_side_is_rendered_as_deleted() {
        let mut old = BTreeMap::new();
        let mut new = BTreeMap::new();
        old.insert("brand-new.rs".to_string(), Vec::new());
        new.insert("brand-new.rs".to_string(), Vec::new());

        let mut anchors = BTreeMap::new();
        anchors.insert("brand-new.rs".to_string(), vec![5]);

        let out = diff_manifests(&old, &new, 3, &anchors);
        assert!(
            out.contains("diff --git a/brand-new.rs b/brand-new.rs"),
            "{out}"
        );
        assert!(out.contains("deleted file"), "{out}");
    }
}
