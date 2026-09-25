use super::*;
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use rustls::{pki_types::PrivatePkcs8KeyDer, ServerConfig, ServerConnection, StreamOwned};

fn certificates(expired: bool, tls12: bool) -> (String, Arc<ServerConfig>) {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().unwrap();
    let ca = params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(params, ca_key);
    let mut params = CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    if expired {
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        params.not_after = rcgen::date_time_ymd(2001, 1, 1);
    }
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &issuer).unwrap();
    let versions = if tls12 {
        vec![&rustls::version::TLS12]
    } else {
        vec![&rustls::version::TLS13]
    };
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&versions)
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
    (ca.pem(), Arc::new(config))
}

fn package(name: &str, body: &str, ca: &str) -> Package {
    let source = format!(
        r#"
import joky/socket/tcp
import joky/socket/tls
import joky/mut_bytes
import joky/pgsql
fn main() -> Result(Unit, String) effects {{ tcp, tls }} {{
    let ca = include_bytes("ca.pem")
    {body}
    Ok(())
}}
"#
    );
    let package = Package::new(name, &[("main.jk", &source)]);
    std::fs::write(package.root.join("src/ca.pem"), ca).unwrap();
    package
}

#[test]
fn tls_verified_fragmented_io_and_close_notify() {
    for tls12 in [false, true] {
        let (ca, config) = certificates(false, tls12);
        let peer = Peer::new(move |stream| {
            let connection = ServerConnection::new(config.clone()).unwrap();
            let mut secure = StreamOwned::new(connection, stream);
            let mut input = vec![0; 1_048_576];
            secure.read_exact(&mut input).unwrap();
            assert!(input.iter().all(|b| *b == 65));
            for chunk in input.chunks(313) {
                secure.write_all(chunk).unwrap();
            }
            secure.flush().unwrap();
            secure.conn.send_close_notify();
            while secure.conn.wants_write() {
                secure.conn.write_tls(&mut secure.sock).unwrap();
            }
            assert_eq!(secure.read(&mut [0]).unwrap(), 0);
        });
        let body = format!(
            r#"
    let stream = tls.upgrade(tcp.connect("127.0.0.1", {})?, "localhost", ca)?
    if !stream.read(0)?.is_empty() {{ panic("zero read") }}
    if stream.write(b"")? != 0u64 {{ panic("zero write") }}
    let bytes = MutBytes()
    var i = 0u64
    while i < 1048576u64 {{ bytes.push(65); i += 1 }}
    let data = bytes.to_bytes()
    if stream.write(data)? != data.length() {{ panic("write count") }}
    let received = MutBytes()
    loop {{
        let chunk = stream.read(997)?
        if chunk.is_empty() {{ break }}
        received.extend(chunk)
    }}
    if received.to_bytes() != data {{ panic("TLS payload") }}
    stream.close()?
    println("tls io ok")
"#,
            peer.port
        );
        package(&format!("tls-io-{tls12}"), &body, &ca).check_cached("tls io ok\n", None, &[]);
    }
}

#[test]
fn tls_rejects_untrusted_wrong_name_expired_and_malformed_inputs() {
    for (label, expired, name, use_ca, expected) in [
        ("unknown-ca", false, "localhost", false, "UnknownIssuer"),
        (
            "wrong-name",
            false,
            "other.example",
            true,
            "not valid for name",
        ),
        ("expired", true, "localhost", true, "expired"),
        ("bad-name", false, "bad/name", true, "invalid server name"),
    ] {
        let (ca, config) = certificates(expired, false);
        let peer = Peer::new(move |mut stream| {
            let mut connection = ServerConnection::new(config.clone()).unwrap();
            while connection.is_handshaking() {
                if connection.complete_io(&mut stream).is_err() {
                    return;
                }
            }
            panic!("invalid peer was accepted");
        });
        let ca_expr = if use_ca { "ca" } else { "Bytes()" };
        let body = format!(
            r#"
    match tls.upgrade(tcp.connect("127.0.0.1", {})?, "{name}", {ca_expr}) {{
        Ok(stream) => {{ let _ = stream; panic("accepted invalid certificate") }}
        Err(message) => if !message.contains("{expected}") {{ panic(message) }}
    }}
    println("rejected")
"#,
            peer.port
        );
        package(&format!("tls-{label}"), &body, &ca).check_cached("rejected\n", None, &[]);
    }
    let peer = Peer::new(move |mut stream| assert_closed(&mut stream));
    let body = format!(
        r#"
    match tls.upgrade(tcp.connect("127.0.0.1", {})?, "localhost", b"not a PEM") {{
        Ok(stream) => {{ let _ = stream; panic("accepted invalid CA") }}
        Err(message) => if !message.contains("no certificates") {{ panic(message) }}
    }}
    println("rejected")
"#,
        peer.port
    );
    package("tls-malformed-ca", &body, "").check_cached("rejected\n", None, &[]);
}

