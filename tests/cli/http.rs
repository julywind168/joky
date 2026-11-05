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
fn http_request_and_response_helpers_preserve_boundaries() {
    let work = Workspace::new();
    work.write("url.jk", r##"
import joky/url
import joky/http.{type Service}
import joky/log
import joky/uuid
import joky/mem
import joky/task
fn pairs(var actual: Array<(String, String)>, var expected: Array<(String, String)>) {
    assert(actual.len() == expected.len(), "pair count")
    while !expected.is_empty() {
        let (key, value) = actual.take_last()
        let (expected_key, expected_value) = expected.take_last()
        assert(key == expected_key && value == expected_value, "pair order and content")
    }
}
fn request(target: String) -> http.Request {
    http.Request {
        context: http.RequestContext { request_id: uuid.Uuid.parse("00112233-4455-6677-8899-aabbccddeeff")!, logger: log.Logger.new(log.Level.Info) },
        method: http.Method.Get, target, headers: [], body: []
    }
}
fn components() {
    assert(url.encode_component("AZaz09-._~") == "AZaz09-._~", "unreserved ASCII")
    assert(url.encode_component(" /?&=+#%") == "%20%2F%3F%26%3D%2B%23%25", "reserved bytes")
    assert(url.encode_component("中🙂") == "%E4%B8%AD%F0%9F%99%82", "UTF-8 encoding")
    assert(url.decode_component("%e4%b8%ad%2f+")! == "中/+", "hex case and literal plus")
    assert(url.decode_component("%252F")! == "%2F", "decode once")
    for text in ["", "a b+c", "中文🙂", "\0", "x&y=z?/#", "%'()*!"] {
        assert(url.decode_component(url.encode_component(&text))! == text, "round trip")
    }
    for text in ["%", "%1", "%GG", "%0g"] {
        assert(match url.decode_component(&text) { Err(InvalidEscape(offset)) => offset == 0, _ => false }, "invalid escape")
    }
    for text in ["%FF", "%C0%AF", "%E4%B8", "%ED%A0%80"] {
        assert(match url.decode_component(&text) { Err(Utf8(_)) => true, _ => false }, "strict UTF-8")
    }
}
fn queries() {
    pairs(url.parse_query("q=a%26b&tag=x&tag=y&x%3Dy=a=b&plus=a+b%2Bc")!,
        [("q", "a&b"), ("tag", "x"), ("tag", "y"), ("x=y", "a=b"), ("plus", "a b+c")])
    pairs(url.parse_query("&&bare&empty=&=value&=&")!, [("bare", ""), ("empty", ""), ("", "value"), ("", "")])
    pairs(url.parse_query("%E4%B8%AD=🙂&a+b=%2526&%2B=+")!, [("中", "🙂"), ("a b", "%26"), ("+", " ")])
    assert(url.parse_query("")!.is_empty() && url.parse_query("&&")!.is_empty(), "empty query")
    for text in ["ok=1&bad=%", "ok=1&%=bad", "x=%&y=41", "x=%FF"] {
        assert(url.parse_query(&text).is_err(), "reject entire query")
    }
    assert(match url.parse_query("ok=1&bad=%") { Err(InvalidEscape(offset)) => offset == 9, _ => false }, "query source offset")
}
fn requests() {
    let value = request("/a%2Fb?q=a%26b&tag=x&tag=y")
    assert(value.path() == "/a%2Fb", "raw encoded path")
    pairs(value.query_pairs()!, [("q", "a&b"), ("tag", "x"), ("tag", "y")])
    assert(value.target == "/a%2Fb?q=a%26b&tag=x&tag=y", "queries borrow request")
    assert(request("/plain").query_pairs()!.is_empty() && request("/plain?").query_pairs()!.is_empty(), "absent and empty query")
    assert(request("/plain").path() == "/plain" && request("/plain?").path() == "/plain", "plain paths")
    assert(request("/bad?x=%FF").query_pairs().is_err(), "request decode error")
    pairs(request("/path?x=a?b").query_pairs()!, [("x", "a?b")])
}
#derive(ToJson, FromJson)
struct ErrorBody { error: String }
fn decoding() {
    let value = http.Request {
        headers: [http.Header { name: "X-Item", value: "kept" }],
        body: r#"{"error":"bad"}"#.into_bytes(), ..request("/decode?x=1")
    }
    let id = value.context.request_id
    let decoded = value.json::<ErrorBody>()!
    assert(value.json::<ErrorBody>()!.error == "bad" && value.body.len() == 15, "shared request supports repeated decoding")
    assert(value.context.request_id == id && value.header("x-item")! == "kept"
        && value.path() == "/decode" && value.method == http.Method.Get, "decode preserves request metadata")
    mem.drop(value)
    assert(decoded.error == "bad", "decoded value outlives request")
    var invalid = request("/")
    invalid.body = "42".into_bytes()
    assert(match invalid.json::<ErrorBody>() { Err(Mapping(_)) => true, _ => false }, "mapping error remains observable")
    assert(invalid.body.len() == 2 && invalid.body[0] == 52, "mapping failure preserves body")
    invalid.body = [255]
    assert(match invalid.json::<ErrorBody>() { Err(Syntax(_)) => true, _ => false }, "syntax error remains observable")
    assert(invalid.body.len() == 1 && invalid.body[0] == 255, "syntax failure preserves body")
}
fn helpers() {
    var value = request("/")
    assert(value.method == http.Method.Get && value.method != http.Method.Post, "typed method equality")
    assert("{value.method}" == "GET" && "{http.Method.Options}" == "OPTIONS", "method wire names")
    value.headers.push(http.Header { name: "X-Item", value: "first" })
    value.headers.push(http.Header { name: "x-item", value: "second" })
    value.headers.push(http.Header { name: "Empty", value: "" })
    let first = { let name = "X-ITEM"; value.header(&name)! }
    assert(first == "first", "borrowed first value, independent of name lifetime")
    assert(value.header("empty")! == "" && value.header("missing").is_none(), "empty and missing headers")
    var all = value.header_all("X-item")
    assert(all.len() == 2 && all.take_last() == "second" && all.take_last() == "first", "header order and duplicates")
    assert(value.header_all("missing").is_empty(), "missing header list")
    let response = http.text(http.Status.Ok, "hello")
        .with_header("Set-Cookie", "a=1").with_header("Set-Cookie", "b=2")
    assert(response.headers.len() == 2 && response.headers[0].value == "a=1" && response.headers[1].value == "b=2", "append headers")
    let http.Response { status, content_type, headers, body } = response
    assert(String.from_utf8(body)! == "hello", "text body")
    assert(http.empty(http.Status.NoContent).body.is_empty(), "empty body")
    let payload = ErrorBody { error: "quote\"\n" }
    let response = http.json(http.Status.BadRequest, &payload)!
    let http.Response { status, content_type, headers, body } = response
    assert(match content_type { Json => true, _ => false }, "JSON content type")
    assert(String.from_utf8(body)! == r#"{"error":"quote\"\n"}"#, "JSON escaping")
    assert(payload.error == "quote\"\n", "JSON borrows input")
    assert(http.json(http.Status.Ok, Float64.NAN).is_err(), "encoding failure remains observable")
}
fn routed_request(method: http.Method, target: String) -> http.Request { http.Request { method, ..request(target) } }
fn routed(request: http.Request) @suspends -> http.Response {
    task.yield_now()
    http.text(http.Status.Ok, "{request.method} {request.target}")
}
fn alternate(request: http.Request) @suspends -> http.Response { http.empty(http.Status.Accepted) }
fn routing() @suspends {
    var router = http.Router.new()
    router.post("/item", routed)!
    router.get("/item", routed)!
    router.add(http.Method.Patch, "/a%2Fb", routed)!
    assert(match router.get("/item", alternate) { Err(DuplicateRoute) => true, _ => false }, "reject duplicate route")
    for path in ["", "relative", "/item?x=1", "/item#fragment", "/bad\npath", "/你"] {
        assert(match router.get(path, routed) { Err(InvalidPath) => true, _ => false }, "reject invalid route path")
    }
    assert(String.from_utf8(router.handle(request("/item?x=1")).body)! == "GET /item?x=1", "query does not change route or request")
    let denied = router.handle(routed_request(http.Method.Options, "/item"))
    assert(match denied.status { MethodNotAllowed => true, _ => false }, "known path with unsupported method")
    assert(denied.headers.len() == 1 && denied.headers[0].name == "Allow" && denied.headers[0].value == "GET, HEAD, POST", "deterministic allowed methods include implicit HEAD")
    for path in ["/missing", "/Item", "/item/", "/a/b"] {
        assert(match router.handle(request(path)).status { NotFound => true, _ => false }, "exact path matching")
    }
    assert(String.from_utf8(router.handle(routed_request(http.Method.Patch, "/a%2Fb")).body)! == "PATCH /a%2Fb", "encoded path remains literal")
    let shared = router.share()
    router.head("/item", alternate)!
    router.options("/item", alternate)!
    assert(match router.handle(routed_request(http.Method.Head, "/item")).status { Accepted => true, _ => false }, "explicit HEAD overrides GET fallback")
    assert(match router.handle(routed_request(http.Method.Options, "/item")).status { Accepted => true, _ => false }, "explicit OPTIONS handler")
    mem.drop(router)
    assert(String.from_utf8(shared.handle(routed_request(http.Method.Head, "/item")).body)! == "HEAD /item", "shared table owns routes and preserves method in GET fallback")
}
fn main() @suspends { components(); queries(); requests(); helpers(); decoding(); routing() }
"##);
    work.assert_all_backends("url.jk", &[], b"", 0);
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
        let mut command = if backend == "aot" {
            Command::new(work.path().join("http_server"))
        } else {
            let mut c = work.command();
            c.args(["run", "http_server.jk"]);
            if backend == "interpreter" {
                c.arg("--interpreter");
            }
            c
        };
        let log_path = work.path().join(format!("requests-{backend}.jsonl"));
        command.stderr(std::fs::File::create(&log_path).unwrap());
        let mut server = Running::start(command);
        let port = server
            .line()
            .strip_prefix("listening ")
            .unwrap()
            .parse()
            .unwrap();
        let health = http_reply(
            port,
            &[b"GET /health?probe=1 HTTP/1.1\r\nHost: localhost\r\n\r\n"],
        );
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
        let missing = http_reply(
            port,
            &[b"OPTIONS /unhandled?secret=1 HTTP/1.1\r\nHost: a\r\n\r\n"],
        );
        assert!(missing.starts_with("HTTP/1.1 404 "));
        let denied = http_reply(
            port,
            &[b"POST /health?secret=1 HTTP/1.1\r\nHost: a\r\n\r\n"],
        );
        assert!(
            denied.starts_with("HTTP/1.1 405 Method Not Allowed\r\n")
                && denied.contains("\r\nAllow: GET, HEAD\r\n")
        );
        let head = http_reply(port, &[b"HEAD /health HTTP/1.1\r\nHost: a\r\n\r\n"]);
        assert!(head.ends_with("Content-Length: 11\r\nConnection: close\r\n\r\n"));
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
            for method in ["OTHER", "get", "options"] {
                let request = format!("{method} / HTTP/1.1\r\nHost: a\r\n\r\n");
                assert!(http_reply(port, &[request.as_bytes()]).starts_with("HTTP/1.1 501 "));
            }
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
            assert!(http_reply(
                port,
                &[b"POST /echo HTTP/1.1\r\nHost: a\r\nContent-Length: 1\r\n\r\nx"]
            )
            .starts_with("HTTP/1.1 400 "));
        }
        server.signal("-TERM");
        assert!(server.finish().success());
        let events: Vec<serde_json::Value> = std::fs::read_to_string(log_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (reply, method, path) in [
            (&health, "GET", "/health"),
            (&denied, "POST", "/health"),
            (&missing, "OPTIONS", "/unhandled"),
            (&head, "HEAD", "/health"),
        ] {
            let id = super::tcp_server::response_id(reply);
            let event = events
                .iter()
                .find(|event| event["request_id"] == id)
                .unwrap();
            assert_eq!(event["method"], method);
            assert_eq!(event["path"], path);
            assert!(event.get("route").is_none());
        }
    }
}

#[test]
fn http_read_timeout_and_shutdown_preserve_admitted_handler() {
    let work = Workspace::new();
    work.write("http_probe.jk", r#"
import joky/http
import joky/log
import joky/process
import joky/shutdown
import joky/task
import joky/tcp
fn handle(request: http.Request) @suspends -> http.Response {
    if request.method == http.Method.Options {
        return http.text(http.Status.Ok, "{request.method}")
    }
    if let Some(code) = request.target.strip_prefix("/status/") {
        return http.empty(http.Status.from_code(UInt16.parse(&code)!)!)
    }
    if request.target == "/created" {
        return http.empty(http.Status.Created).with_header("Location", "/items/42")
    }
    if request.target == "/empty" {
        request.context.warn("app.empty", [])
        return http.empty(http.Status.from_code(204)!)
    }
    if request.target == "/invalid" {
        return http.empty(http.Status.Ok).with_header("CONTENT-length", "99")
    }
    if request.target == "/bytes" {
        return http.Response { headers: [], status: http.Status.Ok, content_type: http.ContentType.custom("image/png")!, body: request.body }
    }
    println("admitted")
    task.sleep(if request.target == "/stuck" { 60s } else { 300ms })
    http.text(http.Status.Ok, "done")
}
fn main() @suspends -> Int32 {
    assert(http.ContentType.custom("text/html; charset=utf-8").is_some(), "valid media type parameter")
    assert(http.ContentType.custom("multipart/form-data; boundary=\"a;b\"").is_some(), "quoted media parameter")
    assert(http.ContentType.custom("text/plain; charset=\"utf-8").is_none(), "unterminated media parameter")
    assert(http.ContentType.custom("text/html\r\nX: no").is_none() && http.ContentType.custom("invalid").is_none(), "invalid media type")
    let stop = shutdown.create()
    let signals = stop.subscribe_unix()!
    var listener = tcp.listen("127.0.0.1", 0)!
    var options = http.Options.default()
    if process.env("JOKY_TEST_HTTP_MINIMUM_WARN")!.is_some() { options.logger = log.Logger.new(log.Level.Warn) }
    options.read_timeout = 250ms
    options.max_connections = 1
    options.grace = 1s
    println("listening {listener.local_addr()!.port()}")
    match http.serve(listener, &stop, options, handle) { Ok(_) => 0, Err(Deadline) => 7, Err(_) => 1 }
}
"#);
    let start = |quiet: bool| {
        let mut command = work.command();
        command.args(["run", "http_probe.jk"]);
        if quiet {
            command.env("JOKY_TEST_HTTP_MINIMUM_WARN", "1");
        } else {
            command.env_remove("JOKY_TEST_HTTP_MINIMUM_WARN");
        }
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
    let (mut server, port) = start(false);
    assert!(
        http_reply(port, &[b"OPTIONS / HTTP/1.1\r\nHost: a\r\n\r\n"]).ends_with("\r\n\r\nOPTIONS")
    );
    for (code, reason) in [
        (401, "Unauthorized"),
        (403, "Forbidden"),
        (405, "Method Not Allowed"),
        (422, "Unprocessable Content"),
        (429, "Too Many Requests"),
        (500, "Internal Server Error"),
    ] {
        let request = format!("GET /status/{code} HTTP/1.1\r\nHost: a\r\n\r\n");
        assert!(http_reply(port, &[request.as_bytes()])
            .starts_with(&format!("HTTP/1.1 {code} {reason}\r\n")));
    }
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

    let (mut server, port) = start(false);
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

    let (mut server, port) = start(false);
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
    let (mut server, port) = start(true);
    let reply = http_reply(port, &[b"DELETE /empty HTTP/1.1\r\nHost: a\r\n\r\n"]);
    let id = super::tcp_server::response_id(&reply);
    server.signal("-TERM");
    assert!(server.finish().success());
    let events = std::fs::read_to_string(work.path().join("requests.jsonl")).unwrap();
    let event: serde_json::Value = serde_json::from_str(&events).unwrap();
    assert_eq!(event["event"], "app.empty");
    assert_eq!(event["request_id"], id);
}

#[test]
fn http_client_frames_responses_and_sends_post_once() {
    let work = Workspace::new();
    work.write("client.jk", include_str!("../fixtures/http_client.jk"));
    work.assert_all_backends("client.jk", &[], b"", 0);
}
