use super::*;

#[test]
fn executes_parallel_through_the_task_runtime() {
    Compiler::new()
            .unwrap()
            .run_program(
                "fn first() -> Int32 { 1 }\n\
                 fn second() -> Int32 { 2 }\n\
                 fn main() { let values = parallel {\n| first()\n| second()\n}; println(values.0); println(values.1) }",
            )
            .expect("parallel tasks should execute through the runtime");
}

#[test]
fn executes_time_sleep_through_the_runtime_timer() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait() -> Unit effects { time } { time.sleep(1ms) }\n\
                 fn main() effects { time } { wait(); println(\"awake\") }",
        )
        .expect("time.sleep should use the runtime timer");
}

#[test]
fn handles_a_suspending_effect_with_immediate_resume() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Input { @suspends fn read() -> Int32 }

                    fn main() {
                        let value = do { Input.read() } with {
                            Input.read() => 42
                        }
                        println(value)
                    }
                "#,
        )
        .expect("suspending effects should support synchronous test handlers");
}

#[test]
fn reads_a_file_through_the_blocking_provider() {
    let path = std::env::temp_dir().join(format!(
        "joky-file-read-{}-{}.txt",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::write(&path, "blocking file read").expect("test file should be written");
    let source = format!(
        "eff file {{ @suspends fn read(path: String) -> Result(String, String) }}\n\
             fn main() effects {{ file }} {{ match file.read(path: \"{}\") {{\n\
             Ok(contents) => println(contents)\n\
             Err(message) => panic(message)\n\
             }} }}",
        path.to_string_lossy()
    );
    let result = try_run_program(&source);
    std::fs::remove_file(&path).expect("test file should be removed");
    result.expect("file.read should complete on the blocking pool");
}

#[test]
fn reads_non_utf8_file_as_bytes() {
    let path = std::env::temp_dir().join(format!("joky-file-bytes-{}", std::process::id()));
    std::fs::write(&path, [0_u8, 0xff, 0x41]).expect("write test bytes");
    let source = format!(
        "eff file {{ @suspends fn read_bytes(path: String) -> Result(Bytes, String) }}\n\
         fn main() effects {{ file }} {{ match file.read_bytes(path: \"{}\") {{\n\
         Ok(contents) => println(contents.length())\n\
         Err(message) => panic(message)\n\
         }} }}",
        path.to_string_lossy()
    );
    let result = try_run_program(&source);
    std::fs::remove_file(&path).expect("remove test file");
    result.expect("file.read_bytes should preserve binary data");
}

#[test]
fn writes_text_and_binary_files() {
    let text_path = std::env::temp_dir().join(format!("joky-file-write-{}", std::process::id()));
    let bytes_path =
        std::env::temp_dir().join(format!("joky-file-write-bytes-{}", std::process::id()));
    let source = format!(
        "eff file {{ @suspends fn write(path: String, data: String) -> Result(UInt64, String) @suspends fn write_bytes(path: String, data: Bytes) -> Result(UInt64, String) }}\n\
         fn main() effects {{ file }} {{ let data = MutBytes(); data.push(0); data.push(255); data.push(65); match file.write(path: \"{}\", data: \"hello\") {{ Ok(_) => println(\"text\"), Err(e) => panic(e) }}; match file.write_bytes(path: \"{}\", data: data.to_bytes()) {{ Ok(_) => println(\"bytes\"), Err(e) => panic(e) }} }}",
        text_path.to_string_lossy(), bytes_path.to_string_lossy()
    );
    let result = try_run_program(&source);
    result.expect("file writes should complete on the blocking pool");
    assert_eq!(std::fs::read(&text_path).unwrap(), b"hello");
    assert_eq!(std::fs::read(&bytes_path).unwrap(), [0, 255, 65]);
    let _ = std::fs::remove_file(text_path);
    let _ = std::fs::remove_file(bytes_path);
}

#[test]
fn opens_and_reads_a_file_handle() {
    let path = std::env::temp_dir().join(format!("joky-file-handle-{}", std::process::id()));
    std::fs::write(&path, b"handle-data").unwrap();
    let source = format!(
        "{}\nfn main() effects {{ file }} {{ let f = file.open(\"{}\", FileMode.Read)!; let b = f.read_chunk(64)!; println(b.length()); f.close()! }}",
        include_str!("../../../std/joky/file.jk"), path.display()
    );
    let result = try_run_program(&source);
    let _ = std::fs::remove_file(path);
    result.expect("file handle should open and read");
}

#[test]
fn connects_through_tcp_provider() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind socket test");
    let port = listener.local_addr().expect("socket test address").port();
    listener
        .set_nonblocking(true)
        .expect("set test listener nonblocking");
    let (accepted_sender, accepted_receiver) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        for _ in 0..1_000 {
            match listener.accept() {
                Ok((_stream, _)) => {
                    accepted_sender.send(true).expect("send accepted socket");
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("accept socket test: {error}"),
            }
        }
        accepted_sender.send(false).expect("send missing socket");
    });
    let source = format!(
        "eff tcp {{\n\
                 @suspends\n\
                 fn connect(host: String, port: UInt16) -> Result(TcpStream, String)\n\
                 @suspends\n\
                 fn read(socket: &TcpStream, max_bytes: UInt64) -> Result(Bytes, String)\n\
                 @suspends\n\
                 fn write(socket: &TcpStream, value: Bytes) -> Result(UInt64, String)\n\
                 @suspends\n\
                 fn close(socket: TcpStream) -> Result(Unit, String)\n\
                 }}\n\
                 fn main() effects {{ tcp }} {{\n\
                     match tcp.connect(host: \"127.0.0.1\", port: {port}) {{\n\
                         Ok(_socket) => {{}}\n\
                         Err(message) => panic(message)\n\
                     }}\n\
                 }}"
    );
    Compiler::new()
        .unwrap()
        .run_program(&source)
        .expect("TcpStream connect should complete through mio reactor");
    server.join().expect("socket test server should finish");
    assert!(accepted_receiver.recv().expect("receive accepted socket"));
}

