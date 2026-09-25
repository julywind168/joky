use super::*;

// Independent Python hashlib.pbkdf2_hmac/hmac reference, password=pencil,
// salt=salt, iterations=4096, nonce=base64(012345678901234567890123).
const FIRST: &str = "n,,n=,r=MDEyMzQ1Njc4OTAxMjM0NTY3ODkwMTIz";
const CHALLENGE: &str = "r=MDEyMzQ1Njc4OTAxMjM0NTY3ODkwMTIzSERVER,s=c2FsdA==,i=4096";
const PROOF: &str = "c=biws,r=MDEyMzQ1Njc4OTAxMjM0NTY3ODkwMTIzSERVER,p=rX6tunUtLo1v87RBrWVSaiudr9uO5huW22FuJhN/JGQ=";
const VERIFIER: &str = "v=Dro0MZGWcVIegAsoitXj/k7l1DtkFv6/sb3VV/TRKc8=";

fn authentication(method: i32, body: &[u8]) -> Vec<u8> {
    frame(b'R', &[method.to_be_bytes().as_slice(), body].concat())
}

fn response(stream: &mut TcpStream) -> Vec<u8> {
    let mut header = [0; 5];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(header[0], b'p');
    let length = i32::from_be_bytes(header[1..].try_into().unwrap());
    assert!((4..4096).contains(&length));
    let mut data = vec![0; length as usize - 4];
    stream.read_exact(&mut data).unwrap();
    data
}

fn initial_response(stream: &mut TcpStream) {
    let data = response(stream);
    let expected = [
        b"SCRAM-SHA-256\0".as_slice(),
        (FIRST.len() as i32).to_be_bytes().as_slice(),
        FIRST.as_bytes(),
    ]
    .concat();
    assert_eq!(data, expected);
}

#[test]
fn pgsql_scram_validates_credentials_limits_and_entropy_before_network() {
    Package::new(
        "pgsql-scram-limits",
        &[("main.jk", include_str!("../fixtures/pgsql_scram_limits.jk"))],
    )
    .check_cached("pgsql SCRAM limits ok\n", None, &[]);
}

