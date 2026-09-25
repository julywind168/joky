use super::parity::Package;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[path = "pgsql_scram.rs"]
mod scram;

struct Peer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Peer {
    fn new(mut handle: impl FnMut(TcpStream) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(15)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(15)))
                            .unwrap();
                        handle(stream);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let result = self.thread.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn frame(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut data = vec![tag];
    data.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    data.extend_from_slice(body);
    data
}

fn startup(stream: &mut TcpStream) -> Vec<u8> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let length = i32::from_be_bytes(length);
    assert!((8..4096).contains(&length));
    let mut body = vec![0; length as usize - 4];
    stream.read_exact(&mut body).unwrap();
    assert_eq!(&body[..4], &196608_i32.to_be_bytes());
    assert!(body
        .windows(b"client_encoding\0UTF8\0\0".len())
        .any(|s| s == b"client_encoding\0UTF8\0\0"));
    body
}

fn query(stream: &mut TcpStream) -> String {
    let mut header = [0; 5];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(header[0], b'Q');
    let length = i32::from_be_bytes(header[1..].try_into().unwrap());
    assert!((5..4096).contains(&length));
    let mut data = vec![0; length as usize - 4];
    stream.read_exact(&mut data).unwrap();
    assert_eq!(data.pop(), Some(0));
    String::from_utf8(data).unwrap()
}

fn handshake() -> Vec<u8> {
    [
        frame(b'R', &0_i32.to_be_bytes()),
        frame(b'S', b"client_encoding\0UTF8\0"),
        frame(b'K', &[0, 0, 0, 42, 1, 2, 3, 4]),
        frame(b'Z', b"I"),
    ]
    .concat()
}

fn description() -> Vec<u8> {
    let mut body = 4_i16.to_be_bytes().to_vec();
    for name in ["answer", "greeting", "missing", "empty"] {
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&0_u32.to_be_bytes());
        body.extend_from_slice(&0_i16.to_be_bytes());
        body.extend_from_slice(&25_u32.to_be_bytes());
        body.extend_from_slice(&(-1_i16).to_be_bytes());
        body.extend_from_slice(&(-1_i32).to_be_bytes());
        body.extend_from_slice(&0_i16.to_be_bytes());
    }
    frame(b'T', &body)
}

fn data_row(first: &str) -> Vec<u8> {
    let mut body = 4_i16.to_be_bytes().to_vec();
    for value in [Some(first), Some("中文"), None, Some("")] {
        match value {
            Some(text) => {
                body.extend_from_slice(&(text.len() as i32).to_be_bytes());
                body.extend_from_slice(text.as_bytes());
            }
            None => body.extend_from_slice(&(-1_i32).to_be_bytes()),
        }
    }
    frame(b'D', &body)
}

fn assert_closed(stream: &mut TcpStream) {
    let mut byte = [0];
    match stream.read(&mut byte) {
        Ok(0) => (),
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => (),
        other => panic!("connection was not released: {other:?}"),
    }
}

#[test]
fn pgsql_binary_jit_and_aot() {
    Package::new(
        "pgsql-binary",
        &[("main.jk", include_str!("../fixtures/binary.jk"))],
    )
    .check("binary ok\n", None, &[]);
}