#[test]
fn tcp_provider_round_trips_bytes() {
    const TCP_API: &str = include_str!("../../../std/joky/socket/tcp.jk");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").unwrap();
    });
    let source = format!(
            "{TCP_API}
                 fn main() effects {{ tcp }} {{
                     let expected: UInt64 = 4
                     match tcp.connect(host: \"127.0.0.1\", port: {port}) {{
                         Ok(socket) => {{
                             match socket.write(Bytes.from_string(\"ping\")) {{
                                 Ok(_) => {{
                                     match socket.read(4) {{
                                         Ok(value) => {{
                                             if value.length() != expected {{ panic(\"bad read\") }} else {{ }}
                                             match value.to_string() {{
                                                 Some(text) => if text != \"pong\" {{ panic(\"bad payload\") }} else {{ }},
                                                 None => panic(\"invalid UTF-8 payload\")
                                             }}
                                             match socket.close() {{
                                                 Ok(_) => {{ }}
                                                 Err(message) => panic(message)
                                             }}
                                         }}
                                         Err(message) => panic(message)
                                     }}
                                 }}
                                 Err(message) => panic(message)
                             }}
                         }}
                         Err(message) => panic(message)
                     }}
                 }}"
        );
    run_program(&source);
    server.join().unwrap();
}

#[test]
fn tcp_provider_accepts_server_connections() {
    const TCP_API: &str = include_str!("../../../std/joky/socket/tcp.jk");
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let (client_sender, client_receiver) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut stream = None;
        for _ in 0..1_000 {
            match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(value) => {
                    stream = Some(value);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        let mut stream = stream.expect("server should accept a client");
        stream.write_all(b"ping").unwrap();
        let mut response = [0_u8; 4];
        stream.read_exact(&mut response).unwrap();
        client_sender.send(response).unwrap();
    });
    let source = format!(
        "{TCP_API}
                 fn main() effects {{ tcp }} {{
                     match tcp.listen(host: \"127.0.0.1\", port: {port}) {{
                         Ok(listener) => match listener.accept() {{
                             Ok(socket) => match socket.read(4) {{
                                 Ok(value) => match socket.write(value) {{
                                     Ok(_) => {{}}
                                     Err(message) => panic(message)
                                 }}
                                 Err(message) => panic(message)
                             }}
                             Err(message) => panic(message)
                         }}
                         Err(message) => panic(message)
                     }}
                 }}"
    );
    run_program(&source);
    assert_eq!(client_receiver.recv().unwrap(), *b"ping");
    client.join().unwrap();
    std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("listener resource should close when its owner is dropped");
}

#[test]
fn tcp_accept_can_spawn_a_branch_after_resuming() {
    const TCP_API: &str = include_str!("../../../std/joky/socket/tcp.jk");
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let (client_sender, client_receiver) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut stream = None;
        for _ in 0..1_000 {
            match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(value) => {
                    stream = Some(value);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        let mut stream = stream.expect("server should accept a client");
        stream.write_all(b"ping").unwrap();
        let mut response = [0_u8; 4];
        stream.read_exact(&mut response).unwrap();
        client_sender.send(response).unwrap();
    });
    let source = format!(
        "{TCP_API}
                 fn main() effects {{ tcp }} {{
                     match tcp.listen(host: \"127.0.0.1\", port: {port}) {{
                         Ok(listener) => {{
                             let socket = listener.accept()!
                             branch {{
                                 match socket.read(4) {{
                                     Ok(value) => match socket.write(value) {{
                                         Ok(_) => {{}}
                                         Err(message) => panic(message)
                                     }}
                                     Err(message) => panic(message)
                                 }}
                             }}
                         }}
                         Err(message) => panic(message)
                     }}
                 }}"
    );
    Compiler::new()
        .unwrap()
        .run_program(&source)
        .expect("accept followed by branch should resume successfully");
    assert_eq!(client_receiver.recv().unwrap(), *b"ping");
    client.join().unwrap();
}

