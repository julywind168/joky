//! A unified preview with linear memory use. One hunk encloses the changed line range;
//! common prefix/suffix lines are trimmed to three lines of context.
use std::fmt::Write;

pub(super) fn unified(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }
    let old: Vec<_> = before.split_inclusive('\n').collect();
    let new: Vec<_> = after.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let start = prefix.saturating_sub(3);
    let old_change = old.len() - suffix;
    let new_change = new.len() - suffix;
    let context = suffix.min(3);
    let mut output = format!(
        "--- {}\n+++ {}\n",
        quote(&format!("a/{path}")),
        quote(&format!("b/{path}"))
    );
    writeln!(
        output,
        "@@ -{} +{} @@",
        range(start, old_change + context - start),
        range(start, new_change + context - start)
    )
    .unwrap();
    for line in &old[start..prefix] {
        emit(&mut output, ' ', line);
    }
    for line in &old[prefix..old_change] {
        emit(&mut output, '-', line);
    }
    for line in &new[prefix..new_change] {
        emit(&mut output, '+', line);
    }
    for line in &old[old_change..old_change + context] {
        emit(&mut output, ' ', line);
    }
    output
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
}