#[test]
fn pgsql_fragmentation_null_utf8_and_error_recovery() {
    let peer = Peer::new(|mut stream| {
        startup(&mut stream);
        // Split every header and body, then coalesce whole query responses.
        for byte in handshake() {
            stream.write_all(&[byte]).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        assert!(query(&mut stream).starts_with("SELECT 1"));
        stream
            .write_all(
                &[
                    description(),
                    data_row("1"),
                    frame(b'C', b"SELECT 1\0"),
                    frame(b'Z', b"I"),
                ]
                .concat(),
            )
            .unwrap();
        assert!(query(&mut stream).contains("does_not_exist"));
        stream
            .write_all(
                &[
                    frame(b'N', b"SNOTICE\0Mnotice\0\0"),
                    frame(b'E', b"SERROR\0C42P01\0Mmissing relation\0\0"),
                    frame(b'Z', b"I"),
                ]
                .concat(),
            )
            .unwrap();
        assert_eq!(query(&mut stream), "SELECT 2");
        stream
            .write_all(
                &[
                    description(),
                    data_row("2"),
                    frame(b'C', b"SELECT 1\0"),
                    frame(b'Z', b"I"),
                ]
                .concat(),
            )
            .unwrap();
        assert_closed(&mut stream);
    });
    let source = include_str!("../../examples/networking/pgsql.jk")
        .replace("\"5432\"", &format!("\"{}\"", peer.port));
    // Avoid depending on the developer's JOKY_PG_* environment.
    let source = source
        .replace(
            "env.get(\"JOKY_PG_PORT\")?.unwrap_or",
            "Option(String).None.unwrap_or",
        )
        .replace(
            "env.get(\"JOKY_PG_USER\")?.unwrap_or",
            "Option(String).None.unwrap_or",
        )
        .replace(
            "env.get(\"JOKY_PG_DATABASE\")?.unwrap_or",
            "Option(String).None.unwrap_or",
        );
    Package::new("pgsql-wire", &[("main.jk", &source)]).check("1\n中文\ntrue\n\n2\n", None, &[]);
}

#[test]
fn pgsql_rejects_malformed_messages_and_releases_connections() {
    let peer = Peer::new(|mut stream| {
        let request = startup(&mut stream);
        let user = request[4..].split(|b| *b == 0).nth(1).unwrap();
        match user {
            b"auth" => {
                stream
                    .write_all(&frame(b'R', &10_i32.to_be_bytes()))
                    .unwrap();
            }
            b"short_length" => {
                stream.write_all(b"R\0\0\0\x03").unwrap();
            }
            b"oversized" => {
                stream.write_all(b"R\x7f\xff\xff\xff").unwrap();
            }
            b"early_ready" => {
                stream.write_all(&frame(b'Z', b"I")).unwrap();
            }
            b"truncated" => {
                stream.write_all(b"R\0\0\0\x08\0").unwrap();
                stream.shutdown(std::net::Shutdown::Write).unwrap();
            }
            _ => {
                stream.write_all(&handshake()).unwrap();
                query(&mut stream);
                let response = match user {
                    b"bad_column" => {
                        [description(), frame(b'D', &[0, 4, 255, 255, 255, 254])].concat()
                    }
                    b"bad_utf8" => {
                        let mut row = data_row("1");
                        row[11] = 255;
                        [description(), row].concat()
                    }
                    b"row_limit" => [description(), data_row("1"), data_row("2")].concat(),
                    b"bad_ready" => frame(b'Z', b"X"),
                    b"premature_ready" => [description(), frame(b'Z', b"I")].concat(),
                    b"unterminated" => frame(b'E', b"Mno terminator"),
                    b"copy" => frame(b'G', &[]),
                    _ => panic!("unknown case"),
                };
                stream.write_all(&response).unwrap();
            }
        }
        assert_closed(&mut stream);
    });
    let source = format!(
        r#"
import joky/pgsql
import joky/socket/tls
import joky/socket/tcp
fn fails(name: String) -> Result(Unit, String) effects {{ tcp, tls }} {{
    let result = pgsql.connect_trust(pgsql.PgConfig(port: {}, user: name, database: "postgres", max_rows: 1))
    let failed = match result {{
        Err(message) => message
        Ok(connection) => match connection.query("SELECT 1") {{
            Err(message) => message
            Ok(_) => {{ panic("malformed response accepted"); "accepted" }}
        }}
    }}
    println(name + ": " + failed)
    Ok(())
}}
fn main() -> Result(Unit, String) effects {{ tcp, tls }} {{
    for name in List("auth", "short_length", "oversized", "early_ready", "truncated", "bad_column", "bad_utf8", "row_limit", "bad_ready", "premature_ready", "unterminated", "copy") {{ fails(name)? }}
    Ok(())
}}
"#,
        peer.port
    );
    Package::new("pgsql-malformed", &[("main.jk", &source)]).check(
        concat!(
            "auth: pgsql: only trust authentication is supported by connect_trust\n",
            "short_length: pgsql: invalid message length\n",
            "oversized: pgsql: message limit exceeded\n",
            "early_ready: pgsql: ready before authentication\n",
            "truncated: buffered TCP: truncated input\n",
            "bad_column: pgsql: invalid column length\n",
            "bad_utf8: pgsql: invalid UTF-8 column\n",
            "row_limit: pgsql: result row limit exceeded\n",
            "bad_ready: pgsql: invalid transaction status\n",
            "premature_ready: pgsql: ready before command completion\n",
            "unterminated: binary: missing string terminator\n",
            "copy: pgsql: unsupported query response (COPY and binary results are not supported)\n"
        ),
        None,
        &[],
    );
}

#[test]
fn pgsql_cancellation_closes_the_in_flight_connection() {
    let signal = TcpListener::bind("127.0.0.1:0").unwrap();
    signal.set_nonblocking(true).unwrap();
    let signal_port = signal.local_addr().unwrap().port();
    let peer = Peer::new(move |mut stream| {
        startup(&mut stream);
        stream.write_all(&handshake()).unwrap();
        assert_eq!(query(&mut stream), "SELECT waiting");
        stream.write_all(b"T\0\0").unwrap();
        // Only release the race winner once the server has received the query.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut wake = loop {
            match signal.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "no cancellation waiter"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("signal accept: {error}"),
            }
        };
        wake.set_nonblocking(false).unwrap();
        wake.write_all(b"!").unwrap();
        assert_closed(&mut stream);
    });
    let source = format!(
        r#"
import joky/pgsql
import joky/socket/tls
import joky/socket/tcp
fn main() -> Result(Unit, String) effects {{ tcp, tls }} {{
    let connection = pgsql.connect_trust(pgsql.PgConfig(port: {}, user: "postgres", database: "postgres"))?
    let winner = race {{
        | {{ let _ = connection.query("SELECT waiting")!; "unexpected" }}
        | {{
            let signal = tcp.connect("127.0.0.1", {signal_port})!
            let _ = signal.read(1)!
            signal.close()!
            "cancelled"
        }}
    }}
    println(winner)
    Ok(())
}}
"#,
        peer.port
    );
    Package::new("pgsql-cancel", &[("main.jk", &source)]).check("cancelled\n", None, &[]);
}