#[test]
fn tcp_accept_can_spawn_function_session_branches_concurrently() {
    const TCP_API: &str = include_str!("../../../std/joky/socket/tcp.jk");
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let (client_sender, client_receiver) = std::sync::mpsc::channel();
    let clients = (0..2)
        .map(|index| {
            let client_sender = client_sender.clone();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut stream = None;
                for _ in 0..1_000 {
                    match std::net::TcpStream::connect(("127.0.0.1", port)) {
                        Ok(value) => {
                            stream = Some(value);
                            break;
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    }
                }
                let mut stream = stream.expect("server should accept a client");
                stream.write_all(b"ping").unwrap();
                let mut response = [0_u8; 4];
                stream.read_exact(&mut response).unwrap();
                client_sender.send((index, response)).unwrap();
            })
        })
        .collect::<Vec<_>>();
    drop(client_sender);
    let source = format!(
        "{TCP_API}
                 fn run_session(socket: TcpStream) effects {{ tcp }} {{
                     let request = socket.read(4)!
                     let _written = socket.write(request)!
                 }}
                 fn main() effects {{ tcp }} {{
                     let listener = tcp.listen(host: \"127.0.0.1\", port: {port})!
                     let first = listener.accept()!
                     branch {{ run_session(first) }}
                     let second = listener.accept()!
                     branch {{ run_session(second) }}
                 }}"
    );
    Compiler::new()
        .unwrap()
        .run_program(&source)
        .expect("function session branches should run concurrently");
    let mut responses = client_receiver.into_iter().collect::<Vec<_>>();
    responses.sort_by_key(|(index, _)| *index);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0].1, *b"ping");
    assert_eq!(responses[1].1, *b"ping");
    for client in clients {
        client.join().unwrap();
    }
}

