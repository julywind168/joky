//! Static Rust/Tokio counterpart of examples/chat's measured business protocol.
use std::{
    collections::HashMap,
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch, Semaphore},
    task::JoinSet,
};

const USAGE: &str = "error use join <room>, leave, say <message>, or balance\n";
const NOT_JOINED: &str = "error join a room first\n";
const PRICE: u64 = 100;

#[derive(Clone)]
struct Config {
    workers: usize,
    chat_port: u16,
    admin_port: u16,
    balance: u64,
    max_clients: usize,
    max_rooms: usize,
    queue_messages: usize,
    queue_bytes: usize,
    max_line: usize,
    idle_ms: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            workers: 1,
            chat_port: 7777,
            admin_port: 7778,
            balance: 500,
            max_clients: 512,
            max_rooms: 4096,
            queue_messages: 64,
            queue_bytes: 65536,
            max_line: 4096,
            idle_ms: 0,
        }
    }
}
impl Config {
    fn parse() -> Result<Self, String> {
        let mut config = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            if key == "--help" {
                println!(
                    "chat-tokio [--task-workers N] [--chat-port PORT] [--admin-port PORT]\n\
                    [--balance-cents N] [--max-clients N] [--max-rooms N]\n\
                    [--queue-messages N] [--queue-bytes N] [--max-line-bytes N] [--idle-ms N]"
                );
                std::process::exit(0);
            }
            let value: u64 = args
                .next()
                .ok_or_else(|| format!("Missing value for {key}"))?
                .parse()
                .map_err(|_| format!("Invalid integer for {key}"))?;
            let size = || usize::try_from(value).map_err(|_| format!("{key} is too large"));
            match key.as_str() {
                "--task-workers" => config.workers = size()?,
                "--chat-port" => config.chat_port = value.try_into().map_err(|_| "Invalid port")?,
                "--admin-port" => {
                    config.admin_port = value.try_into().map_err(|_| "Invalid port")?
                }
                "--balance-cents" => config.balance = value,
                "--max-clients" => config.max_clients = size()?,
                "--max-rooms" => config.max_rooms = size()?,
                "--queue-messages" => config.queue_messages = size()?,
                "--queue-bytes" => config.queue_bytes = size()?,
                "--max-line-bytes" => config.max_line = size()?,
                "--idle-ms" => config.idle_ms = value,
                _ => return Err(format!("Unknown option {key}")),
            }
        }
        if config.workers == 0
            || config.max_clients == 0
            || config.max_clients > Semaphore::MAX_PERMITS
            || config.max_rooms == 0
            || config.queue_messages == 0
            || config.queue_messages > Semaphore::MAX_PERMITS
            || config.max_line == 0
        {
            return Err("Workers, limits and queue capacity must be positive and supported".into());
        }
        Ok(config)
    }
}

struct Reservation {
    bytes: usize,
    used: Arc<AtomicUsize>,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
struct Message {
    bytes: Vec<u8>,
    reservation: Reservation,
}
impl Message {
    fn receive(self) -> Vec<u8> {
        let Self { bytes, reservation } = self;
        // Both mailbox limits count queued messages, not the active socket write.
        drop(reservation);
        bytes
    }
}
#[derive(Clone)]
struct Output {
    sender: mpsc::Sender<Message>,
    used: Arc<AtomicUsize>,
    limit: usize,
    stop: watch::Sender<bool>,
}
impl Output {
    fn channel(config: &Config) -> (Self, mpsc::Receiver<Message>, watch::Receiver<bool>) {
        let (sender, receiver) = mpsc::channel(config.queue_messages);
        let (stop, stopped) = watch::channel(false);
        (
            Self {
                sender,
                used: Arc::new(AtomicUsize::new(0)),
                limit: config.queue_bytes,
                stop,
            },
            receiver,
            stopped,
        )
    }
    fn send(&self, bytes: &[u8]) {
        if *self.stop.borrow() {
            return;
        }
        if self
            .used
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes.len())
                    .filter(|&n| self.limit == 0 || n <= self.limit)
            })
            .is_err()
        {
            self.stop.send_replace(true);
            return;
        }
        let reservation = Reservation {
            bytes: bytes.len(),
            used: self.used.clone(),
        };
        // Each recipient owns a byte copy, as in the Joky outbox snapshots.
        if self
            .sender
            .try_send(Message {
                bytes: bytes.to_vec(),
                reservation,
            })
            .is_err()
        {
            self.stop.send_replace(true);
        }
    }
}
#[derive(Clone)]
struct Member {
    id: u64,
    output: Output,
}
struct Room {
    name: String,
    members: Mutex<Vec<Member>>,
}
struct Rooms {
    names: Mutex<HashMap<String, Arc<Room>>>,
    limit: usize,
}
fn broadcast(members: &[Member], message: &str) {
    for member in members {
        member.output.send(message.as_bytes());
    }
}
fn remove(members: &mut Vec<Member>, id: u64) {
    let index = members
        .iter()
        .position(|m| m.id == id)
        .expect("live room member");
    members.swap_remove(index);
}

