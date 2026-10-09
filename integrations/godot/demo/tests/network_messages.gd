extends SceneTree

var frames := 0
var language

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("NET4 CHECK FAILED: " + message)
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
    for i in range(120):
        if server.is_connection_available():
            return server.take_connection()
        await process_frame
    check(false, "message client connects across frames")
    return null

func completed(node: Node, getter: String, expected: int = 1) -> void:
    for i in range(180):
        if node.call(getter) == expected:
            return
        await process_frame
    check(false, "message task completes: " + getter)

func closed(peer: StreamPeerTCP) -> void:
    for i in range(120):
        peer.poll()
        if peer.get_status() != StreamPeerTCP.STATUS_CONNECTED:
            return
        await process_frame
    check(false, "protocol close disconnects peer")

func exchange(peer: StreamPeerTCP, node: Node, expected: int = 1) -> void:
    # Split header, payload and a UTF-8 scalar; merge following frames and lines.
    check(peer.put_data(PackedByteArray([0, 0])) == OK, "send partial header")
    await process_frame
    await process_frame
    check(peer.put_data(PackedByteArray([0, 3, 0])) == OK, "send header and partial payload")
    await process_frame
    check(peer.put_data(PackedByteArray([255, 42, 0, 0, 0, 0, 228, 189])) == OK, "merge message and UTF-8 prefix")
    await process_frame
    check(peer.put_data(PackedByteArray([160, 0, 229, 165, 189, 13, 10, 10])) == OK, "complete text and blank line")
    var reply := PackedByteArray()
    for i in range(180):
        peer.poll()
        var available = peer.get_available_bytes()
        if available > 0:
            var data = peer.get_data(available)
            check(data[0] == OK, "read framed response")
            reply.append_array(data[1])
        if reply.size() == 19 and node.call("completed") == expected:
            break
        await process_frame
    check(reply == PackedByteArray([0, 0, 0, 3, 0, 255, 42, 0, 0, 0, 0, 228, 189, 160, 0, 229, 165, 189, 10]), "binary frames and UTF-8/NUL line response")
    check(node.call("completed") == expected, "completion uses the correct snapshot")
    await closed(peer)

func run() -> void:
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    var script = load("res://network_messages.jk")
    check(script != null and script.can_instantiate(), "buffered network script compiles")
    var server = TCPServer.new()
    check(server.listen(0, "127.0.0.1") == OK, "listen")
    var port = server.get_local_port()
    var a = make(script)
    var b = make(script)
    a.call("task_messages", port)
    var peer_a = await accept(server)
    b.call("task_messages", port)
    var peer_b = await accept(server)
    var before = frames
    for i in range(8):
        await process_frame
    check(frames >= before + 8 and a.call("completed") == 0, "pending frames leave engine responsive")
    await exchange(peer_b, b)
    await exchange(peer_a, a)
    a.free()
    b.free()

    a = make(script)
    paused = true
    Engine.time_scale = 0.01
    a.call("task_timeout", port)
    peer_a = await accept(server)
    check(peer_a.put_data(PackedByteArray([0, 0, 0, 3, 42])) == OK, "send incomplete timed message")
    await create_timer(0.12, true, false, true).timeout
    check(a.call("timed_out") == 1, "protocol deadline ignores pause and scale")
    await closed(peer_a)
    check(language.active_task_count() == 0, "timeout ends protocol task")
    a.free()
    paused = false
    Engine.time_scale = 1.0

    a = make(script)
    a.call("task_reject", port)
    peer_a = await accept(server)
    check(peer_a.put_data(PackedByteArray([255, 255, 255, 255])) == OK, "untrusted length header")
    await completed(a, "rejected")
    await closed(peer_a)
    a.free()
    a = make(script)
    a.call("task_truncated", port)
    peer_a = await accept(server)
    check(peer_a.put_data(PackedByteArray([0, 0, 0, 3, 42])) == OK, "truncated payload")
    peer_a.disconnect_from_host()
    await completed(a, "truncated")
    a.free()

    a = make(script)
    a.call("task_messages", port)
    peer_a = await accept(server)
    var source = script.source_code
    script.source_code = source.replace("state.completed + 1", "state.completed + 100")
    check(script.reload(true) == OK, "reload pending buffered owner")
    await exchange(peer_a, a)
    b = make(script)
    b.call("task_messages", port)
    peer_b = await accept(server)
    await exchange(peer_b, b, 100)
    a.free()
    b.free()
    script.source_code = source
    check(script.reload(true) == OK, "restore buffered script")

    for i in range(12):
        a = make(script)
        a.call("task_wait", port)
        peer_a = await accept(server)
        var prefix = PackedByteArray([0, 0]) if i % 2 == 0 else PackedByteArray([0, 0, 0, 3, 42])
        check(peer_a.put_data(prefix) == OK, "partial message before cancellation")
        for frame in range(3):
            await process_frame
        if i % 3 == 0:
            a.set_script(null)
        elif i % 3 == 1:
            root.remove_child(a)
        a.free()
        peer_a.put_partial_data(PackedByteArray([42, 42, 0, 0, 0, 0]))
        await closed(peer_a)
        check(language.active_task_count() == 0, "destroy/detach/exit cancels cached message task")
    check(language.active_instance_count() == 0, "buffered task State roots released")
    server.stop()
    print("NET4 GODOT PASS")
    quit()