#[test]
fn tcp_native_resource_drops_without_explicit_close() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").unwrap();
    });
    let source = format!(
            "eff tcp {{
                 @suspends fn connect(host: String, port: UInt16) -> Result(TcpStream, String)
                 @suspends fn read(socket: &TcpStream, max_bytes: UInt64) -> Result(Bytes, String)
                 @suspends fn write(socket: &TcpStream, value: Bytes) -> Result(UInt64, String)
                 @suspends fn close(socket: TcpStream) -> Result(Unit, String)
                 }}
                 fn main() effects {{ tcp }} {{
                     let expected: UInt64 = 4
                     match tcp.connect(host: \"127.0.0.1\", port: {port}) {{
                         Ok(socket) => match tcp.write(socket, Bytes.from_string(\"ping\")) {{
                             Ok(_) => match tcp.read(socket, 4) {{
                                 Ok(value) => if value.length() != expected {{ panic(\"bad native read\") }} else {{ }}
                                 Err(message) => panic(message)
                             }}
                             Err(message) => panic(message)
                         }}
                         Err(message) => panic(message)
                     }}
                 }}"
        );
    run_program(&source);
    server.join().unwrap();
}

#[test]
fn tcp_provider_reads_eof_as_empty_bytes() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || drop(listener.accept().unwrap()));
    let source = format!(
            "eff tcp {{
                 @suspends fn connect(host: String, port: UInt16) -> Result(TcpStream, String)
                 @suspends fn read(socket: &TcpStream, max_bytes: UInt64) -> Result(Bytes, String)
                 @suspends fn write(socket: &TcpStream, value: Bytes) -> Result(UInt64, String)
                 @suspends fn close(socket: TcpStream) -> Result(Unit, String)
                 }}
                 fn main() effects {{ tcp }} {{
                     let expected: UInt64 = 0
                     match tcp.connect(host: \"127.0.0.1\", port: {port}) {{
                         Ok(socket) => match tcp.read(socket, 64) {{
                             Ok(value) => {{
                                 if value.length() != expected {{ panic(\"expected EOF\") }} else {{ }}
                                 match tcp.close(socket) {{ Ok(_) => {{}}, Err(message) => panic(message) }}
                             }}
                             Err(message) => panic(message)
                         }}
                         Err(message) => panic(message)
                     }}
                 }}"
        );
    run_program(&source);
    server.join().unwrap();
}

#[test]
fn tcp_provider_returns_connect_errors() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let source = format!(
        "eff tcp {{
                 @suspends fn connect(host: String, port: UInt16) -> Result(TcpStream, String)
                 @suspends fn read(socket: &TcpStream, max_bytes: UInt64) -> Result(Bytes, String)
                 @suspends fn write(socket: &TcpStream, value: Bytes) -> Result(UInt64, String)
                 @suspends fn close(socket: TcpStream) -> Result(Unit, String)
                 }}
                 fn main() effects {{ tcp }} {{
                     match tcp.connect(host: \"127.0.0.1\", port: {port}) {{
                         Ok(_) => {{ }}
                         Err(_) => {{ }}
                     }}
                 }}"
    );
    run_program(&source);
}

