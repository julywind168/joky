use std::{
    io::{BufRead, BufReader},
    net::TcpStream,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub(crate) struct Running {
    child: Child,
    lines: mpsc::Receiver<String>,
}
impl Running {
    pub(crate) fn start(mut command: Command) -> Self {
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }
    pub(crate) fn line(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(30))
            .expect("child output deadline")
    }
    pub(crate) fn signal(&self, signal: &str) {
        assert!(Command::new("kill")
            .args([signal, &self.child.id().to_string()])
            .status()
            .unwrap()
            .success());
    }
    pub(crate) fn finish(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "child exit deadline");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub(crate) fn connect(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
}

pub(crate) fn response_id(head: &str) -> &str {
    let mut ids = head
        .lines()
        .filter_map(|line| line.strip_prefix("X-Request-Id: "));
    let id = ids.next().expect("HTTP request id");
    assert_eq!(id.len(), 36);
    for (i, b) in id.bytes().enumerate() {
        if [8, 13, 18, 23].contains(&i) {
            assert_eq!(b, b'-');
        } else {
            assert!(b.is_ascii_hexdigit());
        }
    }
    assert_eq!(id.as_bytes()[14], b'4');
    assert!(ids.next().is_none());
    id
}
