//! Startup input and caller-relative paths across executable routes.
use super::*;

#[test]
fn process_arguments_follow_a_protected_source_path() {
    let work = Workspace::new();
    work.write(
        "-source.jk",
        r#"
        import joky/process
        fn main() @suspends { println(process.args()!) }
    "#,
    );
    let output = work.cli(&["run", "--", "-source.jk", "--", "--help", "a b"]);
    success(&output);
    assert_eq!(output.stdout, b"[--help, a b]\n");
}

#[test]
#[cfg(unix)]
fn process_inputs_preserve_arguments_environment_and_caller_paths() {
    let work = Workspace::new();
    work.write("app/joky.toml", "[project]\nname='demo'\nentry='main.jk'");
    work.write("input.txt", "caller data");
    work.write("app/input.txt", "wrong root");
    work.write(
        "app/main.jk",
        r#"
        import joky/process
        import joky/fs
        fn main() @suspends -> Int32 {
            let arguments = process.args()!
            for argument in &arguments { println(argument) }
            println(process.env("JOKY_TEST_ENTRY")!)
            println(process.env("JOKY_TEST_EMPTY")!)
            println(process.env("JOKY_TEST_MISSING")!)
            println(process.env_or("JOKY_TEST_ENTRY", "fallback")!)
            println(process.env_or("JOKY_TEST_MISSING", "fallback")!)
            assert(process.env_or("JOKY_TEST_EMPTY", "fallback")! == "", "empty is present")
            assert(process.env_or("bad=name", "fallback").err()!.kind == fs.ErrorKind.InvalidInput, "errors are preserved")
            let path = process.resolve_path("input.txt")!
            var file = fs.open(path, fs.OpenOptions.default())!
            println(file.read_text()!)
            assert(process.current_dir()!.byte_length() > 0, "caller directory")
            7
        }
    "#,
    );
    success(&work.cli(&["build", "app", "--parallel"]));
    for backend in ["jit", "interpreter", "aot"] {
        let mut command = if backend == "aot" {
            let mut command = Command::new(work.path().join("app/dist/demo"));
            command.current_dir(work.path());
            command
        } else {
            let mut command = work.command();
            command.args(["run", "app", "--parallel"]);
            if backend == "interpreter" {
                command.arg("--interpreter");
            }
            command
        };
        let output = command
            .args(["--", "", "two words", "中文", "--task-workers", "3"])
            .env("JOKY_TEST_ENTRY", "value")
            .env("JOKY_TEST_EMPTY", "")
            .env_remove("JOKY_TEST_MISSING")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7), "{backend}: {output:?}");
        assert_eq!(
            output.stdout,
            "\ntwo words\n中文\n--task-workers\n3\nSome(value)\nSome()\nNone\nvalue\nfallback\ncaller data\n"
                .as_bytes()
        );
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}

#[test]
#[cfg(unix)]
fn stdin_lines_limits_utf8_and_exclusive_readers_match_backends() {
    use std::{io::Write, process::Stdio};
    let work = Workspace::new();
    work.write(
        "stdin.jk",
        r#"
        import joky/io
        import joky/process
        fn failure(error: io.ReadError) -> Int32 {
            match error {
                io.ReadError.LimitExceeded => 2
                io.ReadError.Utf8(_) => 3
                _ => 4
            }
        }
        fn main() @suspends -> Int32 {
            var input = io.stdin()!
            assert(io.stdin().is_err(), "stdin has an exclusive reader")
            let mode = process.env("JOKY_TEST_READ_MODE")!.unwrap_or("lines")
            if mode == "text" {
                match input.read_text_limited(4) {
                    Ok(text) => { print(text); return 0 }
                    Err(error) => {
                        let code = failure(error)
                        assert(input.read_line(8).is_err(), "failed reader stays closed")
                        return code
                    }
                }
            }
            if mode == "bytes" { println(input.read_bytes_limited(8)!); return 0 }
            let limit: UInt64 = if mode == "chunk" { 8192 } else { 16 }
            while true {
                match input.read_line(limit) {
                    Ok(Some(line)) => println("[{line}]")
                    Ok(None) => break
                    Err(error) => { return failure(error) }
                }
            }
            input.close()!
            0
        }
    "#,
    );
    success(&work.cli(&["build", "stdin.jk", "--parallel"]));
    let chunk = format!("{}中\n", "a".repeat(4095));
    let chunk_output = format!("[{}中]\n", "a".repeat(4095));
    let split_crlf = format!("{}\r\nb\n", "a".repeat(4095));
    let split_output = format!("[{}]\n[b]\n", "a".repeat(4095));
    for backend in ["jit", "interpreter", "aot"] {
        for (mode, input, expected, status) in [
            (
                "lines",
                b"\nhello\r\nlast".as_slice(),
                b"[]\n[hello]\n[last]\n".as_slice(),
                0,
            ),
            ("lines", "中文\n".as_bytes(), "[中文]\n".as_bytes(), 0),
            ("lines", b"", b"", 0),
            ("text", b"1234", b"1234", 0),
            ("text", b"12345", b"", 2),
            ("text", b"\xff", b"", 3),
            ("bytes", b"\0\xff", b"[0, 255]\n", 0),
            ("lines", b"123456789012345\n", b"[123456789012345]\n", 0),
            ("lines", b"1234567890123456\n", b"", 2),
            ("chunk", chunk.as_bytes(), chunk_output.as_bytes(), 0),
            ("chunk", split_crlf.as_bytes(), split_output.as_bytes(), 0),
            ("lines", b"1234567890123456", b"[1234567890123456]\n", 0),
        ] {
            let mut command = if backend == "aot" {
                Command::new(work.path().join("stdin"))
            } else {
                let mut command = work.command();
                command.args(["run", "stdin.jk"]);
                if backend == "interpreter" {
                    command.arg("--interpreter");
                }
                command
            };
            let mut child = command
                .env("JOKY_TEST_READ_MODE", mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input).unwrap();
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(status),
                "{backend}/{mode}: {output:?}"
            );
            assert_eq!(output.stdout, expected, "{backend}/{mode}: {output:?}");
            assert!(output.stderr.is_empty(), "{backend}/{mode}: {output:?}");
        }
    }
}

