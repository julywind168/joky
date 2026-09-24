use super::*;

const FILE_API: &str = include_str!("../../../std/joky/file.jk");

#[test]
fn file_close_consumes_method_and_effect_receivers() {
    for body in [
        "handle.close()!; let _ = handle.position()!",
        "file.close(handle)!; let _ = file.position(handle)!",
        "handle.close()!; handle.close()!",
        "if flag { handle.close()! }; let _ = handle.position()!",
    ] {
        let source = format!("{FILE_API}\n fn invalid(handle: File, flag: Bool) effects {{ file }} {{ {body}; () }} fn main() {{}}");
        let error = try_run_program(&source).expect_err(body);
        assert!(
            error.to_string().contains("after move") || error.to_string().contains("ownership"),
            "{error}"
        );
    }
}

#[test]
fn file_borrowed_parameter_cannot_be_closed_or_returned() {
    for function in [
        "fn invalid(handle: &File) effects { file } { handle.close()! }",
        "fn invalid(handle: &File) -> File { handle }",
        "fn consume(handle: File) {} fn invalid(handle: &File) { consume(handle) }",
        "fn invalid(handle: &File) { let closure = move fn () -> File { handle }; }",
    ] {
        let source = format!("{FILE_API}\n{function}\nfn main() {{}}");
        assert!(try_run_program(&source).is_err(), "accepted {function}");
    }
}

#[test]
fn file_read_chunk_borrows_across_repeated_helper_calls() {
    let directory = Directory::new("borrowed-helpers");
    std::fs::write(directory.0.join("input"), b"abcd").unwrap();
    let source = format!(
        r#"{FILE_API}
        fn read_one(handle: &File) -> Result(Bytes, String) effects {{ file }} {{ handle.read_chunk(1) }}
        fn read_two(handle: &File) effects {{ file }} {{ let _ = read_one(handle)!; let _ = read_one(handle)!; () }}
        fn main() effects {{ file }} {{
            let a: UInt8 = 97; let b: UInt8 = 98; let c: UInt8 = 99
            let handle = file.open("{}/input", FileMode.Read)!
            if read_one(handle)!.get(0)! != a {{ panic("first") }} else {{}}
            if read_one(handle)!.get(0)! != b {{ panic("second") }} else {{}}
            if file.read_chunk(handle, 1)!.get(0)! != c {{ panic("third") }} else {{}}
            let _ = handle.seek(0, SeekFrom.Start)!
            read_two(handle)
            if read_one(handle)!.get(0)! != c {{ panic("after resumed loan") }} else {{}}
            handle.close()!
            let _ = file.write("{}/completed", "done")!
            ()
        }}
    "#,
        directory.0.display(),
        directory.0.display()
    );
    run_program(&source);
    assert_eq!(
        std::fs::read(directory.0.join("completed")).unwrap(),
        b"done"
    );
}

struct Directory(std::path::PathBuf);

impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("joky-files-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn file_handles_copy_binary_chunks_seek_append_and_close() {
    let directory = Directory::new("chunks");
    let bytes: Vec<u8> = (0..100_007).map(|i| (i % 256) as u8).collect();
    std::fs::write(directory.0.join("input"), &bytes).unwrap();
    let source = format!(
        r#"{FILE_API}
        fn main() effects {{ file }} {{
            let total: UInt64 = 100007; let last: UInt64 = 100006; let last_byte: UInt8 = 166
            let input = file.open("{}/input", FileMode.Read)!
            let output = file.open("{}/output", FileMode.Write)!
            loop {{
                let bytes = input.read_chunk(4096)!
                if bytes.is_empty() {{ break }} else {{ }}
                let written = output.write_chunk(bytes)!
                if written != bytes.length() {{ panic("short write") }} else {{ }}
            }}
            if output.position()! != total {{ panic("position") }} else {{ }}
            output.flush()!
            output.sync()!
            output.close()!
            if input.seek(-1, SeekFrom.End)! != last {{ panic("seek") }} else {{ }}
            if input.read_chunk(1)!.get(0)! != last_byte {{ panic("last byte") }} else {{ }}
            input.close()!
            let append = file.open("{}/output", FileMode.Append)!
            let _ = append.write_chunk(Bytes.from_string("tail"))!
            append.close()!
        }}
    "#,
        directory.0.display(),
        directory.0.display(),
        directory.0.display()
    );
    try_run_program(&source).expect("chunked file operations");
    let mut expected = bytes;
    expected.extend_from_slice(b"tail");
    let actual = std::fs::read(directory.0.join("output")).unwrap();
    assert_eq!(actual.len(), expected.len(), "copy was truncated");
    assert!(actual == expected, "copied bytes differ");
}

