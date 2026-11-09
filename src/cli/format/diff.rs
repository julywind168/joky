//! Unified previews use matched lines to separate distant edits into hunks.
use std::{collections::HashMap, fmt::Write, ops::Range};

#[derive(Default)]
struct Occurrences {
    old_count: usize,
    new_count: usize,
    new_index: usize,
}

struct Edit {
    old: Range<usize>,
    new: Range<usize>,
}

pub(super) fn unified(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }
    let old: Vec<_> = before.split_inclusive('\n').collect();
    let new: Vec<_> = after.split_inclusive('\n').collect();
    let matches = matching_lines(&old, &new);
    let mut edits = Vec::new();
    let (mut a, mut b) = (0, 0);
    for (i, j) in matches
        .into_iter()
        .chain(std::iter::once((old.len(), new.len())))
    {
        if a < i || b < j {
            edits.push(Edit {
                old: a..i,
                new: b..j,
            });
        }
        a = i + 1;
        b = j + 1;
    }
    let mut output = format!(
        "--- {}\n+++ {}\n",
        quote(&format!("a/{path}")),
        quote(&format!("b/{path}"))
    );
    let mut first = 0;
    while first < edits.len() {
        let mut last = first;
        while last + 1 < edits.len() && edits[last + 1].old.start - edits[last].old.end <= 6 {
            last += 1;
        }
        let leading = edits[first].old.start.min(3);
        let start = edits[first].old.start - leading;
        let new_start = edits[first].new.start - leading;
        let trailing = (old.len() - edits[last].old.end).min(3);
        let end = edits[last].old.end + trailing;
        let new_end = edits[last].new.end + trailing;
        writeln!(
            output,
            "@@ -{} +{} @@",
            range(start, end - start),
            range(new_start, new_end - new_start)
        )
        .unwrap();
        let mut cursor = start;
        for edit in &edits[first..=last] {
            for line in &old[cursor..edit.old.start] {
                emit(&mut output, ' ', line);
            }
            for line in &old[edit.old.clone()] {
                emit(&mut output, '-', line);
            }
            for line in &new[edit.new.clone()] {
                emit(&mut output, '+', line);
            }
            cursor = edit.old.end;
        }
        for line in &old[cursor..end] {
            emit(&mut output, ' ', line);
        }
        first = last + 1;
    }
    output
}

// Patience anchors keep memory linear. Strip shared edges in each region, then
// split at a longest increasing sequence of lines unique on both sides. A scan
// budget bounds pathological inputs; unmatched regions remain valid replacements.
fn matching_lines(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let mut pending = vec![(0..old.len(), 0..new.len())];
    let mut matches = Vec::new();
    let mut budget = (old.len() + new.len()).saturating_mul(16);
    while let Some((mut a, mut b)) = pending.pop() {
        while !a.is_empty() && !b.is_empty() && old[a.start] == new[b.start] {
            matches.push((a.start, b.start));
            a.start += 1;
            b.start += 1;
        }
        while !a.is_empty() && !b.is_empty() && old[a.end - 1] == new[b.end - 1] {
            a.end -= 1;
            b.end -= 1;
            matches.push((a.end, b.end));
        }
        if a.is_empty() || b.is_empty() || a.len() + b.len() > budget {
            continue;
        }
        budget -= a.len() + b.len();
        let mut positions = HashMap::<&str, Occurrences>::new();
        for i in a.clone() {
            positions.entry(old[i]).or_default().old_count += 1;
        }
        for j in b.clone() {
            if let Some(entry) = positions.get_mut(new[j]) {
                entry.new_index = j;
                entry.new_count += 1;
            }
        }
        let candidates: Vec<_> = a
            .clone()
            .filter_map(|i| {
                let entry = &positions[old[i]];
                (entry.old_count == 1 && entry.new_count == 1).then_some((i, entry.new_index))
            })
            .collect();
        let mut tails: Vec<usize> = Vec::new();
        let mut previous = vec![None; candidates.len()];
        for (index, &(_, j)) in candidates.iter().enumerate() {
            let slot = tails.partition_point(|&t| candidates[t].1 < j);
            previous[index] = slot.checked_sub(1).map(|s| tails[s]);
            if slot == tails.len() {
                tails.push(index);
            } else {
                tails[slot] = index;
            }
        }
        let mut anchors = Vec::new();
        let mut next = tails.last().copied();
        while let Some(index) = next {
            anchors.push(candidates[index]);
            next = previous[index];
        }
        // Repeated-only regions still benefit from a rare shared line. Pick it
        // deterministically in source order, never by hash-table iteration order.
        if anchors.is_empty() {
            if let Some(i) = a
                .clone()
                .filter(|&i| positions[old[i]].new_count > 0)
                .min_by_key(|&i| {
                    positions[old[i]]
                        .old_count
                        .saturating_mul(positions[old[i]].new_count)
                })
            {
                let j = b.clone().find(|&j| new[j] == old[i]).unwrap();
                anchors.push((i, j));
            }
        } else {
            anchors.reverse();
        }
        let (mut i, mut j) = (a.start, b.start);
        for &(x, y) in &anchors {
            pending.push((i..x, j..y));
            matches.push((x, y));
            i = x + 1;
            j = y + 1;
        }
        if !anchors.is_empty() {
            pending.push((i..a.end, j..b.end));
        }
    }
    matches.sort_unstable();
    matches
}

