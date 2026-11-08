//! Static, offline homepage and standard-library reference generation.
use super::{cli_argument_error, Command, Error};
use joky::parser::documentation::{Documentation, Entry};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

pub(super) struct Options {
    output: PathBuf,
    standard: bool,
    open: bool,
}

pub(super) fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Command, Error> {
    let mut standard = false;
    let mut open = false;
    let mut output = None;
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(Command::Help);
        }
        if arg == "--std" && !standard {
            standard = true;
        } else if arg == "--open" && !open {
            open = true;
        } else if (arg == "-o" || arg == "--output") && output.is_none() {
            let path = args
                .next()
                .ok_or_else(|| cli_argument_error("Missing directory after --output"))?;
            if path.is_empty() {
                return Err(cli_argument_error("Output directory must not be empty"));
            }
            output = Some(PathBuf::from(path));
        } else {
            return Err(cli_argument_error(format!(
                "Unknown or duplicate doc argument '{}'",
                arg.to_string_lossy()
            )));
        }
    }
    Ok(Command::Doc(Options {
        output: output.unwrap_or_else(|| {
            PathBuf::from(if standard {
                "target/doc/std"
            } else {
                "target/doc"
            })
        }),
        standard,
        open,
    }))
}

pub(super) fn execute(options: &Options) -> u8 {
    let result = if options.standard {
        let home = options.output.parent().is_some_and(|parent| {
            fs::read_to_string(parent.join(".joky-doc")).ok().as_deref() == Some(SITE_MARKER)
                && parent.join("index.html").is_file()
        });
        generate(&options.output, home)
    } else {
        generate_site(&options.output)
    };
    let result = result.and_then(|()| {
        let index = options.output.join("index.html");
        println!("Generated {}", index.display());
        if options.open {
            open_browser(&index)?;
        }
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

fn open_browser(index: &Path) -> Result<(), Error> {
    let path = fs::canonicalize(index).map_err(io)?;
    let launcher = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let status = std::process::Command::new(launcher)
        .arg(&path)
        .status()
        .map_err(|error| {
            io(std::io::Error::new(
                error.kind(),
                format!(
                    "Documentation was generated, but could not open {} with {launcher}: {error}",
                    path.display()
                ),
            ))
        })?;
    if !status.success() {
        return Err(super::cli_error_with_code(
            super::ErrorCode::IoError,
            format!(
                "Documentation was generated, but {launcher} failed to open {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn io(error: std::io::Error) -> Error {
    super::cli_error_with_code(super::ErrorCode::IoError, error.to_string())
}
const MARKER: &str = "Joky standard documentation v1\n";
const SITE_MARKER: &str = "Joky documentation site v1\n";
const HOME_FILES: &[(&str, &str)] = &[
    (
        "index.html",
        include_str!("../../sites/joky-home/public/index.html"),
    ),
    (
        "style.css",
        include_str!("../../sites/joky-home/public/style.css"),
    ),
    (
        "main.js",
        include_str!("../../sites/joky-home/public/main.js"),
    ),
    (
        "favicon.svg",
        include_str!("../../sites/joky-home/public/favicon.svg"),
    ),
];

fn generate_site(output: &Path) -> Result<(), Error> {
    directory(output)?;
    let entries = fs::read_dir(output)
        .map_err(io)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io)?;
    // Upgrade the default directory left by an earlier "doc --std".
    let standard_only = HOME_FILES.iter().all(|(name, _)| {
        fs::symlink_metadata(output.join(name))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    }) && fs::symlink_metadata(output.join(".joky-doc"))
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        && fs::read_to_string(output.join("std/.joky-doc"))
            .ok()
            .as_deref()
            == Some(MARKER);
    if !entries.is_empty()
        && !standard_only
        && fs::read_to_string(output.join(".joky-doc")).ok().as_deref() != Some(SITE_MARKER)
    {
        return Err(cli_argument_error(
            "Output directory is not empty and was not generated by joky doc",
        ));
    }
    generate(&output.join("std"), true)?;
    write(&output.join(".joky-doc"), SITE_MARKER)?;
    for (name, content) in HOME_FILES {
        write(&output.join(name), content)?;
    }
    Ok(())
}
fn directory(path: &Path) -> Result<(), Error> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(cli_argument_error(format!(
                "Documentation path must be a directory, not a symlink: {}",
                path.display()
            )));
        }
    }
    fs::create_dir_all(path).map_err(io)
}
fn write(path: &Path, text: &str) -> Result<(), Error> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(cli_argument_error(format!(
            "Refusing to replace symlink {}",
            path.display()
        )));
    }
    fs::write(path, text).map_err(io)
}
fn generate(output: &Path, home: bool) -> Result<(), Error> {
    let modules = joky::documentation::standard_library()?;
    directory(output)?;
    let marker = output.join(".joky-doc");
    if fs::read_dir(output).map_err(io)?.next().is_some()
        && fs::read_to_string(&marker).ok().as_deref() != Some(MARKER)
    {
        return Err(cli_argument_error(
            "Output directory is not empty and was not generated by joky doc",
        ));
    }
    write(&marker, MARKER)?;
    write(&output.join("style.css"), include_str!("doc/style.css"))?;
    let mut index = String::from(
        "<p>Standard library API reference. Select a module below.</p><ul class=modules>",
    );
    for (name, docs) in &modules {
        let slug = name.strip_prefix("joky/").unwrap();
        directory(&output.join(slug))?;
        let body = module_page(name, docs);
        write(
            &output.join(slug).join("index.html"),
            &page(name, &"../".repeat(slug.split('/').count()), &body, home),
        )?;
        index.push_str(&format!(
            "<li><a href=\"{slug}/index.html\">{}</a><p>{}</p></li>",
            escape(name),
            inline(docs.docs.lines().next().unwrap_or(""))
        ));
    }
    index.push_str("</ul>");
    write(
        &output.join("index.html"),
        &page("Joky standard library", "./", &index, home),
    )
}
fn page(title: &str, root: &str, body: &str, home: bool) -> String {
    let home_link = if home {
        format!(
            "<a href=\"{root}../index.html\">Joky 首页</a><span aria-hidden=\"true\"> / </span>"
        )
    } else {
        String::new()
    };
    format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{0} — Joky</title><link rel=\"stylesheet\" href=\"{root}style.css\"></head><body><header><nav aria-label=\"Documentation\">{home_link}<a href=\"{root}index.html\">Joky / std</a></nav><span>v{1}</span></header><main><h1>{0}</h1>{body}</main></body></html>", escape(title), env!("CARGO_PKG_VERSION"))
}
fn module_page(name: &str, docs: &Documentation) -> String {
    let mut out = markdown(&docs.docs);
    if docs.entries.is_empty() {
        out.push_str(
            "<p>This module has no direct public declarations; see its source imports below.</p>",
        );
    }
    out.push_str("<nav aria-label=\"API index\"><ul>");
    for e in &docs.entries {
        out.push_str(&format!(
            "<li><a href=\"#{}\">{}</a></li>",
            anchor(e),
            label(e)
        ));
    }
    out.push_str("</ul></nav>");
    for e in &docs.entries {
        render_entry(e, &anchor(e), 2, &mut out);
    }
    if !docs.imports.is_empty() {
        out.push_str(
            "<details><summary>Source imports (not necessarily re-exports)</summary><pre><code>",
        );
        out.push_str(&highlight(&docs.imports.join("\n")));
        out.push_str("</code></pre></details>");
    }
    out.push_str(&format!("<footer>Generated from the embedded {} source. Internal host declarations are omitted.</footer>", escape(name)));
    out
}
fn anchor(e: &Entry) -> String {
    let plain = e
        .name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'_' {
                (b as char).to_string()
            } else {
                format!("-{b:02x}")
            }
        })
        .collect::<String>();
    if e.kind == "impl" {
        let hash = e.signature.bytes().fold(14695981039346656037_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(1099511628211)
        });
        format!("impl-{plain}-{hash:x}")
    } else {
        format!("{}-{plain}", e.kind)
    }
}
fn label(e: &Entry) -> String {
    if e.kind == "impl" {
        escape(&e.signature)
    } else {
        format!("{} {}", e.kind, escape(&e.name))
    }
}
fn render_entry(e: &Entry, id: &str, level: u8, out: &mut String) {
    out.push_str(&format!("<section id=\"{id}\"><h{level}><a href=\"#{id}\">{}</a></h{level}><pre><code>{}</code></pre>{}", label(e), highlight(&e.signature), markdown(&e.docs)));
    for c in &e.children {
        render_entry(c, &format!("{id}-{}", anchor(c)), 3, out);
    }
    out.push_str("</section>");
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn highlight(source: &str) -> String {
    let Ok(spans) = joky::parser::documentation::syntax_spans(source) else {
        return escape(source);
    };
    let mut out = String::new();
    let mut cursor = 0;
    for (span, category) in spans {
        out.push_str(&escape(&source[cursor..span.start()]));
        out.push_str(&format!(
            "<span class=\"syntax-{category}\">{}</span>",
            escape(&source[span.start()..span.end()])
        ));
        cursor = span.end();
    }
    out.push_str(&escape(&source[cursor..]));
    out
}
fn code_block(source: &str, language: &str) -> String {
    if matches!(language, "" | "jk" | "joky") {
        format!(
            "<pre><code class=\"language-joky\">{}</code></pre>",
            highlight(source)
        )
    } else {
        format!("<pre><code>{}</code></pre>", escape(source))
    }
}
fn inline(text: &str) -> String {
    let parts: Vec<_> = text.split('`').collect();
    if parts.len() % 2 == 0 {
        return escape(text);
    }
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if i % 2 == 1 {
                format!("<code>{}</code>", escape(p))
            } else {
                escape(p)
            }
        })
        .collect()
}
// A deliberately small Markdown subset: paragraphs, headings, list items and
// fenced examples. Raw HTML is always escaped; code text is never reformatted.
fn markdown(text: &str) -> String {
    let mut out = String::new();
    let mut code: Option<String> = None;
    let mut language = "";
    let mut paragraph = false;
    let mut list = false;
    for line in text.lines() {
        if let Some(source) = &mut code {
            if line.starts_with("```") {
                out.push_str(&code_block(source, language));
                code = None;
            } else {
                source.push_str(line);
                source.push('\n');
            }
            continue;
        }
        let fence = line.starts_with("```");
        let heading = line
            .strip_prefix("# ")
            .or_else(|| line.strip_prefix("## "))
            .or_else(|| line.strip_prefix("### "));
        let item = line.strip_prefix("- ");
        let plain = !fence && heading.is_none() && item.is_none() && !line.trim().is_empty();
        if paragraph && !plain {
            out.push_str("</p>");
            paragraph = false;
        }
        if list && item.is_none() {
            out.push_str("</ul>");
            list = false;
        }
        if fence {
            language = line[3..].trim();
            code = Some(String::new());
        } else if let Some(heading) = heading {
            out.push_str(&format!("<h4>{}</h4>", inline(heading)));
        } else if let Some(item) = item {
            if !list {
                out.push_str("<ul>");
                list = true;
            }
            out.push_str(&format!("<li>{}</li>", inline(item)));
        } else if plain {
            out.push_str(if paragraph { "\n" } else { "<p>" });
            out.push_str(&inline(line));
            paragraph = true;
        }
    }
    if let Some(source) = code {
        out.push_str(&code_block(&source, language));
    }
    if paragraph {
        out.push_str("</p>");
    }
    if list {
        out.push_str("</ul>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documentation_html_escapes_text_and_preserves_examples() {
        let html = markdown("# Example\n`a < b` and <script>\n- first\n```jk\nfn main() {\n    println(\"<tag>\")\n}\n```");
        assert!(html.contains("<code>a &lt; b</code> and &lt;script&gt;"));
        assert!(html.contains("class=\"syntax-keyword\">fn</span>"));
        assert!(html.contains("class=\"syntax-string\">&quot;&lt;tag&gt;&quot;</span>"));
        assert!(html.contains("\n    <span class=\"syntax-function\">println</span>"));
        assert!(!html.contains("<script>"));
        assert_eq!(inline("an `unfinished span"), "an `unfinished span");
        assert_eq!(markdown("```text\n<"), "<pre><code>&lt;\n</code></pre>");
        assert_eq!(
            markdown("```sh\necho '<script>'\n```"),
            "<pre><code>echo &#39;&lt;script&gt;&#39;\n</code></pre>"
        );
        assert_eq!(
            highlight("\"unfinished <script>"),
            "&quot;unfinished &lt;script&gt;"
        );
        let code = "/* outer\n /* inner */ end */\nlet 文本 = r#\"<script>  &\"#\r\n";
        let marked = highlight(code);
        let mut text = String::new();
        let mut tag = false;
        for c in marked.chars() {
            if c == '<' {
                tag = true;
            } else if c == '>' {
                tag = false;
            } else if !tag {
                text.push(c);
            }
        }
        assert_eq!(text, escape(code));
        assert_eq!(
            markdown("first\nsecond\n\n- one\n- two"),
            "<p>first\nsecond</p><ul><li>one</li><li>two</li></ul>"
        );
        let docs =
            joky::parser::documentation::extract("pub fn 中文() {} pub fn 字符() {}").unwrap();
        assert_ne!(anchor(&docs.entries[0]), anchor(&docs.entries[1]));
    }
}