#[test]
fn pgsql_buffered_read_ahead_eof_and_truncation() {
    let peer = Peer::new(|mut stream| {
        stream.write_all(b"abcdef").unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        assert_closed(&mut stream);
    });
    let source = format!(
        r#"
import joky/socket/tls
import joky/socket/tcp
import joky/socket/buffered
fn main() -> Result(Unit, String) effects {{ tcp, tls }} {{
    let stream = tcp.connect("127.0.0.1", {})?
    let (first, state) = buffered.read_exact(stream, buffered.empty(), 2, 8)?
    if first.to_string()! != "ab" {{ panic("first chunk") }}
    let (empty, state) = buffered.read_exact(stream, state, 0, 8)?
    if !empty.is_empty() {{ panic("zero read") }}
    // A limit failure must not consume any buffered or socket input.
    if buffered.read_exact(stream, state, 9, 8).is_ok() {{ panic("read limit") }}
    let (last, state) = buffered.read_exact(stream, state, 4, 8)?
    if last.to_string()! != "cdef" {{ panic("read-ahead lost") }}
    match buffered.read_exact(stream, state, 1, 8) {{
        Ok(_) => panic("EOF accepted")
        Err(error) => println(error)
    }}
    stream.close()?
    let second = tcp.connect("127.0.0.1", {})?
    match buffered.read_exact(second, buffered.empty(), 7, 8) {{
        Ok(_) => panic("truncation accepted")
        Err(error) => println(error)
    }}
    second.close()?
    Ok(())
}}
"#,
        peer.port, peer.port
    );
    Package::new("pgsql-buffered", &[("main.jk", &source)]).check(
        "buffered TCP: end of stream\nbuffered TCP: truncated input\n",
        None,
        &[],
    );
}

#[path = "pgsql_tls.rs"]
mod tls_tests;
