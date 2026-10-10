extends SceneTree

var language
var buffers := {}

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("JSON3 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

# Godot parses JSON numbers as floats; compare both trees after independent parsing.
func same(actual, expected) -> bool:
    return actual == JSON.parse_string(JSON.stringify(expected))

func _initialize() -> void:
    run.call_deferred()

func make(script) -> Node:
    var node = Node.new()
    node.set_script(script)
    root.add_child(node)
    return node

func accept(server: TCPServer) -> StreamPeerTCP:
    for i in range(600):
        if server.is_connection_available():
            return server.take_connection()
        await process_frame
    check(false, "JSON client connects")
    return null

func framed(bytes: PackedByteArray) -> PackedByteArray:
    var size = bytes.size()
    var wire = PackedByteArray([(size >> 24) & 255, (size >> 16) & 255, (size >> 8) & 255, size & 255])
    wire.append_array(bytes)
    return wire

func post(peer: StreamPeerTCP, value) -> void:
    check(peer.put_data(framed(JSON.stringify(value).to_utf8_buffer())) == OK, "send independent JSON encoding")

func collect(peer: StreamPeerTCP) -> void:
    peer.poll()
    if peer.get_status() != StreamPeerTCP.STATUS_CONNECTED:
        return
    var key = peer.get_instance_id()
    var bytes: PackedByteArray = buffers.get(key, PackedByteArray())
    var available = peer.get_available_bytes()
    if available > 0:
        var data = peer.get_data(available)
        check(data[0] == OK, "receive JSON bytes")
        bytes.append_array(data[1])
    buffers[key] = bytes

func take(peer: StreamPeerTCP):
    var key = peer.get_instance_id()
    var bytes: PackedByteArray = buffers.get(key, PackedByteArray())
    if bytes.size() < 4:
        return null
    var size = (bytes[0] << 24) | (bytes[1] << 16) | (bytes[2] << 8) | bytes[3]
    check(size <= 65536, "bounded response frame")
    if bytes.size() < size + 4:
        return null
    var parser = JSON.new()
    check(parser.parse(bytes.slice(4, size + 4).get_string_from_utf8()) == OK, "Godot accepts Joky encoding")
    check(parser.data is Array and parser.data.size() == 3, "application envelope")
    buffers[key] = bytes.slice(size + 4)
    return parser.data

func message(peer: StreamPeerTCP) -> Array:
    for i in range(600):
        collect(peer)
        var value = take(peer)
        if value != null:
            return value
        await process_frame
    check(false, "JSON response arrives")
    return []

func status(node: Node, getter: String, expected: int) -> void:
    for i in range(600):
        if node.call(getter) == expected:
            return
        await process_frame
    check(false, "scalar status: " + getter)

func closed(peer: StreamPeerTCP) -> void:
    for i in range(180):
        peer.poll()
        if peer.get_status() != StreamPeerTCP.STATUS_CONNECTED:
            buffers.erase(peer.get_instance_id())
            return
        if peer.get_available_bytes() > 0:
            peer.get_data(peer.get_available_bytes())
        await process_frame
    check(false, "both JSON socket halves close")

func idle() -> void:
    for i in range(180):
        if language.active_task_count() == 0:
            return
        await process_frame
    check(false, "JSON tasks release")

func start(node: Node, server: TCPServer, port: int, deadline: int = 10000) -> StreamPeerTCP:
    node.call("task_session", port, deadline)
    var peer = await accept(server)
    var value = await message(peer)
    check(same(value, ["push", 0, "你🙂"]), "unsolicited Unicode push")
    return peer

func finish(node: Node, peer: StreamPeerTCP, expected: int = 1) -> void:
    post(peer, ["end", 0, null])
    await status(node, "completed", expected)
    await closed(peer)

func exchange(node: Node, peer: StreamPeerTCP, id: int) -> void:
    var payload = {"nested": [null, true, "你🙂"], "amount": -12.5}
    var wire = framed(JSON.stringify(["request", id, payload]).to_utf8_buffer())
    # Split header and UTF-8 scalars; following complete bad frames share one write.
    for byte in wire:
        check(peer.put_data(PackedByteArray([byte])) == OK, "fragment JSON frame")
        if byte >= 128:
            await process_frame
    check(same(await message(peer), ["reply", id, payload]), "independent semantic roundtrip")
    var bad = framed("[".to_utf8_buffer())
    bad.append_array(framed(PackedByteArray([255])))
    bad.append_array(framed('["request",1.5,null]'.to_utf8_buffer()))
    bad.append_array(framed('["request",42,true]'.to_utf8_buffer()))
    check(peer.put_data(bad) == OK, "coalesce errors and valid request")
    check(same(await message(peer), ["error", 0, "invalid-json"]), "malformed JSON is recoverable")
    check(same(await message(peer), ["error", 0, "invalid-json"]), "invalid UTF-8 is recoverable")
    check(same(await message(peer), ["error", 0, "invalid-request"]), "schema validation rejects fractional ID")
    check(same(await message(peer), ["reply", 42, true]), "valid request after errors")
    await status(node, "replies", 2)
    await status(node, "errors", 3)

func pressure(peer: StreamPeerTCP, node: Node) -> void:
    var previous = -1
    var stable = 0
    for i in range(240):
        var count = node.call("sent")
        stable = stable + 1 if count > 0 and count == previous else 0
        if stable >= 8:
            check(count < 512, "writes really stall")
            return
        check(count < 512, "receive window creates backpressure")
        previous = count
        await process_frame
    check(false, "pending JSON write observed")

func run() -> void:
    for i in range(Engine.get_script_language_count()):
        var candidate = Engine.get_script_language(i)
        if candidate.get_class() == "JokyLanguage":
            language = candidate
    check(language != null, "Joky registered")
    var script = load("res://network_json.jk")
    check(script != null and script.can_instantiate(), "JSON script compiles")
    var server = TCPServer.new()
    check(server.listen(0, "127.0.0.1") == OK, "native JSON listener")
    var port = server.get_local_port()
    var a = make(script)
    var b = make(script)
    var peer_a = await start(a, server, port)
    var peer_b = await start(b, server, port)
    await exchange(a, peer_a, 1)
    check(b.call("replies") == 0 and b.call("errors") == 0, "two instances stay isolated")
    await exchange(b, peer_b, 2)
    await finish(a, peer_a)
    await finish(b, peer_b)
    a.free()
    b.free()
    await idle()

    a = make(script)
    peer_a = await start(a, server, port, 1000)
    check(peer_a.put_data(PackedByteArray([0, 0, 0, 20, 91])) == OK, "pending JSON payload before pause")
    paused = true
    Engine.time_scale = 0.01
    await create_timer(1.2, true, false, true).timeout
    check(a.call("timed_out") == 1 and a.call("completed") == 0, "real-time deadline runs while paused")
    await closed(peer_a)
    a.free()
    paused = false
    Engine.time_scale = 1.0
    await idle()

    a = make(script)
    peer_a = await start(a, server, port)
    var snapshot = script.source_code
    script.source_code = snapshot.replace("state.completed + 1", "state.completed + 100")
    check(script.reload(true) == OK, "reload with pending JSON session")
    b = make(script)
    peer_b = await start(b, server, port)
    await exchange(a, peer_a, 3)
    await exchange(b, peer_b, 4)
    await finish(a, peer_a, 1)
    await finish(b, peer_b, 100)
    a.free()
    b.free()
    await idle()
    script.source_code = snapshot
    check(script.reload(true) == OK, "restore JSON snapshot")

    a = make(script)
    a.call("task_pressure", port)
    peer_a = await accept(server)
    var first = await message(peer_a)
    check(same(first, ["request", 7, "x".repeat(8192)]), "bounded representative JSON payload")
    await pressure(peer_a, a)
    var pushed = framed(JSON.stringify(["push", 99, {"alive": true}]).to_utf8_buffer())
    check(peer_a.put_data(pushed.slice(0, 7)) == OK, "partial push during pending JSON write")
    await process_frame
    var rest = pushed.slice(7)
    rest.append_array(pushed)
    rest.append_array(pushed)
    check(peer_a.put_data(rest) == OK, "coalesced JSON pushes during backpressure")
    await status(a, "received", 3)
    await pressure(peer_a, a)
    var count = 1
    for i in range(1200):
        collect(peer_a)
        var value = take(peer_a)
        while value != null:
            check(value == first, "every framed JSON payload accepted independently")
            count += 1
            value = take(peer_a)
        if count == 512 and a.call("completed") == 1:
            break
        await process_frame
    check(count == 512 and a.call("sent") == 512 and a.call("completed") == 1, "drain resumes exact JSON wire and retained tree")
    await closed(peer_a)
    a.free()
    await idle()

    # Destroy task owners with partial headers/payloads and with borrowed JSON writes.
    for case in range(9):
        a = make(script)
        if case >= 6:
            a.call("task_pressure", port)
            peer_a = await accept(server)
            await message(peer_a)
            await pressure(peer_a, a)
        else:
            peer_a = await start(a, server, port)
        check(peer_a.put_data(PackedByteArray([0, 0]) if case % 2 == 0 else PackedByteArray([0, 0, 0, 30, 91])) == OK, "cancel at frame boundary")
        await process_frame
        if case % 3 == 0:
            a.set_script(null)
        elif case % 3 == 1:
            root.remove_child(a)
        a.free()
        b = make(script)
        peer_b = await start(b, server, port)
        peer_a.put_partial_data(framed('["request",1,true]'.to_utf8_buffer()))
        await closed(peer_a)
        await process_frame
        check(b.call("replies") == 0 and b.call("errors") == 0 and b.call("completed") == 0, "late old bytes do not reach new JSON owner")
        check(language.active_task_count() == 1, "old JSON scope canceled")
        await finish(b, peer_b)
        b.free()
        await idle()
        check(language.active_instance_count() == 0, "JSON instance storage released")

    a = make(script)
    peer_a = await start(a, server, port)
    check(peer_a.put_data(PackedByteArray([0, 1, 0, 1])) == OK, "declared frame exceeds transport cap")
    await status(a, "broken", 1)
    check(a.call("errors") == 0, "transport failure is not JSON error")
    await closed(peer_a)
    a.free()
    await idle()
    server.stop()
    print("JSON3 GODOT PASS")
    quit()
