extends SceneTree

var frames := 0
var language

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("NET3 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func _initialize() -> void:
    process_frame.connect(func(): frames += 1)
    run.call_deferred()

func make(script) -> Node:
    var node = Node.new()
    node.set_script(script)
    root.add_child(node)
    return node

func accept(server: TCPServer) -> StreamPeerTCP:
    for i in range(90):
        if server.is_connection_available():
            return server.take_connection()
        await process_frame
    check(false, "connection makes progress across frames")
    return null

func echo(peer: StreamPeerTCP, node: Node, expected: int = 1) -> void:
    # Split the input across frames to force a pending partial read.
    check(peer.put_data(PackedByteArray([0])) == OK, "send first byte")
    await process_frame
    await process_frame
    check(peer.put_data(PackedByteArray([128, 255, 42])) == OK, "send tail")
    var received := PackedByteArray()
    for i in range(90):
        peer.poll()
        var available = peer.get_available_bytes()
        if available > 0:
            var data = peer.get_data(available)
            check(data[0] == OK, "receive response")
            received.append_array(data[1])
        if received.size() == 4 and node.call("completed") == expected:
            break
        await process_frame
    check(received == PackedByteArray([0, 128, 255, 42]), "binary response")
    check(node.call("completed") == expected, "completion belongs to the right snapshot")

func closed(peer: StreamPeerTCP) -> void:
    for i in range(60):
        peer.poll()
        if peer.get_status() != StreamPeerTCP.STATUS_CONNECTED:
            return
        await process_frame
    check(false, "cancel closes the peer and invalidates old readiness")

func run() -> void:
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    var script = load("res://network.jk")
    check(script != null and script.can_instantiate(), "network script compiles")
    var server = TCPServer.new()
    check(server.listen(0, "127.0.0.1") == OK, "listen")
    var port = server.get_local_port()
    var a = make(script)
    var b = make(script)
    a.call("task_exchange", port)
    var peer_a = await accept(server)
    b.call("task_exchange", port)
    var peer_b = await accept(server)
    var before = frames
    for i in range(8):
        await process_frame
    check(frames >= before + 8 and a.call("completed") == 0, "idle network never blocks frames")
    await echo(peer_b, b)
    await echo(peer_a, a)
    a.free()
    b.free()

    # Network deadlines use the executor's monotonic clock during engine pause.
    a = make(script)
    paused = true
    Engine.time_scale = 0.01
    a.call("task_timeout", port)
    peer_a = await accept(server)
    await create_timer(0.12, true, false, true).timeout
    check(a.call("timed_out") == 1, "network deadline ignores pause and time_scale")
    await echo(peer_a, a)
    a.free()
    paused = false
    Engine.time_scale = 1.0

    a = make(script)
    a.call("task_exchange", port)
    peer_a = await accept(server)
    var source = script.source_code
    script.source_code = source.replace("state.completed + 1", "state.completed + 100")
    check(script.reload(true) == OK, "reload with pending network I/O")
    await echo(peer_a, a)
    b = make(script)
    b.call("task_exchange", port)
    peer_b = await accept(server)
    await echo(peer_b, b, 100)
    a.free()
    b.free()
    script.source_code = source
    check(script.reload(true) == OK, "restore source")

    for i in range(12):
        a = make(script)
        a.call("task_exchange", port)
        peer_a = await accept(server)
        await process_frame
        if i % 2 == 0:
            a.set_script(null)
        a.free()
        # Late data after cancellation must not enter destroyed State.
        peer_a.put_partial_data(PackedByteArray([0, 128, 255, 42]))
        await closed(peer_a)
        check(language.active_task_count() == 0, "destroy/replacement releases network roots")
    check(language.active_instance_count() == 0, "network State roots released")
    server.stop()
    print("NET3 GODOT PASS")
    quit()