#[test]
fn file_read_returns_an_error_result_for_missing_paths() {
    let path = std::env::temp_dir().join(format!(
        "joky-file-missing-{}-{}.txt",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let source = format!(
        "eff file {{ @suspends fn read(path: String) -> Result(String, String) }}\n\
             fn main() effects {{ file }} {{ match file.read(path: \"{}\") {{\n\
             Ok(contents) => panic(contents)\n\
             Err(message) => println(message)\n\
             }} }}",
        path.to_string_lossy()
    );
    Compiler::new()
        .unwrap()
        .run_program(&source)
        .expect("missing files should become Err values");
}

#[test]
fn compiles_the_user_panic_builtin() {
    Compiler::new()
        .unwrap()
        .run_program(
            "fn main() { if false { panic(\"unreachable\") }; println(\"still running\") }",
        )
        .expect("the user panic builtin should compile as a diverging intrinsic");
}

// The three provider-cancellation regression tests below assert on the
// standalone runtime's process-wide managed-object registry. The registry is
// only populated in test builds and stays noisy while other
// tests run programs in parallel, so each test is `#[ignore]`d and must run
// in an isolated single-threaded process (wired into stress.yml):
//
//   cargo test --lib -- --ignored --test-threads=1 \
//     tcp_read_cancelled_while_queued_discards_the_late_payload \
//     tcp_read_completing_racing_cancellation_keeps_one_winner \
//     tcp_write_argument_share_is_released_after_the_suspend

#[test]
#[ignore = "leak check needs an isolated single-threaded process; see the comment above"]
fn tcp_read_cancelled_while_queued_discards_the_late_payload() {
    // Socket cancellation, queued case: the read is registered with the
    // reactor but no data has arrived when the race winner cancels the loser.
    // The loser must end Cancelled without resuming, and the payload that the
    // server delivers only after the client scope exited must be discarded.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::Write;
        let (mut stream, _) = listener.accept().unwrap();
        // The client is long gone by the time data arrives: any completion
        // routed to the cancelled continuation must be a no-op.
        std::thread::sleep(std::time::Duration::from_millis(150));
        stream.write_all(b"ping").unwrap();
    });
    const TCP_API: &str = include_str!("../../../std/joky/socket/tcp.jk");
    let source = format!(
        "eff time {{ @suspends fn sleep(duration: Duration) -> Unit }}\n\
         {TCP_API}\n\
         fn quick() -> Unit effects {{ time }} {{\n\
             time.sleep(1ms)\n\
         }}\n\
         fn main() effects {{ tcp, time }} {{\n\
             match tcp.connect(host: \"127.0.0.1\", port: {port}) {{\n\
                 Ok(socket) => {{\n\
                     let winner = race {{\n\
                         | {{ match socket.read(4) {{\n\
                             Ok(value) => println(value.length())\n\
                             Err(message) => panic(message)\n\
                         }} }}\n\
                         | quick()\n\
                     }}\n\
                     println(\"cancelled-cleanly\")\n\
                 }}\n\
                 Err(message) => panic(message)\n\
             }}\n\
         }}"
    );
    Compiler::new()
        .unwrap()
        .run_program(&source)
        .expect("a queued read cancelled by the race must not resume or leak");
    server.join().expect("server should finish");
    assert_eq!(
        joky_runtime::host::testing::process_managed_objects(),
        0,
        "a cancelled queued read must not retain its socket resource"
    );
}

#[test]
#[ignore = "leak check needs an isolated single-threaded process; see the comment above"]
fn tcp_read_completing_racing_cancellation_keeps_one_winner() {
    // Socket cancellation, in-flight case: both race arms wait on a read and
    // data arrives on both connections around the winner's completion. The
    // loser's satisfied read races its cancellation; the late result must be
    // dropped exactly once and never resumed.
    for iteration in 0..5 {
        assert_eq!(
            joky_runtime::host::testing::process_managed_objects(),
            0,
            "iteration {iteration} starts with a clean managed registry"
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::Write;
            let (mut first, _) = listener.accept().unwrap();
            let (mut second, _) = listener.accept().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
            first.write_all(b"ping").unwrap();
            second.write_all(b"ping").unwrap();
        });
        const TCP_API: &str = include_str!("../../../std/joky/socket/tcp.jk");
        let source = format!(
            "{TCP_API}\n\
             fn main() effects {{ tcp }} {{\n\
                 match tcp.connect(host: \"127.0.0.1\", port: {port}) {{\n\
                     Ok(first) => match tcp.connect(host: \"127.0.0.1\", port: {port}) {{\n\
                         Ok(second) => {{\n\
                             let winner = race {{\n\
                                 | {{ match first.read(4) {{\n\
                                     Ok(value) => println(value.length())\n\
                                     Err(message) => panic(message)\n\
                                 }} }}\n\
                                 | {{ match second.read(4) {{\n\
                                     Ok(value) => println(value.length())\n\
                                     Err(message) => panic(message)\n\
                                 }} }}\n\
                             }}\n\
                             println(\"done\")\n\
                         }}\n\
                         Err(message) => panic(message)\n\
                     }}\n\
                     Err(message) => panic(message)\n\
                 }}\n\
             }}"
        );
        Compiler::new()
            .unwrap()
            .run_program(&source)
            .expect("exactly one race arm should win with the round-tripped payload");
        server.join().expect("server should finish");
        assert_eq!(
            joky_runtime::host::testing::process_managed_objects(),
            0,
            "iteration {iteration} leaked a managed payload"
        );
    }
}

