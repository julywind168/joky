//! Real PostgreSQL tests use a disposable cluster, never the caller's database.
#![cfg(unix)]
#[path = "support/runtime.rs"]
mod runtime;
#[path = "support/scratch.rs"]
mod scratch;
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn checked(cmd: &mut Command) {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
struct Cluster {
    data: PathBuf,
    port: u16,
    password: String,
}
impl Cluster {
    fn start(root: &Path) -> Self {
        let data = root.join("pgdata");
        let mut secret = [0u8; 24];
        getrandom::fill(&mut secret).unwrap();
        let password: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        let pwfile = root.join("initdb-password");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pwfile)
            .unwrap();
        writeln!(file, "{password}").unwrap();
        drop(file);
        checked(
            Command::new("initdb")
                .arg("-D")
                .arg(&data)
                .args([
                    "-A",
                    "scram-sha-256",
                    "-U",
                    "postgres",
                    "--no-locale",
                    "-E",
                    "UTF8",
                    "--pwfile",
                ])
                .arg(&pwfile),
        );
        fs::remove_file(pwfile).unwrap();
        let port = TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        fs::write(
            data.join("server.crt"),
            include_bytes!("fixtures/tls/server.pem"),
        )
        .unwrap();
        fs::write(
            data.join("server.key"),
            include_bytes!("fixtures/tls/test-only-key.pem"),
        )
        .unwrap();
        fs::set_permissions(data.join("server.key"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(root.join("ca.pem"), include_bytes!("fixtures/tls/ca.pem")).unwrap();
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(data.join("postgresql.conf"))
                .unwrap(),
            "\nlisten_addresses='127.0.0.1'\nport={port}\nunix_socket_directories=''\nssl=on"
        )
        .unwrap();
        checked(
            Command::new("pg_ctl")
                .arg("-D")
                .arg(&data)
                .arg("-l")
                .arg(root.join("server.log"))
                .args(["-w", "start"]),
        );
        Self {
            data,
            port,
            password,
        }
    }
    fn url(&self, port: u16) -> String {
        format!(
            "postgres://postgres:{}@127.0.0.1:{port}/postgres?sslmode=disable",
            self.password
        )
    }
}
impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = Command::new("pg_ctl")
            .arg("-D")
            .arg(&self.data)
            .args(["-m", "immediate", "-w", "stop"])
            .output();
    }
}
fn joky(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_joky"));
    c.current_dir(root)
        .env("JOKY_RUNTIME", runtime::runtime_archive())
        .env_remove("JOKY_POSTGRES_URL");
    c
}
fn bounded(cmd: &mut Command, root: &Path) {
    let log = root.join("output.log");
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{}", fs::read_to_string(log).unwrap());
            return;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Postgres test process timed out")
        }
        thread::sleep(Duration::from_millis(10));
    }
}
// Keep the complete HTTP + database import graph within the compiler's type
// budgets even on machines that cannot start the optional PostgreSQL cluster.
#[test]
fn postgres_http_order_example_type_checks() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    checked(joky(&examples).args(["check", "postgres_orders_http.jk"]));
}

#[test]
fn postgres_requires_host_configuration() {
    let scratch = scratch::ScratchDir::new("pg-config");
    let dir = scratch.path();
    fs::write(dir.join("app.jk"),r#"import joky/postgres fn main() @suspends {
        assert(match postgres.open(0) { Err(Invalid) => true, _ => false }, "invalid capacity")
        assert(match postgres.open(1) { Err(Unavailable) => true, _ => false }, "missing capability")
    }"#).unwrap();
    bounded(joky(dir).args(["run", "app.jk"]), dir);
}