#[test]
#[cfg(unix)]
fn process_invalid_unicode_is_reported_without_lossy_conversion() {
    use std::os::unix::ffi::OsStringExt;
    let work = Workspace::new();
    work.write(
        "encoding.jk",
        r#"
        import joky/process
        fn main() @suspends -> Int32 {
            if process.args().is_err() { return 3 }
            if process.env("JOKY_TEST_ENCODING").is_err() { return 4 }
            assert(process.env("bad=name").is_err(), "invalid key")
            0
        }
    "#,
    );
    success(&work.cli(&["build", "encoding.jk"]));
    for native in [false, true] {
        for invalid_argument in [false, true] {
            let mut command = if native {
                Command::new(work.path().join("encoding"))
            } else {
                let mut command = work.command();
                command.args(["run", "encoding.jk"]);
                command
            };
            let invalid = std::ffi::OsString::from_vec(vec![255]);
            if invalid_argument {
                command.arg("--").arg(invalid);
            } else {
                command.env("JOKY_TEST_ENCODING", invalid);
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(if invalid_argument { 3 } else { 4 }),
                "{output:?}"
            );
            assert!(output.stdout.is_empty() && output.stderr.is_empty());
        }
    }
}

#[test]
#[cfg(unix)]
fn stdin_examples_accept_pipes_and_keep_diagnostics_on_stderr() {
    use std::{io::Write, process::Stdio};
    let work = Workspace::new();
    for (name, source, input, stdout, status) in [
        (
            "json",
            include_str!("../../examples/stdin_json.jk"),
            b" { \"a\" : 1 } ".as_slice(),
            b"{\"a\":1}\n".as_slice(),
            0,
        ),
        (
            "invalid_json",
            include_str!("../../examples/stdin_json.jk"),
            b"{",
            b"",
            2,
        ),
        (
            "lines",
            include_str!("../../examples/stdin_lines.jk"),
            b"one\ntwo",
            b"item: one\nitem: two\n",
            0,
        ),
    ] {
        let file = format!("{name}.jk");
        work.write(&file, source);
        success(&work.cli(&["build", &file]));
        for backend in ["jit", "interpreter", "aot"] {
            let mut command = if backend == "aot" {
                Command::new(work.path().join(name))
            } else {
                let mut command = work.command();
                command.args(["run", &file]);
                if backend == "interpreter" {
                    command.arg("--interpreter");
                }
                command
            };
            let mut child = command
                .env("JOKY_LINE_PREFIX", "item: ")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input).unwrap();
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(status),
                "{backend}/{name}: {output:?}"
            );
            assert_eq!(output.stdout, stdout, "{backend}/{name}: {output:?}");
            if status == 0 {
                assert!(output.stderr.is_empty(), "{output:?}");
            } else {
                assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid JSON at byte"));
            }
        }
    }
}

#[test]
fn chat_port_configuration_validates_environment_values() {
    let work = Workspace::new();
    work.write(
        "config.jk",
        include_str!("../../examples/chat/host/config.jk"),
    );
    work.write(
        "port.jk",
        r#"
        import config
        fn main() @suspends -> Int32 {
            match config.port("JOKY_TEST_PORT", 7777) {
                Ok(port) => { println(port); 0 }
                Err(error) => { eprintln(error); 2 }
            }
        }
    "#,
    );
    for (value, expected) in [
        (None, Some(7777)),
        (Some("0"), Some(0)),
        (Some("65535"), Some(65535)),
        (Some("+0042"), Some(42)),
        (Some(""), None),
        (Some("65536"), None),
        (Some("-1"), None),
        (Some("１２"), None),
    ] {
        let mut command = work.command();
        command.args(["run", "--interpreter", "port.jk"]);
        if let Some(value) = value {
            command.env("JOKY_TEST_PORT", value);
        } else {
            command.env_remove("JOKY_TEST_PORT");
        }
        let output = command.output().unwrap();
        if let Some(port) = expected {
            success(&output);
            assert_eq!(output.stdout, format!("{port}\n").as_bytes());
        } else {
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("JOKY_TEST_PORT"));
        }
    }
}