#[test]
#[ignore = "leak check needs an isolated single-threaded process; see the comment above"]
fn tcp_write_argument_share_is_released_after_the_suspend() {
    // Regression: `Bytes` arguments to a suspending provider call transfer
    // ownership into the continuation's suspend-argument storage, but the
    // cleanup-component table missed the Bytes/MutBytes leaves. The provider
    // copied the payload and the runtime published the result without ever
    // releasing the argument share, leaking one managed object per write.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4];
        stream.read_exact(&mut request).unwrap();
        stream.write_all(b"pong").unwrap();
    });
    let source = format!(
        "eff tcp {{\n\
             @suspends fn connect(host: String, port: UInt16) -> Result(TcpStream, String)\n\
             @suspends fn read(socket: &TcpStream, max_bytes: UInt64) -> Result(Bytes, String)\n\
             @suspends fn write(socket: &TcpStream, value: Bytes) -> Result(UInt64, String)\n\
         }}\n\
         fn main() effects {{ tcp }} {{\n\
             let result = tcp.connect(host: \"127.0.0.1\", port: {port})\n\
             match result {{\n\
                 Ok(socket) => {{\n\
                     let payload = Bytes.from_string(\"ping\")\n\
                     match tcp.write(socket, payload) {{\n\
                         Ok(_) => match tcp.read(socket, 4) {{\n\
                             Ok(value) => println(value.length())\n\
                             Err(message) => panic(message)\n\
                         }}\n\
                         Err(message) => panic(message)\n\
                     }}\n\
                 }}\n\
                 Err(message) => panic(message)\n\
             }}\n\
         }}"
    );
    Compiler::new()
        .unwrap()
        .run_program(&source)
        .expect("a suspending write should not leak its argument share");
    server.join().expect("server should finish");
    assert_eq!(
        joky_runtime::host::testing::process_managed_objects(),
        0,
        "the Bytes argument share must be released once the suspend completes"
    );
}

#[test]
fn tcp_split_supports_pending_read_concurrent_write_and_write_half_eof() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"ping");
        stream.write_all(b"pong").unwrap();
    });
    let source = format!(
        r#"
        {}
        eff time {{ @suspends fn sleep(duration: Duration) -> Unit }}
        fn main() effects {{ tcp, time }} {{
            let connection = tcp.connect("127.0.0.1", {port})!
            let (reader, writer) = connection.split()!
            let responses = parallel {{
                | {{ let value = reader.read(4)!; value.to_string()! }}
                | {{ time.sleep(10ms); let _ = writer.write(Bytes.from_string("ping"))!; writer.close()!; "sent" }}
            }}
            if responses.0 != "pong" {{ panic("split read failed") }}
        }}
    "#,
        include_str!("../../../std/joky/socket/tcp.jk")
    );
    run_program(&source);
    server.join().unwrap();
}

#[test]
fn tcp_split_half_types_restrict_direction_and_preserve_unique_ownership() {
    for function in [
        "fn invalid(stream: &TcpReadHalf) effects { tcp } { let _ = stream.write(Bytes.from_string(\"x\")); () }",
        "fn invalid(stream: &TcpWriteHalf) effects { tcp } { let _ = stream.read(1); () }",
        "fn invalid(stream: TcpStream) effects { tcp } { let halves = stream.split()!; let _ = stream.read(1); () }",
    ] {
        let source = format!("{} {function} fn main() {{}}", include_str!("../../../std/joky/socket/tcp.jk"));
        assert!(Compiler::new().unwrap().run_program(&source).is_err(), "{function}");
    }
}