struct Account {
    member: Member,
    room: Option<Arc<Room>>,
    rooms: Arc<Rooms>,
    balance: u64,
}
impl Account {
    fn new(id: u64, output: Output, rooms: Arc<Rooms>, balance: u64) -> Self {
        output.send(format!("welcome {id}\n").as_bytes());
        Self {
            member: Member { id, output },
            room: None,
            rooms,
            balance,
        }
    }
    fn reply(&self, message: &str) {
        self.member.output.send(message.as_bytes());
    }
    fn join(&mut self, name: &str) -> bool {
        if self.room.as_ref().is_some_and(|r| r.name == name) {
            self.reply("error already joined\n");
            return true;
        }
        // Membership changes lock directory -> rooms. Say locks just its room.
        // Holding both old and new room locks makes a move atomic to broadcasts.
        let mut names = self.rooms.names.lock().unwrap();
        if !names.contains_key(name) && names.len() == self.rooms.limit {
            return false;
        }
        let next = names
            .entry(name.to_owned())
            .or_insert_with(|| {
                Arc::new(Room {
                    name: name.to_owned(),
                    members: Mutex::new(Vec::new()),
                })
            })
            .clone();
        let old = self.room.take();
        let mut old_members = old.as_ref().map(|room| room.members.lock().unwrap());
        let mut next_members = next.members.lock().unwrap();
        if let (Some(old), Some(members)) = (&old, &mut old_members) {
            remove(members, self.member.id);
            if members.is_empty() {
                names.remove(&old.name);
            }
        }
        next_members.push(self.member.clone());
        self.room = Some(next.clone());
        if let (Some(old), Some(members)) = (&old, &old_members) {
            let left = format!("left {} {}\n", old.name, self.member.id);
            broadcast(members, &left);
            self.reply(&left);
        }
        broadcast(
            &next_members,
            &format!("joined {name} {}\n", self.member.id),
        );
        true
    }
    fn leave(&mut self, notify_self: bool) {
        let Some(room) = self.room.take() else {
            if notify_self {
                self.reply(NOT_JOINED);
            }
            return;
        };
        let mut names = self.rooms.names.lock().unwrap();
        let mut members = room.members.lock().unwrap();
        remove(&mut members, self.member.id);
        if members.is_empty() {
            names.remove(&room.name);
        }
        let left = format!("left {} {}\n", room.name, self.member.id);
        broadcast(&members, &left);
        if notify_self {
            self.reply(&left);
        }
    }
    fn say(&mut self, text: &str) {
        let Some(room) = &self.room else {
            self.reply(NOT_JOINED);
            return;
        };
        // No await or socket I/O under the lock. All recipients see the same
        // command order; queue overflow signals disconnect without rollback.
        let members = room.members.lock().unwrap();
        assert!(members.iter().any(|m| m.id == self.member.id));
        if self.balance < PRICE {
            self.reply("error insufficient balance\n");
            return;
        }
        self.balance -= PRICE;
        broadcast(
            &members,
            &format!("message {} {} {text}\n", room.name, self.member.id),
        );
    }
    fn dispatch(&mut self, line: &str) -> bool {
        let (verb, argument) = line.split_once(' ').unwrap_or((line, ""));
        match verb {
            "join" if !argument.is_empty() && !argument.contains([' ', '\t']) => {
                return self.join(argument)
            }
            "say" if !argument.is_empty() => self.say(argument),
            _ if line == "balance" => self.reply(&format!("balance {}\n", self.balance / 100)),
            _ if line == "leave" => self.leave(true),
            _ => self.reply(USAGE),
        }
        true
    }
}
impl Drop for Account {
    fn drop(&mut self) {
        self.member.output.stop.send_replace(true);
        self.leave(false);
    }
}