#[test]
fn pgsql_scram_cancellation_releases_each_authentication_state() {
    let signal = TcpListener::bind("127.0.0.1:0").unwrap();
    signal.set_nonblocking(true).unwrap();
    let signal_port = signal.local_addr().unwrap().port();
    let peer = Peer::new(move |mut stream| {
        let request = startup(&mut stream);
        let user = request[4..].split(|b| *b == 0).nth(1).unwrap();
        if user != b"offer" {
            stream
                .write_all(&authentication(10, b"SCRAM-SHA-256\0\0"))
                .unwrap();
            initial_response(&mut stream);
        }
        if user == b"final" || user == b"ready" {
            stream
                .write_all(&authentication(11, CHALLENGE.as_bytes()))
                .unwrap();
            assert_eq!(response(&mut stream), PROOF.as_bytes());
        }
        if user == b"ready" {
            stream
                .write_all(
                    &[
                        authentication(12, VERIFIER.as_bytes()),
                        authentication(0, b""),
                    ]
                    .concat(),
                )
                .unwrap();
        }
        // Leave the active read suspended in a partial next frame.
        stream.write_all(b"R\0\0").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut wake = loop {
            match signal.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "no authentication cancellation waiter"
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
import joky/crypto/random
import joky/socket/tls
import joky/socket/tcp
fn cancelled(stage: String) -> Result(Unit, String) effects {{ tcp, tls }} {{
    let winner = race {{
        | {{
            let result = do {{ pgsql.connect(pgsql.PgConfig(port: {}, user: stage, database: "postgres"), b"pencil") }}
                with {{ random.bytes(length) => Ok(b"012345678901234567890123") }}
            let _ = result!
            "unexpected"
        }}
        | {{
            let signal = tcp.connect("127.0.0.1", {signal_port})!
            let _ = signal.read(1)!
            signal.close()!
            "cancelled"
        }}
    }}
    if winner != "cancelled" {{ panic("authentication did not cancel") }}
    Ok(())
}}
fn main() -> Result(Unit, String) effects {{ tcp, tls }} {{
    for stage in List("offer", "challenge", "final", "ready") {{ cancelled(stage)? }}
    println("pgsql SCRAM cancellation ok")
    Ok(())
}}
"#,
        peer.port
    );
    Package::new("pgsql-scram-cancellation", &[("main.jk", &source)]).check_cached(
        "pgsql SCRAM cancellation ok\n",
        None,
        &[],
    );
}

#[test]
fn pgsql_scram_negotiation_proof_and_query() {
    let peer = Peer::new(|mut stream| {
        startup(&mut stream);
        // Ignore unknown/PLUS mechanisms and select the exact supported name.
        let offer = authentication(10, b"OTHER\0SCRAM-SHA-256\0\0");
        for chunk in offer.chunks(2) {
            stream.write_all(chunk).unwrap();
        }
        initial_response(&mut stream);
        stream
            .write_all(
                &[
                    frame(b'N', b"SNOTICE\0Mauth notice\0\0"),
                    authentication(11, CHALLENGE.as_bytes()),
                ]
                .concat(),
            )
            .unwrap();
        assert_eq!(response(&mut stream), PROOF.as_bytes());
        stream
            .write_all(&[authentication(12, VERIFIER.as_bytes()), handshake()].concat())
            .unwrap();
        assert_eq!(query(&mut stream), "SELECT 42");
        stream
            .write_all(
                &[
                    description(),
                    data_row("42"),
                    frame(b'C', b"SELECT 1\0"),
                    frame(b'Z', b"I"),
                ]
                .concat(),
            )
            .unwrap();
        assert_closed(&mut stream);
    });
    let source = format!(
        r#"
import joky/pgsql
import joky/crypto/random
import joky/socket/tls
import joky/socket/tcp
fn main() -> Result(Unit, String) effects {{ tcp, tls }} {{
    let connection = do {{
        pgsql.connect(pgsql.PgConfig(port: {}, user: "user,=", database: "postgres"), b"pencil")
    }} with {{ random.bytes(length) => Ok(b"012345678901234567890123") }}
    let response = connection?.query("SELECT 42")?
    if response.result?.head()!.rows.head()!.text(0)?! != "42" {{ panic("query") }}
    response.connection.close()
    println("pgsql SCRAM ok")
    Ok(())
}}
"#,
        peer.port
    );
    Package::new("pgsql-scram-success", &[("main.jk", &source)]).check_cached(
        "pgsql SCRAM ok\n",
        None,
        &[],
    );
}

#[test]
fn pgsql_scram_rejects_invalid_authentication_and_closes_socket() {
    let cases = [
        ("trust", "pgsql: expected SCRAM-SHA-256 authentication"),
        ("cleartext", "pgsql: expected SCRAM-SHA-256 authentication"),
        ("md5", "pgsql: expected SCRAM-SHA-256 authentication"),
        (
            "early_continue",
            "pgsql: expected SCRAM-SHA-256 authentication",
        ),
        (
            "early_final",
            "pgsql: expected SCRAM-SHA-256 authentication",
        ),
        (
            "plus_only",
            "pgsql: server offered SCRAM-SHA-256-PLUS without TLS",
        ),
        ("empty_offer", "pgsql: server does not offer SCRAM-SHA-256"),
        ("unterminated_offer", "binary: missing string terminator"),
        ("trailing_offer", "binary: trailing input"),
        ("utf8_offer", "binary: invalid UTF-8"),
        ("early_parameter", "pgsql: parameter before authentication"),
        ("early_key", "pgsql: backend key before authentication"),
        (
            "oversized_auth",
            "pgsql: authentication message limit exceeded",
        ),
        ("error_initial", "pgsql [28000]: rejected"),
        ("duplicate_offer", "pgsql: expected SASL continue"),
        ("ok_before_challenge", "pgsql: expected SASL continue"),
        ("final_before_challenge", "pgsql: expected SASL continue"),
        ("bad_nonce", "scram: server nonce must extend client nonce"),
        ("high_iterations", "scram: iteration limit exceeded"),
        ("low_iterations", "scram: iteration count below minimum"),
        (
            "utf8_challenge",
            "pgsql: invalid UTF-8 authentication message",
        ),
        ("early_ready", "pgsql: ready before authentication"),
        ("wrong_signature", "scram: server signature mismatch"),
        ("missing_signature", "pgsql: expected SASL final"),
        ("short_signature", "scram: invalid server signature length"),
        ("final_error", "scram: server rejected authentication"),
        (
            "server_error",
            "pgsql [28P01]: password authentication failed",
        ),
        ("duplicate_challenge", "pgsql: expected SASL final"),
        ("missing_ok", "pgsql: ready before authentication"),
        (
            "duplicate_final",
            "pgsql: expected authentication completion",
        ),
        ("ok_trailing", "binary: trailing input"),
        ("duplicate_ok", "pgsql: authentication already completed"),
    ];
    let peer = Peer::new(|mut stream| {
        let request = startup(&mut stream);
        let user = std::str::from_utf8(request[4..].split(|b| *b == 0).nth(1).unwrap()).unwrap();
        let early = match user {
            "trust" => Some(authentication(0, b"")),
            "cleartext" => Some(authentication(3, b"")),
            "md5" => Some(authentication(5, b"salt")),
            "early_continue" => Some(authentication(11, CHALLENGE.as_bytes())),
            "early_final" => Some(authentication(12, VERIFIER.as_bytes())),
            "plus_only" => Some(authentication(10, b"SCRAM-SHA-256-PLUS\0\0")),
            "empty_offer" => Some(authentication(10, b"\0")),
            "unterminated_offer" => Some(authentication(10, b"SCRAM-SHA-256\0")),
            "trailing_offer" => Some(authentication(10, b"SCRAM-SHA-256\0\0x")),
            "utf8_offer" => Some(authentication(10, b"\xff\0\0")),
            "early_parameter" => Some(frame(b'S', b"client_encoding\0UTF8\0")),
            "early_key" => Some(frame(b'K', &[0; 8])),
            // Send only the header: rejection must precede allocation/body reads.
            "oversized_auth" => {
                Some([b"R".as_slice(), 16393_i32.to_be_bytes().as_slice()].concat())
            }
            "error_initial" => Some(frame(b'E', b"SERROR\0C28000\0Mrejected\0\0")),
            _ => None,
        };
        if let Some(data) = early {
            stream.write_all(&data).unwrap();
            assert_closed(&mut stream);
            return;
        }
        stream
            .write_all(&authentication(10, b"SCRAM-SHA-256\0\0"))
            .unwrap();
        initial_response(&mut stream);
        let invalid = match user {
            "duplicate_offer" => Some(authentication(10, b"SCRAM-SHA-256\0\0")),
            "ok_before_challenge" => Some(authentication(0, b"")),
            "final_before_challenge" => Some(authentication(12, VERIFIER.as_bytes())),
            "bad_nonce" => Some(authentication(11, b"r=wrong,s=c2FsdA==,i=4096")),
            "high_iterations" => Some(authentication(
                11,
                CHALLENGE.replace("4096", "100001").as_bytes(),
            )),
            "low_iterations" => Some(authentication(
                11,
                CHALLENGE.replace("4096", "4095").as_bytes(),
            )),
            "utf8_challenge" => Some(authentication(11, &[255])),
            "early_ready" => Some(frame(b'Z', b"I")),
            _ => None,
        };
        if let Some(data) = invalid {
            stream.write_all(&data).unwrap();
            assert_closed(&mut stream);
            return;
        }
        stream
            .write_all(&authentication(11, CHALLENGE.as_bytes()))
            .unwrap();
        assert_eq!(response(&mut stream), PROOF.as_bytes());
        let invalid = match user {
            "wrong_signature" => Some(authentication(
                12,
                b"v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            )),
            "missing_signature" => Some(authentication(0, b"")),
            "short_signature" => Some(authentication(12, b"v=c2FsdA==")),
            "final_error" => Some(authentication(12, b"e=invalid-proof")),
            "server_error" => Some(frame(
                b'E',
                b"SERROR\0C28P01\0Mpassword authentication failed\0\0",
            )),
            "duplicate_challenge" => Some(authentication(11, CHALLENGE.as_bytes())),
            _ => None,
        };
        if let Some(data) = invalid {
            stream.write_all(&data).unwrap();
            assert_closed(&mut stream);
            return;
        }
        let tail = match user {
            "missing_ok" => frame(b'Z', b"I"),
            "duplicate_final" => authentication(12, VERIFIER.as_bytes()),
            "ok_trailing" => authentication(0, b"x"),
            "duplicate_ok" => [authentication(0, b""), authentication(0, b"")].concat(),
            _ => panic!("unknown case {user}"),
        };
        stream
            .write_all(&[authentication(12, VERIFIER.as_bytes()), tail].concat())
            .unwrap();
        assert_closed(&mut stream);
    });
    for (batch, cases) in cases.chunks(4).enumerate() {
        let mut calls = String::new();
        for (name, expected) in cases {
            calls.push_str(&format!("    fails({name:?}, {expected:?})?\n"));
        }
        let source = format!(
            r#"
import joky/pgsql
import joky/crypto/random
import joky/socket/tls
import joky/socket/tcp
fn fails(name: String, expected: String) -> Result(Unit, String) effects {{ tcp, tls }} {{
    let result = do {{
        pgsql.connect(pgsql.PgConfig(port: {}, user: name, database: "postgres"), b"pencil")
    }} with {{ random.bytes(length) => Ok(b"012345678901234567890123") }}
    match result {{
        Ok(connection) => {{ connection.close(); panic(name + " accepted") }}
        Err(message) => if message != expected {{ panic(name + ": " + message) }}
    }}
    Ok(())
}}
fn main() -> Result(Unit, String) effects {{ tcp, tls }} {{
{calls}
    println("pgsql SCRAM failures ok")
    Ok(())
}}
"#,
            peer.port
        );
        Package::new(
            &format!("pgsql-scram-failures-{batch}"),
            &[("main.jk", &source)],
        )
        .check_cached("pgsql SCRAM failures ok\n", None, &[]);
    }
}
