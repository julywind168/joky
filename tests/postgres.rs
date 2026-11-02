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
    os::unix::fs::PermissionsExt,
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
}
impl Cluster {
    fn start(root: &Path) -> Self {
        let data = root.join("pgdata");
        checked(Command::new("initdb").arg("-D").arg(&data).args([
            "-A",
            "trust",
            "-U",
            "postgres",
            "--no-locale",
            "-E",
            "UTF8",
        ]));
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
        Self { data, port }
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
fn url(port: u16) -> String {
    format!("postgres://postgres@127.0.0.1:{port}/postgres?sslmode=disable")
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
    let direct = url(cluster.port);
    fs::write(dir.join("driver.jk"), include_str!("fixtures/postgres.jk")).unwrap();
    for args in [
        vec!["run", "driver.jk"],
        vec!["run", "--interpreter", "driver.jk"],
    ] {
        bounded(joky(dir).args(args).env("JOKY_POSTGRES_URL", &direct), dir);
    }
    checked(joky(dir).args(["build", "--task-workers", "2", "driver.jk"]));
    bounded(
        Command::new(dir.join("driver"))
            .current_dir(dir)
            .env("JOKY_POSTGRES_URL", &direct),
        dir,
    );

    fs::write(dir.join("verified.jk"),r#"import joky/postgres fn main() @suspends {
        let p=postgres.open(1)!
        let r=p.query("SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()",[])!
        assert(match r.borrow_ref(0).borrow_ref(0) {BoolValue(b)=>b,_=>false},"verified TLS connection")
    }"#).unwrap();
    bounded(
        joky(dir).args(["run", "verified.jk"]).env(
            "JOKY_POSTGRES_URL",
            format!(
                "postgres://postgres@127.0.0.1:{}/postgres?sslrootcert={}",
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
            .env("JOKY_POSTGRES_URL", url(port)),
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
            .env("JOKY_POSTGRES_URL", url(port)),
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