#[test]
fn tls_rejects_truncated_handshake_and_unclean_eof() {
    let peer = Peer::new(|mut stream| {
        let mut hello = [0; 4096];
        assert!(stream.read(&mut hello).unwrap() > 0);
        stream.write_all(&[22, 3, 3, 0, 99, 2]).unwrap();
    });
    let body = format!(
        r#"
    match tls.upgrade(tcp.connect("127.0.0.1", {})?, "localhost", ca) {{
        Ok(stream) => {{ let _ = stream; panic("accepted truncation") }}
        Err(message) => if !message.contains("EOF") {{ panic(message) }}
    }}
    println("truncated")
"#,
        peer.port
    );
    package("tls-truncated", &body, "").check_cached("truncated\n", None, &[]);
    let (ca, config) = certificates(false, false);
    let peer = Peer::new(move |stream| {
        let mut secure = StreamOwned::new(ServerConnection::new(config.clone()).unwrap(), stream);
        let mut byte = [0];
        secure.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [65]);
    });
    let body = format!(
        r#"
    let stream = tls.upgrade(tcp.connect("127.0.0.1", {})?, "127.0.0.1", ca)?
    let _ = stream.write(b"A")?
    match stream.read(1) {{
        Ok(data) => {{ let _ = data; panic("accepted unclean EOF") }}
        Err(message) => if !message.contains("EOF") {{ panic(message) }}
    }}
    if stream.read(1).is_ok() {{ panic("reused poisoned connection") }}
    println("unclean")
"#,
        peer.port
    );
    package("tls-unclean", &body, &ca).check_cached("unclean\n", None, &[]);
}

#[test]
fn pgsql_tls_negotiation_rejects_downgrade_and_injection() {
    for reply in [b"N".as_slice(), b"X", b"", b"Splaintext"] {
        let peer = Peer::new(move |mut stream| {
            let mut request = [0; 8];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(request, [0, 0, 0, 8, 4, 210, 22, 47]);
            stream.write_all(reply).unwrap();
            if reply.is_empty() {
                return;
            }
            if reply[0] == b'S' {
                let mut hello = [0; 5];
                stream.read_exact(&mut hello).unwrap();
                assert_eq!(hello[0], 22);
            } else {
                assert_closed(&mut stream);
            }
        });
        let body = format!(
            r#"
    let Mode: type = pgsql.SslMode
    let config = pgsql.PgConfig(port: {}, user: "u", database: "db", ssl: Mode.VerifyFull(ca))
    match pgsql.connect_trust(config) {{
        Ok(connection) => {{ connection.close(); panic("downgraded") }}
        Err(_) => ()
    }}
    println("no downgrade")
"#,
            peer.port
        );
        package(&format!("pgsql-tls-refusal-{}", reply.len()), &body, "").check_cached(
            "no downgrade\n",
            None,
            &[],
        );
    }
}