async fn line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    limit: usize,
) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(None);
        } // Discard a partial final line.
        let newline = available.iter().position(|&b| b == b'\n');
        let take = newline.map_or(available.len(), |n| n + 1);
        if take > limit - bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Line limit exceeded",
            ));
        }
        bytes.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
        if bytes.len() == limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Line limit exceeded",
            ));
        }
    }
}
async fn session(stream: TcpStream, id: u64, rooms: Arc<Rooms>, config: Config) {
    let (output, mut outgoing, mut stop) = Output::channel(&config);
    let mut account = Account::new(id, output, rooms, config.balance);
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::with_capacity(16384, reader);
    let receive = async {
        loop {
            if *account.member.output.stop.borrow() {
                return;
            }
            let next = if config.idle_ms == 0 {
                line(&mut reader, config.max_line).await
            } else {
                match tokio::time::timeout(
                    Duration::from_millis(config.idle_ms),
                    line(&mut reader, config.max_line),
                )
                .await
                {
                    Ok(next) => next,
                    Err(_) => return,
                }
            };
            let Ok(Some(text)) = next else {
                return;
            };
            if !account.dispatch(&text) {
                return;
            }
        }
    };
    let send = async {
        while let Some(message) = outgoing.recv().await {
            let bytes = message.receive();
            if writer.write_all(&bytes).await.is_err() {
                return;
            }
        }
    };
    tokio::select! {
        _ = receive => {},
        _ = send => {},
        _ = stop.wait_for(|stopped| *stopped) => {},
    }
    // Account's synchronous Drop removes membership on EOF, overflow, writer
    // failure, panic, and JoinSet cancellation; both socket halves then close.
}

#[cfg(test)]
mod tests;
async fn admin(listener: TcpListener) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let mut reader = BufReader::with_capacity(16384, stream);
        while let Ok(Some(command)) = line(&mut reader, 128).await {
            let shutdown = command == "shutdown";
            let reply = match command.as_str() {
                "shutdown" => "shutting down\n",
                "reload" => "error reload unsupported in static Rust server\n",
                _ => "error use reload or shutdown\n",
            };
            let result = reader.get_mut().write_all(reply.as_bytes()).await;
            if shutdown {
                return Ok(());
            }
            if result.is_err() {
                break;
            }
        }
    }
}
async fn serve(config: Config) -> io::Result<()> {
    let chat = TcpListener::bind(("127.0.0.1", config.chat_port)).await?;
    let management = TcpListener::bind(("127.0.0.1", config.admin_port)).await?;
    println!("chat {}", chat.local_addr()?.port());
    println!("admin {}", management.local_addr()?.port());
    let rooms = Arc::new(Rooms {
        names: Mutex::new(HashMap::new()),
        limit: config.max_rooms,
    });
    let permits = Arc::new(Semaphore::new(config.max_clients));
    let mut sessions = JoinSet::new();
    let mut next_id = 1u64;
    let administration = admin(management);
    tokio::pin!(administration);
    let result = loop {
        tokio::select! {
            accepted = chat.accept() => {
                let (stream, _) = match accepted { Ok(pair) => pair, Err(error) => break Err(error) };
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                // Match Joky's system-default Nagle behavior; do not silently
                // optimize only the reference server with TCP_NODELAY.
                if let Err(error) = stream.set_nodelay(false) { break Err(error); }
                let id = next_id;
                let Some(next) = next_id.checked_add(1) else { break Err(io::Error::other("Account ID exhausted")); };
                next_id = next;
                let (rooms, config) = (rooms.clone(), config.clone());
                sessions.spawn(async move { let _permit = permit; session(stream, id, rooms, config).await; });
            },
            completed = sessions.join_next(), if !sessions.is_empty() => {
                if let Some(Err(error)) = completed { eprintln!("Connection task failed: {error}"); }
            },
            result = &mut administration => break result,
        }
    };
    sessions.shutdown().await;
    result
}
fn main() {
    let result = Config::parse().and_then(|config| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(config.workers)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(serve(config))
            .map_err(|e| e.to_string())
    });
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