#[test]
fn file_empty_binary_errors_and_overwrite_are_typed_results() {
    let directory = Directory::new("results");
    std::fs::write(directory.0.join("binary"), [0, 255, 65]).unwrap();
    let base = directory.0.display();
    let source = format!(
        r#"{FILE_API}
        fn main() effects {{ file }} {{
            let u0: UInt64 = 0; let u2: UInt64 = 2; let u3: UInt64 = 3; let b0: UInt8 = 0; let b255: UInt8 = 255
            let bytes = file.read_bytes("{base}/binary")!
            if bytes.length() != u3 {{ panic("length") }} else {{ }}
            if bytes.get(0)! != b0 {{ panic("zero") }} else {{ }}
            if bytes.get(1)! != b255 {{ panic("binary") }} else {{ }}
            match file.read("{base}/binary") {{ Ok(_) => panic("UTF-8"), Err(message) => if message == "" {{ panic("error") }} else {{ }} }}
            match file.read_bytes("{base}/missing") {{ Ok(_) => panic("missing"), Err(message) => if message == "" {{ panic("error") }} else {{ }} }}
            match file.write_bytes("{base}", bytes) {{ Ok(_) => panic("directory"), Err(_) => {{ }} }}
            if file.write_bytes("{base}/binary", Bytes())! != u0 {{ panic("empty count") }} else {{ }}
            if !file.read_bytes("{base}/binary")!.is_empty() {{ panic("truncate") }} else {{ }}
            if file.read("{base}/binary")! != "" {{ panic("empty text") }} else {{ }}
            let _ = file.write("{base}/text", "long previous text")!
            if file.write("{base}/text", "hi")! != u2 {{ panic("count") }} else {{ }}
        }}
    "#
    );
    try_run_program(&source).unwrap();
    assert_eq!(std::fs::read(directory.0.join("text")).unwrap(), b"hi");
}

#[test]
fn file_modes_and_invalid_operations() {
    let directory = Directory::new("modes");
    let base = directory.0.display();
    let source = format!(
        r#"{FILE_API}
        fn main() effects {{ file }} {{
            let a: UInt8 = 97; let u2: UInt64 = 2
            match file.open("{base}/missing", FileMode.ReadWrite) {{ Ok(_) => panic("must exist"), Err(_) => {{ }} }}
            let created = file.open("{base}/new", FileMode.CreateNew)!
            let _ = created.write_chunk(Bytes.from_string("abc"))!
            let _ = created.seek(0, SeekFrom.Start)!
            if created.read_chunk(1)!.get(0)! != a {{ panic("read write") }} else {{ }}
            if created.seek(1, SeekFrom.Current)! != u2 {{ panic("relative") }} else {{ }}
            match created.seek(-1, SeekFrom.Start) {{ Ok(_) => panic("negative"), Err(_) => {{ }} }}
            match created.read_chunk(0) {{ Ok(_) => panic("zero"), Err(_) => {{ }} }}
            match file.open("{base}/new", FileMode.CreateNew) {{ Ok(_) => panic("exclusive"), Err(_) => {{ }} }}
            let readonly = file.open("{base}/new", FileMode.Read)!
            match readonly.write_chunk(Bytes.from_string("no")) {{ Ok(_) => panic("readonly"), Err(_) => {{ }} }}
            // Deliberately leave both handles to lexical cleanup.
        }}
    "#
    );
    try_run_program(&source).unwrap();
    assert_eq!(std::fs::read(directory.0.join("new")).unwrap(), b"abc");
}

