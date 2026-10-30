use super::*;

fn rooms(limit: usize) -> Arc<Rooms> {
    Arc::new(Rooms {
        names: Mutex::new(HashMap::new()),
        limit,
    })
}
fn account(id: u64, rooms: Arc<Rooms>) -> (Account, mpsc::Receiver<Message>) {
    let (out, mut rx, _) = Output::channel(&Config::default());
    let account = Account::new(id, out, rooms, 500);
    assert_eq!(
        rx.try_recv().unwrap().receive(),
        format!("welcome {id}\n").as_bytes()
    );
    (account, rx)
}
fn expect(rx: &mut mpsc::Receiver<Message>, message: &str) {
    assert_eq!(rx.try_recv().unwrap().receive(), message.as_bytes());
}

#[test]
fn commands_charge_once_move_atomically_and_reclaim_rooms() {
    let rooms = rooms(2);
    let (mut a, mut ar) = account(1, rooms.clone());
    let (mut b, mut br) = account(2, rooms.clone());
    a.dispatch("say before joining");
    expect(&mut ar, NOT_JOINED);
    a.dispatch("join room");
    expect(&mut ar, "joined room 1\n");
    b.dispatch("join room");
    expect(&mut ar, "joined room 2\n");
    expect(&mut br, "joined room 2\n");
    a.dispatch("join room");
    expect(&mut ar, "error already joined\n");
    for _ in 0..5 {
        a.dispatch("say  Unicode 中文");
        expect(&mut ar, "message room 1  Unicode 中文\n");
        expect(&mut br, "message room 1  Unicode 中文\n");
    }
    a.dispatch("say unavailable");
    expect(&mut ar, "error insufficient balance\n");
    assert!(br.try_recv().is_err());
    a.dispatch("balance");
    expect(&mut ar, "balance 0\n");
    b.dispatch("balance");
    expect(&mut br, "balance 5\n");
    b.dispatch("join 房间");
    expect(&mut ar, "left room 2\n");
    expect(&mut br, "left room 2\n");
    expect(&mut br, "joined 房间 2\n");
    assert_eq!(rooms.names.lock().unwrap().len(), 2);
    a.dispatch("leave");
    expect(&mut ar, "left room 1\n");
    assert!(!rooms.names.lock().unwrap().contains_key("room"));
    for invalid in [
        "join ",
        "join two rooms",
        "join a\tb",
        "say",
        "balance ",
        "reload",
        "shutdown",
    ] {
        a.dispatch(invalid);
        expect(&mut ar, USAGE);
    }
    a.dispatch("join 房间");
    expect(&mut ar, "joined 房间 1\n");
    expect(&mut br, "joined 房间 1\n");
    drop(a);
    expect(&mut br, "left 房间 1\n");
    drop(b);
    assert!(rooms.names.lock().unwrap().is_empty());
}

#[test]
fn full_directory_does_not_partially_leave_the_old_room() {
    let rooms = rooms(1);
    let (mut a, mut rx) = account(1, rooms.clone());
    assert!(a.dispatch("join old"));
    expect(&mut rx, "joined old 1\n");
    assert!(!a.dispatch("join new"));
    assert_eq!(a.room.as_ref().unwrap().name, "old");
    assert!(rx.try_recv().is_err());
    drop(a);
    assert!(rooms.names.lock().unwrap().is_empty());
}

#[test]
fn queue_limits_disconnect_and_refund_without_harming_healthy_recipients() {
    let config = Config {
        queue_messages: 1,
        queue_bytes: 4,
        ..Config::default()
    };
    let (slow, mut rx, stop) = Output::channel(&config);
    slow.send(b"1234");
    assert_eq!(slow.used.load(Ordering::Relaxed), 4);
    let bytes = rx.try_recv().unwrap().receive();
    assert_eq!(bytes, b"1234");
    assert_eq!(
        slow.used.load(Ordering::Relaxed),
        0,
        "receiving refunds before write completion"
    );
    slow.send(b"12");
    let (healthy, mut ready, healthy_stop) = Output::channel(&Config::default());
    broadcast(
        &[
            Member {
                id: 1,
                output: slow.clone(),
            },
            Member {
                id: 2,
                output: healthy,
            },
        ],
        "ok",
    );
    assert!(*stop.borrow(), "message count overflow disconnects");
    assert!(!*healthy_stop.borrow());
    expect(&mut ready, "ok");
    assert_eq!(
        slow.used.load(Ordering::Relaxed),
        2,
        "failed send refunds its reservation"
    );
    drop(rx);
    assert_eq!(slow.used.load(Ordering::Relaxed), 0);
    let (out, rx, stop) = Output::channel(&config);
    out.send(b"12345");
    assert!(*stop.borrow(), "byte overflow disconnects");
    assert_eq!(out.used.load(Ordering::Relaxed), 0);
    drop(rx);
    let (out, rx, stop) = Output::channel(&config);
    drop(rx);
    out.send(b"x");
    assert!(*stop.borrow(), "closed receiver disconnects");
    assert_eq!(out.used.load(Ordering::Relaxed), 0);
}

#[test]
fn concurrent_senders_publish_the_same_room_order_to_every_recipient() {
    let rooms = rooms(1);
    let (mut a, mut ar) = account(1, rooms.clone());
    let (mut b, mut br) = account(2, rooms.clone());
    a.join("room");
    expect(&mut ar, "joined room 1\n");
    b.join("room");
    expect(&mut ar, "joined room 2\n");
    expect(&mut br, "joined room 2\n");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let gate = barrier.clone();
    let first = std::thread::spawn(move || {
        gate.wait();
        a.say("first");
        a
    });
    let second = std::thread::spawn(move || {
        barrier.wait();
        b.say("second");
        b
    });
    let (a, b) = (first.join().unwrap(), second.join().unwrap());
    let left = [
        ar.try_recv().unwrap().receive(),
        ar.try_recv().unwrap().receive(),
    ];
    let right = [
        br.try_recv().unwrap().receive(),
        br.try_recv().unwrap().receive(),
    ];
    assert_eq!(left, right);
    assert_ne!(left[0], left[1]);
    assert_eq!((a.balance, b.balance), (400, 400));
    drop((a, b));
    assert!(rooms.names.lock().unwrap().is_empty());
}

#[tokio::test]
async fn framing_is_bounded_validates_utf8_and_discards_partial_eof() {
    let mut reader = BufReader::with_capacity(2, &b"a\r\nb\nunfinished"[..]);
    assert_eq!(line(&mut reader, 3).await.unwrap().as_deref(), Some("a"));
    assert_eq!(line(&mut reader, 3).await.unwrap().as_deref(), Some("b"));
    assert!(line(&mut reader, 64).await.unwrap().is_none());
    for input in [&b"abcd\n"[..], &b"abcd"[..], &b"\xff\n"[..]] {
        let mut reader = BufReader::with_capacity(2, input);
        assert_eq!(
            line(&mut reader, 4).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