// Suppress the PostgreSQL CommandComplete message *after* PostgreSQL has
// committed. Dropping the client before sending COMMIT would test only rollback.
fn lose_ack(server: u16, command: &'static [u8]) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    (
        port,
        thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut db = TcpStream::connect(("127.0.0.1", server)).unwrap();
            db.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            let mut input = client.try_clone().unwrap();
            let mut upload = db.try_clone().unwrap();
            let writer = thread::spawn(move || {
                let _ = std::io::copy(&mut input, &mut upload);
                let _ = upload.shutdown(Shutdown::Write);
            });
            loop {
                let mut head = [0; 5];
                db.read_exact(&mut head).unwrap();
                let n = u32::from_be_bytes(head[1..].try_into().unwrap()) as usize;
                assert!((4..=2_000_000).contains(&n));
                let mut payload = vec![0; n - 4];
                db.read_exact(&mut payload).unwrap();
                if head[0] == b'C' && payload == command {
                    break;
                }
                client.write_all(&head).unwrap();
                client.write_all(&payload).unwrap();
            }
            client.shutdown(Shutdown::Both).unwrap();
            let _ = db.shutdown(Shutdown::Both);
            writer.join().unwrap();
        }),
    )
}
#[test]
#[ignore = "requires local initdb and pg_ctl; starts a disposable PostgreSQL cluster"]
fn postgres_driver_transactions_cancellation_and_unknown_outcomes() {
    let work = scratch::ScratchDir::new("pg-driver");
    let dir = work.path();
    let cluster = Cluster::start(dir);
    let direct = cluster.url(cluster.port);
    fs::write(dir.join("denied.jk"),r#"import joky/postgres fn main() @suspends {
        assert(match postgres.open(1) { Err(Connect(_)) => true, _ => false }, "database must reject the credentials")
    }"#).unwrap();
    for userinfo in ["postgres:wrong", "postgres"] {
        let bad = format!(
            "postgres://{userinfo}@127.0.0.1:{}/postgres?sslmode=disable",
            cluster.port
        );
        bounded(
            joky(dir)
                .args(["run", "denied.jk"])
                .env("JOKY_POSTGRES_URL", bad)
                .env_remove("PGPASSWORD")
                .env("PGPASSFILE", dir.join("no-password-file")),
            dir,
        );
    }
    for (name, source) in [
        ("driver", include_str!("fixtures/postgres.jk")),
        ("types", include_str!("fixtures/postgres_types.jk")),
    ] {
        let file = format!("{name}.jk");
        fs::write(dir.join(&file), source).unwrap();
        for args in [vec!["run", &file], vec!["run", "--interpreter", &file]] {
            bounded(joky(dir).args(args).env("JOKY_POSTGRES_URL", &direct), dir);
        }
        checked(joky(dir).args(["build", "--task-workers", "2", &file]));
        bounded(
            Command::new(dir.join(name))
                .current_dir(dir)
                .env("JOKY_POSTGRES_URL", &direct),
            dir,
        );
    }

    fs::write(dir.join("verified.jk"),r#"import joky/postgres fn main() @suspends {
        let p=postgres.open(1)!
        let r=p.query("SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()",[])!
        assert(match r.borrow_ref(0).borrow_ref(0) {BoolValue(b)=>b,_=>false},"verified TLS connection")
    }"#).unwrap();
    bounded(
        joky(dir).args(["run", "verified.jk"]).env(
            "JOKY_POSTGRES_URL",
            format!(
                "postgres://postgres:{}@127.0.0.1:{}/postgres?sslrootcert={}",
                cluster.password,
                cluster.port,
                dir.join("ca.pem").display()
            ),
        ),
        dir,
    );
    fs::write(
        dir.join("orders.jk"),
        include_str!("../examples/postgres_orders.jk"),
    )
    .unwrap();
    fs::write(dir.join("setup.jk"),"import orders import joky/postgres fn main() @suspends { let p=postgres.open(1)!; orders.setup(&p) }").unwrap();
    bounded(
        joky(dir)
            .args(["run", "setup.jk"])
            .env("JOKY_POSTGRES_URL", &direct),
        dir,
    );
    fs::write(dir.join("lost.jk"),r#"import orders import joky/postgres fn main() @suspends {
        let p=postgres.open(1)!
        assert(match orders.place(&p,1,"lost",10) { Err(OutcomeUnknown)=>true, _=>false }, "lost commit ack")
    }"#).unwrap();
    let (port, proxy) = lose_ack(cluster.port, b"COMMIT\0");
    bounded(
        joky(dir)
            .args(["run", "lost.jk"])
            .env("JOKY_POSTGRES_URL", cluster.url(port)),
        dir,
    );
    proxy.join().unwrap();

    fs::write(dir.join("retry.jk"),r#"import orders import joky/postgres fn main() @suspends {
        let p=postgres.open(1)!
        assert(match orders.place(&p,1,"lost",10)! { Confirmed(id)=>id==1, _=>false }, "original order")
        assert(match orders.place(&p,1,"lost",11)! { Conflict=>true, _=>false }, "changed quantity")
        let r=p.query("SELECT count(*), (SELECT available FROM joky_demo_stock WHERE sku=1) FROM joky_demo_orders",[])!
        let row=r.borrow_ref(0)
        assert(match row.borrow_ref(0) { IntValue(n)=>n==1,_=>false },"one order")
        assert(match row.borrow_ref(1) { IntValue(n)=>n==90,_=>false },"one deduction")
    }"#).unwrap();
    bounded(
        joky(dir)
            .args(["run", "retry.jk"])
            .env("JOKY_POSTGRES_URL", &direct),
        dir,
    );

    fs::write(dir.join("auto.jk"),r#"import joky/postgres fn main() @suspends {
        let p=postgres.open(1)!
        assert(match p.execute("INSERT INTO joky_demo_stock VALUES (2, 10)",[]) { Err(OutcomeUnknown)=>true, _=>false },"lost autocommit ack")
    }"#).unwrap();
    let (port, proxy) = lose_ack(cluster.port, b"INSERT 0 1\0");
    bounded(
        joky(dir)
            .args(["run", "auto.jk"])
            .env("JOKY_POSTGRES_URL", cluster.url(port)),
        dir,
    );
    proxy.join().unwrap();
    fs::write(dir.join("verify_auto.jk"), r#"import joky/postgres fn main() @suspends {
        let p=postgres.open(1)!
        let r=p.query("SELECT available FROM joky_demo_stock WHERE sku=2",[])!
        assert(match r.borrow_ref(0).borrow_ref(0) { IntValue(n)=>n==10,_=>false },"autocommit persisted")
    }"#).unwrap();
    bounded(
        joky(dir)
            .args(["run", "verify_auto.jk"])
            .env("JOKY_POSTGRES_URL", &direct),
        dir,
    );
}