#[test]
fn file_handle_cannot_be_captured_by_multiple_parallel_arms() {
    let source = format!(
        r#"{FILE_API}
        fn main() effects {{ file }} {{
            let input = file.open("unused", FileMode.Read)!
            let values = parallel {{
                | input.read_chunk(8)
                | input.read_chunk(8)
            }}
        }}
    "#
    );
    let error = try_run_program(&source).unwrap_err();
    assert!(error.to_string().contains("after move"), "{error:?}");
}

#[test]
#[ignore = "process-isolated file and cancellation stress; use scripts/check-runtime.py"]
fn file_workflows_release_handles_buffers_and_cancelled_outputs() {
    let directory = Directory::new("resources");
    let base = directory.0.display();
    let bytes: Vec<u8> = (0..1_048_589).map(|i| (i % 251) as u8).collect();
    std::fs::write(directory.0.join("input"), &bytes).unwrap();
    for _ in 0..5 {
        let source = format!(
            r#"{FILE_API}
            eff time {{ @suspends fn sleep(duration: Duration) -> Unit }}
            fn delayed_read(handle: &File) effects {{ file, time }} {{
                let _ = handle.read_chunk(1)!
                time.sleep(100ms)
                let _ = handle.read_chunk(4096)!
                ()
            }}
            fn main() effects {{ file, time }} {{
                let sizes = @parallel(limit: 2) for path in List("{base}/a", "{base}/b", "{base}/c") {{
                    let input = file.open("{base}/input", FileMode.Read)!
                    let output = file.open(path, FileMode.Write)!
                    loop {{
                        let bytes = input.read_chunk(8192)!
                        if bytes.is_empty() {{ break }} else {{ }}
                        let _ = output.write_chunk(bytes)!
                    }}
                    output.position()!
                }}
                let total: UInt64 = 1048589
                if sizes.head()! != total {{ panic("copy length") }} else {{ }}
                let result = race {{
                    | {{ let handle = file.open("{base}/input", FileMode.Read)!; delayed_read(handle); 0 }}
                    | {{ time.sleep(1ms); 1 }}
                }}
            }}
        "#
        );
        try_run_program(&source).unwrap();
        for name in ["a", "b", "c"] {
            assert_eq!(std::fs::read(directory.0.join(name)).unwrap(), bytes);
        }
        let source = format!(
            r#"{FILE_API}
            eff Missing {{ @suspends fn fail() -> Unit }}
            fn main() effects {{ file, Missing }} {{
                let input = file.open("{base}/input", FileMode.Read)!
                input.read_chunk(64)!
                Missing.fail()
            }}
        "#
        );
        assert!(try_run_program(&source).is_err());
    }
}

#[test]
#[ignore = "regression campaign for the lost File drop in cancelled race arms; run under CPU load like scripts/check-runtime.py"]
fn race_cancellation_releases_owned_file_handles() {
    let directory = std::env::temp_dir().join(format!("joky-race-leak-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("input"), b"0123456789abcdef").unwrap();
    let base = directory.display();
    for _ in 0..300 {
        let source = format!(
            r#"{FILE_API}
            eff time {{ @suspends fn sleep(duration: Duration) -> Unit }}
            fn main() effects {{ file, time }} {{
                let result = race {{
                    | {{ let handle = file.open("{base}/input", FileMode.Read)!; handle.read_chunk(1)!; time.sleep(100ms); handle.read_chunk(4096)!; 0 }}
                    | {{ time.sleep(1ms); 1 }}
                }}
            }}"#
        );
        try_run_program(&source).unwrap();
    }
    let _ = std::fs::remove_dir_all(&directory);
}
