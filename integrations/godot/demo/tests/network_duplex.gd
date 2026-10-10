extends SceneTree

var language

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("NET5 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func _initialize() -> void:
    run.call_deferred()

func make(script) -> Node:
    var node = Node.new()
    node.set_script(script)
    root.add_child(node)
    return node

func accept(server: TCPServer) -> StreamPeerTCP:
    for i in range(180):
        if server.is_connection_available():
            return server.take_connection()
        await process_frame
    check(false, "duplex client connects")
    return null

func status(node: Node, getter: String, expected: int) -> void:
    for i in range(180):
        if node.call(getter) == expected:
            return
        await process_frame
    check(false, "session status: " + getter)

func idle() -> void:
    for i in range(180):
        if language.active_task_count() == 0:
            return
        await process_frame
    check(false, "all duplex tasks finish")

func closed(peer: StreamPeerTCP) -> void:
    for i in range(180):
        peer.poll()
        if peer.get_status() != StreamPeerTCP.STATUS_CONNECTED:
            return
        var available = peer.get_available_bytes()
        if available > 0:
            peer.get_data(available)
        await process_frame
    check(false, "both socket directions are released")

func push(peer: StreamPeerTCP) -> void:
    check(peer.put_data(PackedByteArray([0, 0, 0, 1, 42])) == OK, "send unsolicited push")

func command(peer: StreamPeerTCP, index: int) -> void:
    var wire := PackedByteArray()
    for i in range(180):
        peer.poll()
        var available = peer.get_available_bytes()
        if available > 0:
            var data = peer.get_data(available)
            check(data[0] == OK, "read command")
            wire.append_array(data[1])
        if wire.size() >= 5:
            break
        await process_frame
    check(wire == PackedByteArray([0, 0, 0, 1, 48 + index]), "send while waiting for push")

func finish(peer: StreamPeerTCP, node: Node, expected: int = 1) -> void:
    check(peer.put_data(PackedByteArray([0, 0, 0, 0])) == OK, "end push stream")
    await status(node, "completed", expected)
    await closed(peer)

func pressure(peer: StreamPeerTCP, node: Node) -> int:
    # Do not drain the native receive window. Require sustained sender stalling,
    # then verify receive progress while that same sender remains incomplete.
    var previous := -1
    var stable := 0
    for i in range(240):
        var sent = node.call("sent")
        if sent > 0 and sent < 512 and sent == previous:
            stable += 1
        else:
            stable = 0
        if stable >= 8:
            return sent
        previous = sent
        await process_frame
    check(false, "native receive window applies write backpressure; sent=" + str(node.call("sent")))
    return 0

func drain(peer: StreamPeerTCP, node: Node) -> void:
    var received := 0
    for frame in range(600):
        peer.poll()
        var available = peer.get_available_bytes()
        if available > 0:
            var data = peer.get_data(available)
            check(data[0] == OK, "drain backpressured bytes")
            check(data[1].count(120) == data[1].size(), "raw pressure payload contents")
            received += data[1].size()
        if node.call("sent") == 512 and received == node.call("written"):
            break
        await process_frame
    check(node.call("sent") == 512 and received == node.call("written"), "all partial writes delivered exactly once")
    check(peer.put_data(PackedByteArray([0, 0, 0, 0])) == OK, "end pressure push stream")
    await status(node, "completed", 1)
    await closed(peer)

func run() -> void:
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    check(language != null, "language registered")
    var script = load("res://network_duplex.jk")
    check(script != null and script.can_instantiate(), "duplex script compiles")
    # Build the large constant in the native harness, so instruction-budgeted
    # Joky polls measure I/O rather than millions of payload-construction steps.
    var pressure_script = ClassDB.instantiate("JokyScript")
    var pressure_source = script.source_code.replace("timed_out: Int32 }", "timed_out: Int32, written: Int32 }").replace("timed_out: 0 }", "timed_out: 0, written: 0 }")
    pressure_script.source_code = pressure_source + '''
fn written(state: &State) -> Int32 { state.written }
fn record_bytes(state: &mut State, count: Int32) { state.written = count }
fn send_pressure(writer: &mut TcpWriter) @suspends {
    let text = "PAYLOAD"
    var index: Int32 = 0
    let method = "record_send"
    var total: Int32 = 0
    let record = "record_bytes"
    while index < 512 {
        let count = match writer.write(text.clone().into_bytes()) { | Ok(count) => count | _ => { ensure(false) return } }
        ensure(count > 0)
        total = total + count.to_i32().unwrap_or(0)
        task.write_int32(&record, total)
        task.call(&method)
        index = index + 1
    }
    ensure(writer.shutdown().is_ok())
}
fn task_pressure(port: Int32) @suspends {
    let stream = match TcpStream.connect(Address.ipv4(0x7f000001, port.to_u16().unwrap_or(0))) { | Ok(stream) => stream | _ => { ensure(false) return } }
    var (reader, writer) = BufferedTcp.new(stream).split()
    parallel { | receive(&mut reader) | send_pressure(&mut writer) }
    reader.close();
    writer.close();
    let method = "finish"
    task.call(&method)
}
'''.replace('"PAYLOAD"', '"' + "x".repeat(65536) + '"')
    check(pressure_script.reload() == OK, "pressure fixture compiles")
    var server = TCPServer.new()
    check(server.listen(0, "127.0.0.1") == OK, "listen")
    var port = server.get_local_port()
    var a = make(script)
    var b = make(script)
    a.call("task_session", port, 10000)
    var peer_a = await accept(server)
    b.call("task_session", port, 10000)
    var peer_b = await accept(server)
    for i in range(3):
        await process_frame
    a.call("request", 1)
    await command(peer_a, 1)
    check(a.call("received") == 0 and b.call("sent") == 0, "read wait and instance state stay independent")
    push(peer_b)
    await status(b, "received", 1)
    check(a.call("received") == 0, "push belongs to the correct instance")
    b.call("request", 1)
    await command(peer_b, 1)
    push(peer_a)
    await status(a, "received", 1)
    a.call("request", 2)
    await command(peer_a, 2)
    push(peer_a)
    await status(a, "received", 2)
    await finish(peer_b, b)
    await finish(peer_a, a)
    a.free()
    b.free()
    await idle()

    a = make(script)
    a.call("task_session", port, 10000)
    peer_a = await accept(server)
    peer_a.disconnect_from_host()
    await status(a, "completed", 1)
    check(a.call("timed_out") == 0, "clean EOF coordinates the sender without a timeout")
    a.free()
    await idle()

    a = make(pressure_script)
    a.call("task_pressure", port)
    peer_a = await accept(server)
    var blocked = await pressure(peer_a, a)
    var pressure_snapshot = pressure_script.source_code
    pressure_script.source_code = pressure_snapshot.replace("state.completed + 1", "state.completed + 100")
    check(pressure_script.reload(true) == OK, "reload with both network directions waiting")
    for i in range(3):
        push(peer_a)
        await status(a, "received", i + 1)
    check(a.call("sent") >= blocked and a.call("sent") < 512 and a.call("completed") == 0, "receive continues while write is incomplete under backpressure")
    await pressure(peer_a, a)
    await drain(peer_a, a)
    pressure_script.source_code = pressure_snapshot
    check(pressure_script.reload(true) == OK, "restore pressure fixture")
    a.free()
    await idle()

    a = make(script)
    a.call("task_session", port, 1000)
    peer_a = await accept(server)
    a.call("request", 1)
    await command(peer_a, 1)
    check(peer_a.put_data(PackedByteArray([0, 0, 0, 1])) == OK, "partial push before deadline")
    # Idle senders use pausable next_frame waits. Complete the handshake before
    # pausing, then require the real-time deadline to cancel the partial frame.
    paused = true
    Engine.time_scale = 0.01
    await create_timer(1.2, true, false, true).timeout
    check(a.call("timed_out") == 1 and a.call("completed") == 0, "duplex deadline ignores pause and time scale")
    await closed(peer_a)
    await idle()
    a.free()
    paused = false
    Engine.time_scale = 1.0

    a = make(script)
    a.call("task_session", port, 10000)
    peer_a = await accept(server)
    var source = script.source_code
    script.source_code = source.replace("state.completed + 1", "state.completed + 100")
    check(script.reload(true) == OK, "reload live duplex session")
    b = make(script)
    b.call("task_session", port, 10000)
    peer_b = await accept(server)
    a.call("request", 1)
    b.call("request", 1)
    await command(peer_a, 1)
    await command(peer_b, 1)
    push(peer_a)
    push(peer_b)
    await status(a, "received", 1)
    await status(b, "received", 1)
    await finish(peer_a, a)
    await finish(peer_b, b, 100)
    a.free()
    b.free()
    await idle()
    script.source_code = source
    check(script.reload(true) == OK, "restore duplex source")

    for i in range(9):
        a = make(pressure_script if i >= 6 else script)
        if i >= 6:
            a.call("task_pressure", port)
        else:
            a.call("task_session", port, 10000)
        peer_a = await accept(server)
        if i >= 6:
            await pressure(peer_a, a)
        var prefix = PackedByteArray([0, 0]) if i % 2 == 0 else PackedByteArray([0, 0, 0, 1])
        check(peer_a.put_data(prefix) == OK, "partial push before owner cancellation")
        for frame in range(3):
            await process_frame
        if i % 3 == 0:
            a.set_script(null)
        elif i % 3 == 1:
            root.remove_child(a)
        a.free()
        # Create a fresh identity before delivering the old connection's event.
        b = make(script)
        b.call("task_session", port, 10000)
        peer_b = await accept(server)
        peer_a.put_partial_data(PackedByteArray([0, 1, 42, 42]))
        await closed(peer_a)
        for frame in range(3):
            await process_frame
        check(b.call("received") == 0 and b.call("completed") == 0, "late events cannot revive old task or affect new instance")
        check(language.active_task_count() == 1, "only the fresh task remains")
        push(peer_b)
        await status(b, "received", 1)
        await finish(peer_b, b)
        b.free()
        await idle()
        check(language.active_instance_count() == 0, "duplex instance roots released")
    server.stop()
    print("NET5 GODOT PASS")
    quit()
