use super::tcp_server::{connect, Running};
use super::*;
use std::io::{Read, Write};

fn http_reply(port: u16, fragments: &[&[u8]]) -> String {
    let mut stream = connect(port);
    for bytes in fragments {
        stream.write_all(bytes).unwrap();
    }
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut bytes = Vec::new();
    let read = stream.read_to_end(&mut bytes);
    let reply = String::from_utf8(bytes).unwrap();
    if let Err(error) = read {
        // An early rejection can close with unread input in the kernel. The
        // HTTP client can still consume a complete, explicitly framed error.
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        assert!(
            reply.starts_with("HTTP/1.1 4")
                && reply.ends_with("Content-Length: 0\r\nConnection: close\r\n\r\n"),
            "{reply:?}"
        );
    }
    reply
}

#[test]
fn http_server_serves_json_and_validates_framing() {
    let work = Workspace::new();
    work.write(
        "http_server.jk",
        include_str!("../../examples/http_server.jk"),
    );
    success(&work.cli(&["build", "--task-workers", "2", "http_server.jk"]));
    for backend in ["jit", "interpreter", "aot"] {
        let command = if backend == "aot" {
            Command::new(work.path().join("http_server"))
        } else {
            let mut c = work.command();
            c.args(["run", "http_server.jk"]);
            if backend == "interpreter" {
                c.arg("--interpreter");
            }
            c
        };
        let mut server = Running::start(command);
        let port = server
            .line()
            .strip_prefix("listening ")
            .unwrap()
            .parse()
            .unwrap();
        let health = http_reply(port, &[b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n"]);
        let id = super::tcp_server::response_id(health.split_once("\r\n\r\n").unwrap().0);
        assert_eq!(health.replacen(&format!("X-Request-Id: {id}\r\n"), "", 1), "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}");
        assert!(http_reply(
            port,
            &[
                b"POST /echo HTTP/1.1\r\nHost: localhost\r\ncOnTeNt-LeNgTh: 8\r\n\r\n{",
                b"\"n\": 1}"
            ]
        )
        .ends_with("\r\n\r\n{\"n\":1}"));
        if backend == "jit" {
            for (headers, code) in [
                (
                    "Host: a\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx",
                    400,
                ),
                (
                    "Host: a\r\nTransfer-Encoding: chunked\r\nContent-Length: 1\r\n\r\nx",
                    400,
                ),
                ("Host: a\r\nContent-Length: 4\r\n\r\nab", 400),
                ("Host: a\r\nContent-Length: +1\r\n\r\n", 400),
                (
                    "Host: a\r\nContent-Length: 99999999999999999999\r\n\r\n",
                    413,
                ),
                ("Host: a\r\n folded\r\n\r\n", 400),
                ("Host: a\r\nHost: b\r\n\r\n", 400),
                ("Host: a\n\n", 400),
                ("\r\n", 400),
                ("Host: a\r\nExpect: 100-continue\r\n\r\n", 417),
            ] {
                let request = format!("POST /echo HTTP/1.1\r\n{headers}");
                assert!(
                    http_reply(port, &[request.as_bytes()])
                        .starts_with(&format!("HTTP/1.1 {code} ")),
                    "{headers:?}"
                );
            }
            assert!(http_reply(port, &[b"G@T / HTTP/1.1\r\nHost: a\r\n\r\n"])
                .starts_with("HTTP/1.1 400 "));
            for method in ["PUT", "PATCH", "DELETE"] {
                let request = format!("{method} /unhandled HTTP/1.1\r\nHost: a\r\n\r\n");
                assert!(http_reply(port, &[request.as_bytes()]).starts_with("HTTP/1.1 404 "));
            }
            assert!(http_reply(port, &[b"OTHER / HTTP/1.1\r\nHost: a\r\n\r\n"])
                .starts_with("HTTP/1.1 501 "));
            let many = format!(
                "GET /health HTTP/1.1\r\nHost: a\r\n{}\r\n",
                "x: y\r\n".repeat(64)
            );
            assert!(http_reply(port, &[many.as_bytes()]).starts_with("HTTP/1.1 431 "));
            let long = format!(
                "GET /health HTTP/1.1\r\nHost: a\r\nx: {}\r\n\r\n",
                "a".repeat(16384)
            );
            assert!(http_reply(port, &[long.as_bytes()]).starts_with("HTTP/1.1 431 "));
            assert!(
                http_reply(port, &[b"HEAD /health HTTP/1.1\r\nHost: a\r\n\r\n"])
                    .ends_with("Content-Length: 11\r\nConnection: close\r\n\r\n")
            );
            assert!(http_reply(
                port,
                &[b"POST /echo HTTP/1.1\r\nHost: a\r\nContent-Length: 1\r\n\r\nx"]
            )
            .starts_with("HTTP/1.1 400 "));
        }
        server.signal("-TERM");
        assert!(server.finish().success());
    }
}

#[test]
fn http_read_timeout_and_shutdown_preserve_admitted_handler() {
    let work = Workspace::new();
    work.write("http_probe.jk", r#"
import joky/http
import joky/shutdown
import joky/task
import joky/tcp
fn handle(request: http.Request) @suspends -> http.Response {
    if request.target == "/created" {
        return http.Response { headers: [http.Header { name: "Location", value: "/items/42" }], status: http.Status.Created, content_type: http.ContentType.Text, body: [] }
    }
    if request.target == "/empty" {
        return http.Response { headers: [], status: http.Status.from_code(204)!, content_type: http.ContentType.Text, body: [] }
    }
    if request.target == "/invalid" {
        return http.Response { headers: [http.Header { name: "CONTENT-length", value: "99" }], status: http.Status.Success, content_type: http.ContentType.Text, body: [] }
    }
    if request.target == "/bytes" {
        return http.Response { headers: [], status: http.Status.Success, content_type: http.ContentType.custom("image/png")!, body: request.body }
    }
    println("admitted")
    task.sleep(if request.target == "/stuck" { 60s } else { 300ms })
    http.Response { headers: [], status: http.Status.Success, content_type: http.ContentType.Text, body: "done".into_bytes() }
}
fn main() @suspends -> Int32 {
    assert(http.ContentType.custom("text/html; charset=utf-8").is_some(), "valid media type parameter")
    assert(http.ContentType.custom("multipart/form-data; boundary=\"a;b\"").is_some(), "quoted media parameter")
    assert(http.ContentType.custom("text/plain; charset=\"utf-8").is_none(), "unterminated media parameter")
    assert(http.ContentType.custom("text/html\r\nX: no").is_none() && http.ContentType.custom("invalid").is_none(), "invalid media type")
    let stop = shutdown.create()
    let signals = stop.subscribe_unix()!
    var listener = tcp.listen("127.0.0.1", 0)!
    var options = http.defaults()
    options.read_timeout = 250ms
    options.max_connections = 1
    options.grace = 1s
    println("listening {listener.local_addr()!.port()}")
    match http.serve(listener, &stop, options, handle) { Ok(_) => 0, Err(Deadline) => 7, Err(_) => 1 }
}
"#);
    let start = || {
        let mut command = work.command();
        command.args(["run", "http_probe.jk"]);
        command.stderr(std::fs::File::create(work.path().join("requests.jsonl")).unwrap());
        let server = Running::start(command);
        let port = server
            .line()
            .strip_prefix("listening ")
            .unwrap()
            .parse()
            .unwrap();
        (server, port)
    };
    let (mut server, port) = start();
    let created = http_reply(port, &[b"POST /created HTTP/1.1\r\nHost: a\r\n\r\n"]);
    assert!(
        created.starts_with("HTTP/1.1 201 Created\r\n")
            && created.contains("\r\nLocation: /items/42\r\n")
    );
    let empty = http_reply(port, &[b"DELETE /empty HTTP/1.1\r\nHost: a\r\n\r\n"]);
    assert!(
        empty.starts_with("HTTP/1.1 204 No Content\r\n")
            && empty.ends_with("\r\n\r\n")
            && !empty.contains("Content-Length")
    );
    assert!(
        http_reply(port, &[b"GET /invalid HTTP/1.1\r\nHost: a\r\n\r\n"])
            .starts_with("HTTP/1.1 500 ")
    );
    let mut binary = connect(port);
    binary
        .write_all(b"PUT /bytes HTTP/1.1\r\nHost: a\r\nContent-Length: 4\r\n\r\n\xff\x00\r\n")
        .unwrap();
    let mut packet = Vec::new();
    binary.read_to_end(&mut packet).unwrap();
    assert!(packet
        .windows(b"Content-Type: image/png\r\n".len())
        .any(|line| line == b"Content-Type: image/png\r\n"));
    assert!(packet.ends_with(b"Content-Length: 4\r\nConnection: close\r\n\r\n\xff\x00\r\n"));
    let mut slow = connect(port);
    slow.write_all(b"POST / HTTP/1.1\r\nHost: a\r\nContent-Length: 5\r\n\r\na")
        .unwrap();
    let mut response = String::new();
    slow.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 408 "));
    let timeout_id = super::tcp_server::response_id(&response).to_owned();
    let mut active = connect(port);
    active
        .write_all(b"GET / HTTP/1.1\r\nHost: a\r\n\r\n")
        .unwrap();
    assert_eq!(server.line(), "admitted");
    server.signal("-TERM");
    response.clear();
    active.read_to_string(&mut response).unwrap();
    assert!(response.ends_with("\r\n\r\ndone"));
    assert!(server.finish().success());

    let events = std::fs::read_to_string(work.path().join("requests.jsonl")).unwrap();
    assert!(events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .any(|event| event["request_id"] == timeout_id && event["status"] == 408));

    let (mut server, port) = start();
    let mut reset = connect(port);
    reset
        .write_all(b"GET /reset HTTP/1.1\r\nHost: a\r\n\r\n")
        .unwrap();
    assert_eq!(server.line(), "admitted");
    let socket = tokio::net::TcpSocket::from_std_stream(reset);
    socket.set_zero_linger().unwrap();
    drop(socket);
    server.signal("-TERM");
    assert!(server.finish().success());
    let text = std::fs::read_to_string(work.path().join("requests.jsonl")).unwrap();
    let event: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(event["status"], 200);
    assert_eq!(event["write_outcome"], "failed");

    let (mut server, port) = start();
    let mut stuck = connect(port);
    stuck
        .write_all(b"GET /stuck HTTP/1.1\r\nHost: a\r\n\r\n")
        .unwrap();
    assert_eq!(server.line(), "admitted");
    // No permit is available while the admitted handler is suspended.
    assert_eq!(connect(port).read(&mut [0]).unwrap(), 0);
    server.signal("-TERM");
    assert_eq!(server.finish().code(), Some(7));
    assert_eq!(stuck.read(&mut [0]).unwrap(), 0);
}

#[test]
fn http_client_frames_responses_and_sends_post_once() {
    let work = Workspace::new();
    work.write("client.jk", include_str!("../fixtures/http_client.jk"));
    work.assert_all_backends("client.jk", &[], b"", 0);
}