#[path = "support/tcp_server.rs"]
mod tcp_server;

fn orders_port(server: &tcp_server::Running) -> u16 {
    server
        .line()
        .strip_prefix("listening ")
        .unwrap()
        .parse()
        .unwrap()
}
fn order_request(port: u16, headers: &str, body: &str) -> TcpStream {
    let mut stream = tcp_server::connect(port);
    write!(
        stream,
        "POST /orders HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream
}
fn post_order(port: u16, headers: &str, body: &str, expected: u16) -> String {
    post_with_id(port, headers, body, expected).0
}
fn post_with_id(port: u16, headers: &str, body: &str, expected: u16) -> (String, String) {
    let mut reply = String::new();
    order_request(port, headers, body)
        .read_to_string(&mut reply)
        .unwrap();
    let (head, payload) = reply.split_once("\r\n\r\n").unwrap();
    assert!(
        head.starts_with(&format!("HTTP/1.1 {expected} ")),
        "{reply}"
    );
    (payload.to_owned(), tcp_server::response_id(head).to_owned())
}

#[test]
#[ignore = "requires local initdb and pg_ctl; starts a disposable PostgreSQL cluster"]
fn http_orders_replay_after_lost_response_and_concurrent_requests() {
    use sqlx::{Connection, PgConnection};
    let work = scratch::ScratchDir::new("http-orders");
    let dir = work.path();
    let cluster = Cluster::start(dir);
    let direct = cluster.url(cluster.port);
    for (name, src) in [
        (
            "postgres_orders.jk",
            include_str!("../examples/postgres_orders.jk"),
        ),
        (
            "postgres_orders_http.jk",
            include_str!("../examples/postgres_orders_http.jk"),
        ),
        (
            "pause.jk",
            include_str!("fixtures/postgres_orders_pause.jk"),
        ),
    ] {
        fs::write(dir.join(name), src).unwrap();
    }
    fs::write(
        dir.join("pool_lifetime.jk"),
        include_str!("fixtures/postgres_orders_pool_lifetime.jk"),
    )
    .unwrap();
    bounded(
        joky(dir)
            .args(["run", "pool_lifetime.jk"])
            .env("JOKY_POSTGRES_URL", &direct),
        dir,
    );
    checked(joky(dir).args(["build", "--task-workers", "2", "postgres_orders_http.jk"]));
    let mut cmd = joky(dir);
    cmd.args(["run", "pause.jk"])
        .env("JOKY_POSTGRES_URL", &direct);
    let mut paused = tcp_server::Running::start(cmd);
    const KEY: &str = "Idempotency-Key: lost-response\r\n";
    let mut client = order_request(orders_port(&paused), KEY, r#"{"qty":10}"#);
    let original = paused.line();
    assert!(original.starts_with("{\"order_id\":"), "{original}");
    paused.signal("-KILL");
    assert!(!paused.finish().success());
    assert_eq!(
        client.read(&mut [0]).unwrap(),
        0,
        "response was never written"
    );

    let mut cmd = Command::new(dir.join("postgres_orders_http"));
    cmd.current_dir(dir)
        .env("JOKY_POSTGRES_URL", &direct)
        .stderr(fs::File::create(dir.join("orders.jsonl")).unwrap());
    let mut server = tcp_server::Running::start(cmd);
    let port = orders_port(&server);
    assert_eq!(post_order(port, KEY, r#"{"qty":10}"#, 200), original);
    post_order(port, KEY, r#"{"qty":11}"#, 409);
    for (headers, body) in [
        ("", r#"{"qty":2}"#),
        (
            "Idempotency-Key: a\r\nIdempotency-Key: b\r\n",
            r#"{"qty":2}"#,
        ),
        (KEY, r#"{"qty":0}"#),
        (KEY, r#"{"qty":"2"}"#),
        (KEY, "{"),
    ] {
        post_order(port, headers, body, 400);
    }
    post_order(port, "Idempotency-Key: too-many\r\n", r#"{"qty":999}"#, 409);

    // Hold the stock row so the winner cannot commit before the other
    // request reaches PostgreSQL and waits on the order's unique key.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut db = rt.block_on(PgConnection::connect(&direct)).unwrap();
    rt.block_on(sqlx::query("BEGIN").execute(&mut db)).unwrap();
    rt.block_on(
        sqlx::query("SELECT sku FROM joky_demo_stock WHERE sku=1 FOR UPDATE").execute(&mut db),
    )
    .unwrap();
    let ids = thread::scope(|scope| {
        let submit = || {
            post_with_id(
                port,
                "Idempotency-Key: simultaneous\r\n",
                r#"{"qty":7}"#,
                200,
            )
        };
        let first = scope.spawn(submit);
        let second = scope.spawn(submit);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let waiting:i64=rt.block_on(sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock' AND (query LIKE 'UPDATE joky_demo_stock SET available=%' OR query LIKE 'INSERT INTO joky_demo_orders %')").fetch_one(&mut db)).unwrap();
            if waiting == 2 {
                break;
            }
            // pg_stat_activity snapshots can be cached within this observer transaction.
            rt.block_on(sqlx::query("SELECT pg_stat_clear_snapshot()").execute(&mut db))
                .unwrap();
            assert!(
                Instant::now() < deadline,
                "both orders must reach the database concurrently"
            );
            thread::sleep(Duration::from_millis(10));
        }
        rt.block_on(sqlx::query("COMMIT").execute(&mut db)).unwrap();
        let (a, b) = (first.join().unwrap(), second.join().unwrap());
        assert_eq!(a.0, b.0);
        assert_ne!(a.1, b.1);
        [a.1, b.1]
    });
    let state:(i64,i64)=rt.block_on(sqlx::query_as("SELECT count(*), (SELECT available FROM joky_demo_stock WHERE sku=1) FROM joky_demo_orders").fetch_one(&mut db)).unwrap();
    assert_eq!(state, (2, 83));
    // Bypass startup DDL on this single-connection fault proxy.
    fs::write(
        dir.join("lost_ack_http.jk"),
        r#"import postgres_orders_http as api
import joky/http import joky/postgres import joky/shutdown import joky/tcp
fn main() @suspends {
    let pool=postgres.open(1)!
    let stop=shutdown.create()
    let signals=stop.subscribe_unix()!
    var listener=tcp.listen("127.0.0.1",0)!
    println("listening {listener.local_addr()!.port()}")
    http.serve_with(listener,&stop,http.Options.default(),api.routes(pool))!
}"#,
    )
    .unwrap();
    let (proxy_port, proxy) = lose_ack(cluster.port, b"COMMIT\0");
    let mut cmd = joky(dir);
    cmd.args(["run", "--interpreter", "lost_ack_http.jk"])
        .env("JOKY_POSTGRES_URL", cluster.url(proxy_port));
    cmd.stderr(fs::File::create(dir.join("fault.jsonl")).unwrap());
    let mut faulty = tcp_server::Running::start(cmd);
    let (unknown, unknown_id) = post_with_id(
        orders_port(&faulty),
        "Idempotency-Key: lost-ack\r\nAuthorization: Bearer test-secret\r\nX-Request-Id: caller-supplied-id\r\n",
        r#"{"qty":1}"#,
        503,
    );
    assert!(
        unknown.contains("retry with the same Idempotency-Key"),
        "{unknown}"
    );
    proxy.join().unwrap();
    faulty.signal("-TERM");
    assert!(faulty.finish().success());
    let replay = post_order(port, "Idempotency-Key: lost-ack\r\n", r#"{"qty":1}"#, 200);
    let id: i64 = rt
        .block_on(
            sqlx::query_scalar(
                "SELECT id FROM joky_demo_orders WHERE customer=1 AND request_id='lost-ack'",
            )
            .fetch_one(&mut db),
        )
        .unwrap();
    assert_eq!(replay, format!("{{\"order_id\":{id}}}"));
    let state:(i64,i64)=rt.block_on(sqlx::query_as("SELECT count(*), (SELECT available FROM joky_demo_stock WHERE sku=1) FROM joky_demo_orders").fetch_one(&mut db)).unwrap();
    assert_eq!(state, (3, 82));
    server.signal("-TERM");
    assert!(server.finish().success());
    let events = |file| {
        let text = fs::read_to_string(dir.join(file)).unwrap();
        for secret in [
            "simultaneous",
            "lost-ack",
            "test-secret",
            "caller-supplied-id",
            "postgres://",
            "\"qty\"",
        ] {
            assert!(!text.contains(secret));
        }
        text.lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    };
    let normal = events("orders.jsonl");
    let assert_finished = |events: &[serde_json::Value], id: &str, status: u16| {
        let matching: Vec<_> = events
            .iter()
            .filter(|e| e["request_id"] == id && e["event"] == "http.request_finished")
            .collect();
        assert_eq!(matching.len(), 1);
        let e = matching[0];
        assert_eq!(e["method"], "POST");
        assert_eq!(e["path"], "/orders");
        assert!(e.get("route").is_none());
        assert_eq!(e["status"], status);
        assert_eq!(e["write_outcome"], "complete");
        assert!(e["duration_ms"].is_u64() && e["timestamp"].is_string());
    };
    for id in ids {
        assert_finished(&normal, &id, 200);
    }
    let fault = events("fault.jsonl");
    assert_finished(&fault, &unknown_id, 503);
    assert_eq!(
        fault
            .iter()
            .filter(|e| e["event"] == "order.commit_unknown"
                && e["level"] == "warn"
                && e["request_id"] == unknown_id)
            .count(),
        1
    );
}
