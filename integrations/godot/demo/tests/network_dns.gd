extends SceneTree

var language

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("NET6 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func _initialize() -> void:
    run.call_deferred()

func make(script) -> Node:
    var node = Node.new()
    node.set_script(script)
    root.add_child(node)
    return node

func idle() -> void:
    for i in range(600):
        if language.active_task_count() == 0:
            return
        await process_frame
    check(false, "DNS tasks terminate")

func run() -> void:
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    check(language != null, "language registered")
    var script = load("res://network_dns.jk")
    check(script != null and script.can_instantiate(), "DNS script compiles")
    var first = make(script)
    var second = make(script)
    first.call("task_resolve", 16)
    first.call("task_frames", 8)
    second.call("task_resolve", 32)
    await idle()
    check(first.call("completed") == 16 and second.call("completed") == 32, "results stay with their instance")
    check(first.call("ticks") == 8, "frame tasks progress alongside lookup")
    check(first.get_script() != null, "lookup completes without detaching owner")

    first.call("task_resolve", 1000)
    await process_frame
    check(language.active_task_count() > 0, "long lookup sequence remains active")
    first.set_script(null)
    await idle()
    var fresh = make(script)
    fresh.call("task_resolve", 2)
    await idle()
    check(fresh.call("completed") == 2, "new owner does not receive old results")

    second.call("task_resolve", 1000)
    await process_frame
    second.free()
    await idle()
    fresh.call("task_resolve", 1000)
    await process_frame
    fresh.free()
    first.free()
    await idle()
    check(language.active_instance_count() == 0, "DNS owners are released")
    print("NET6 GODOT PASS")
    quit(0)