#[test]
fn pgsql_tls_startup_queries_and_repeated_io() {
    let (ca, config) = certificates(false, false);
    let peer = Peer::new(move |mut stream| {
        let mut request = [0; 8];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(request, [0, 0, 0, 8, 4, 210, 22, 47]);
        stream.write_all(b"S").unwrap();
        let mut secure = StreamOwned::new(ServerConnection::new(config.clone()).unwrap(), stream);
        let mut length = [0; 4];
        secure.read_exact(&mut length).unwrap();
        let mut startup = vec![0; i32::from_be_bytes(length) as usize - 4];
        secure.read_exact(&mut startup).unwrap();
        assert_eq!(&startup[..4], &196608_i32.to_be_bytes());
        secure.write_all(&handshake()).unwrap();
        secure.flush().unwrap();
        for _ in 0..40 {
            let mut header = [0; 5];
            secure.read_exact(&mut header).unwrap();
            assert_eq!(header[0], b'Q');
            let mut query =
                vec![0; i32::from_be_bytes(header[1..].try_into().unwrap()) as usize - 4];
            secure.read_exact(&mut query).unwrap();
            assert_eq!(query, b"SELECT 1\0");
            let response = [
                description(),
                data_row("1"),
                frame(b'C', b"SELECT 1\0"),
                frame(b'Z', b"I"),
            ]
            .concat();
            for chunk in response.chunks(7) {
                secure.write_all(chunk).unwrap();
                secure.flush().unwrap();
            }
        }
        assert!(matches!(secure.read(&mut [0]), Ok(0) | Err(_)));
    });
    let body = format!(
        r#"
    let Mode: type = pgsql.SslMode
    var connection = pgsql.connect_trust(pgsql.PgConfig(port: {}, user: "u", database: "db", ssl: Mode.VerifyFull(ca)))?
    var i = 0
    while i < 40 {{
        let response = connection.query("SELECT 1")?
        if response.result?.head()!.rows.head()!.text(0)?! != "1" {{ panic("TLS query") }}
        connection = response.connection
        i += 1
    }}
    connection.close()
    println("pgsql tls ok")
"#,
        peer.port
    );
    package("pgsql-tls-wire", &body, &ca).check_cached("pgsql tls ok\n", None, &[]);
}

#[test]
fn tls_cancellation_closes_handshake_read_and_write() {
    for phase in ["handshake", "read", "write"] {
        let signal = TcpListener::bind("127.0.0.1:0").unwrap();
        signal.set_nonblocking(true).unwrap();
        let signal_port = signal.local_addr().unwrap().port();
        let (ca, config) = certificates(false, false);
        let peer = Peer::new(move |mut stream| {
            let mut connection = ServerConnection::new(config.clone()).unwrap();
            if phase == "handshake" {
                let mut hello = [0; 4096];
                assert!(stream.read(&mut hello).unwrap() > 0);
            } else {
                while connection.is_handshaking() {
                    connection.complete_io(&mut stream).unwrap();
                }
                if phase == "write" {
                    let mut encrypted = [0; 4096];
                    assert!(stream.read(&mut encrypted).unwrap() > 0);
                }
            }
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
                    Err(error) => panic!("signal: {error}"),
                }
            };
            wake.set_nonblocking(false).unwrap();
            wake.write_all(b"!").unwrap();
            // Drain already queued encrypted bytes, then require prompt close.
            let mut encrypted = [0; 65536];
            loop {
                match stream.read(&mut encrypted) {
                    Ok(0) => break,
                    Ok(_) => (),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        break
                    }
                    Err(error) => panic!("cancelled TLS did not close: {error}"),
                }
            }
        });
        let (before, operation) = if phase == "handshake" {
            (
                String::new(),
                format!(
                    r#"let _ = tls.upgrade(tcp.connect("127.0.0.1", {})!, "localhost", ca)!"#,
                    peer.port
                ),
            )
        } else {
            let mut before = format!(
                r#"let stream = tls.upgrade(tcp.connect("127.0.0.1", {})?, "localhost", ca)?"#,
                peer.port
            );
            let operation = if phase == "read" {
                "let _ = stream.read(1)!; let _ = stream".into()
            } else {
                before.push_str(
                    r#"
    let storage = MutBytes()
    storage.push(65)
    while storage.length() < 16777216u64 { storage.extend(storage.to_bytes()) }
    let payload = storage.to_bytes()
"#,
                );
                "let _ = stream.write(payload)!; let _ = stream".into()
            };
            (before, operation)
        };
        let body = format!(
            r#"
    {before}
    let winner = race {{
        | {{ {operation}; "unexpected" }}
        | {{
            let signal = tcp.connect("127.0.0.1", {signal_port})!
            let _ = signal.read(1)!
            signal.close()!
            "cancelled"
        }}
    }}
    println(winner)
"#
        );
        package(&format!("tls-cancel-{phase}"), &body, &ca).check_cached("cancelled\n", None, &[]);
    }
}