fn emit(output: &mut String, prefix: char, line: &str) {
    output.push(prefix);
    output.push_str(line);
    if !line.ends_with('\n') {
        output.push_str("\n\\ No newline at end of file\n");
    }
}

fn range(start: usize, count: usize) -> String {
    if count == 1 {
        (start + 1).to_string()
    } else {
        format!("{},{}", start + usize::from(count > 0), count)
    }
}

fn quote(path: &str) -> String {
    if !path
        .chars()
        .any(|c| c.is_control() || matches!(c, '\\' | '"' | ' '))
    {
        return path.into();
    }
    let mut result = String::from("\"");
    for ch in path.chars() {
        match ch {
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            c if c.is_control() => {
                for byte in c.to_string().bytes() {
                    write!(result, "\\{byte:03o}").unwrap();
                }
            }
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

#[cfg(test)]
mod tests {
    use super::unified;

    #[test]
    fn unified_diff_handles_empty_files_context_and_physical_line_endings() {
        assert_eq!(
            unified("x.jk", "", "x\n"),
            "--- a/x.jk\n+++ b/x.jk\n@@ -0,0 +1 @@\n+x\n"
        );
        assert_eq!(
            unified("x.jk", "x\n", ""),
            "--- a/x.jk\n+++ b/x.jk\n@@ -1 +0,0 @@\n-x\n"
        );
        assert!(unified("x.jk", "x\n", "x\n").is_empty());
        let output = unified("x.jk", "same\r\nold\r\nlast", "same\r\nnew\r\nlast\n");
        assert!(
            output
                .contains(" same\r\n-old\r\n-last\n\\ No newline at end of file\n+new\r\n+last\n"),
            "{output:?}"
        );
        let before = "1\n2\n3\n4\nold\n6\n7\n8\n9\n";
        let output = unified("x.jk", before, &before.replace("old", "new"));
        assert!(
            output.contains("@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-old\n+new\n 6\n 7\n 8\n"),
            "{output}"
        );
    }
    #[test]
    fn unified_diff_splits_distant_changes_and_tracks_shifted_ranges() {
        let before = (1..=20).map(|i| format!("line {i}\n")).collect::<String>();
        let after = before
            .replace("line 2\n", "inserted\nline 2\n")
            .replace("line 19\n", "");
        let diff = unified("x.jk", &before, &after);
        assert_eq!(
            diff,
            concat!(
                "--- a/x.jk\n+++ b/x.jk\n",
                "@@ -1,4 +1,5 @@\n line 1\n+inserted\n line 2\n line 3\n line 4\n",
                "@@ -16,5 +17,4 @@\n line 16\n line 17\n line 18\n-line 19\n line 20\n",
            )
        );
        let repeated = format!("old\n{}old\n", "same\n".repeat(20));
        let diff = unified("x.jk", &repeated, &repeated.replace("old", "new"));
        assert_eq!(
            diff.lines().filter(|line| line.starts_with("@@")).count(),
            2,
            "{diff}"
        );
        let near = before
            .replace("line 2\n", "new 2\n")
            .replace("line 8\n", "new 8\n");
        assert_eq!(
            unified("x.jk", &before, &near)
                .lines()
                .filter(|line| line.starts_with("@@"))
                .count(),
            1
        );
    }

    #[test]
    fn unified_diff_matches_are_ordered_and_preserve_repeated_lines() {
        // Small repeated alphabets exercise ambiguous matches, insertions,
        // deletions and crossed anchors without relying on a particular LCS.
        let texts: Vec<_> = (0..81)
            .map(|mut value| {
                let mut lines = Vec::new();
                for _ in 0..4 {
                    lines.push(["a\n", "b\n", "c\n"][value % 3]);
                    value /= 3;
                }
                lines
            })
            .collect();
        for old in &texts {
            for new in &texts {
                let matches = super::matching_lines(old, new);
                for pair in matches.windows(2) {
                    assert!(pair[0].0 < pair[1].0 && pair[0].1 < pair[1].1);
                }
                for (i, j) in matches {
                    assert_eq!(old[i], new[j]);
                }
            }
        }
    }
}
