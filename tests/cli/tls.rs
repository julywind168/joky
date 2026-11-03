use super::tcp_server::{connect, Running};
use super::*;
use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName};
use std::{
    io::{Read, Write},
    sync::Arc,
};

fn identity(work: &Workspace, prefix: &str) {
    for (name, pem) in [
        ("ca.pem", include_str!("../fixtures/tls/ca.pem")),
        ("server.pem", include_str!("../fixtures/tls/server.pem")),
        (
            "test-only-key.pem",
            include_str!("../fixtures/tls/test-only-key.pem"),
        ),
    ] {
        work.write(&format!("{prefix}{name}"), pem);
    }
}

#[test]
fn tls_stream_authentication_framing_and_cancellation() {
    let work = Workspace::new();
    identity(&work, "");
    work.write("stream.jk", include_str!("../fixtures/tls_stream.jk"));
    success(&work.cli(&["run", "stream.jk"]));
    success(&work.cli(&["run", "--interpreter", "stream.jk"]));
    success(&work.cli(&["build", "stream.jk"]));
    success(
        &Command::new(work.path().join("stream"))
            .current_dir(work.path())
            .output()
            .unwrap(),
    );
}

#[test]
fn https_parallel_server_interoperates_and_drains_incomplete_handshakes() {
    let work = Workspace::new();
    identity(&work, "tests/fixtures/tls/");
    work.write(
        "http_server.jk",
        include_str!("../../examples/http_server.jk"),
    );
    work.write(
        "https_server.jk",
        include_str!("../../examples/https_server.jk"),
    );
    success(&work.cli(&["build", "--task-workers", "2", "https_server.jk"]));
    let mut command = Command::new(work.path().join("https_server"));
    command
        .current_dir(work.path())
        .env("JOKY_TLS_CERT", "tests/fixtures/tls/server.pem")
        .env("JOKY_TLS_KEY", "tests/fixtures/tls/test-only-key.pem");
    let mut server = Running::start(command);
    let port = server
        .line()
        .strip_prefix("listening ")
        .unwrap()
        .parse()
        .unwrap();
    let mut idle = connect(port);
    // Partial TLS record header parks in the TCP reactor.
    idle.write_all(&[22, 3]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(include_bytes!("../fixtures/tls/ca.pem")).unwrap())
        .unwrap();
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let conn =
        rustls::ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap())
            .unwrap();
    let mut client = rustls::StreamOwned::new(conn, connect(port));
    client
        .write_all(b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 8\r\n\r\n{\"n\": 1}")
        .unwrap();
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    let id = super::tcp_server::response_id(reply.split_once("\r\n\r\n").unwrap().0);
    assert_eq!(reply.replacen(&format!("X-Request-Id: {id}\r\n"), "", 1), "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\nConnection: close\r\n\r\n{\"n\":1}");
    work.write("https_client.jk", &r#"
import joky/http
import joky/fs
import joky/tls
fn main() @suspends {
    var ca = fs.open("tests/fixtures/tls/ca.pem", fs.OpenOptions.new())!
    let trusted = tls.ClientConfig.from_ca_pem(ca.read_all_limited(131072)!)!
    let client = http.Client.https("localhost", PORT, trusted)!
    let reply = client.post_json("/echo", "123")!
    assert(reply.status == 200 && String.from_utf8(reply.body)! == "123", "HTTPS exchange")
    let untrusted = http.Client.https("localhost", PORT, tls.ClientConfig.web_roots()!)!
    assert(match untrusted.get("/health") { Err(Handshake(Certificate)) => true, _ => false }, "preserve TLS verification failure")
}
"#.replace("PORT", &port.to_string()));
    success(&work.cli(&["run", "https_client.jk"]));
    server.signal("-TERM");
    assert!(server.finish().success());
    assert_eq!(idle.read(&mut [0]).unwrap(), 0);
}
