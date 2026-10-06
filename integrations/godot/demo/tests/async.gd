extends SceneTree

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("G2 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func _initialize() -> void:
    run.call_deferred()

func make(script, name: String) -> Node:
    var node = Node.new()
    node.name = name
    node.set_script(script)
    root.add_child(node)
    return node

func advance(frames: int = 3) -> void:
    for i in range(frames):
        await process_frame

func run() -> void:
    var script = load("res://async.jk")
    var language
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    check(script != null and script.can_instantiate(), "async script compiles")
    var a = make(script, "Alpha")
    var b = make(script, "Beta")
    check(a.call("count") == 1 and b.call("count") == 1, "ready executes synchronously")
    a.call("_ready")
    a.request_ready()
    root.remove_child(a)
    # Leaving the tree permanently ends this attachment's task lifetime.
    root.add_child(a)
    a.call("_ready")
    await advance(8)
    check(a.call("count") == 1 and a.call("frames") == 0, "repeated ready never duplicates roots after exit")
    check(b.call("frames") == 1 and b.call("physics") == 1, "real frame and physics waits")
    check(b.call("label_size") == 4, "task context uses owning node")
    a.free()
    a = make(script, "Alpha")
    await advance(8)
    check(a.call("label_size") == 5 and b.call("label_size") == 4, "shared program resolves two owners independently")
    a.call("task_access")
    await advance()
    check(a.call("count") == 11 and b.call("count") == 1, "State accesses release borrow while waiting")
    a.call("task_signal")
    await advance()
    check(a.get_signal_connection_list("renamed").size() == 1, "signal subscribes once")
    await advance(5)
    check(a.call("signals") == 0 and a.get_signal_connection_list("renamed").size() == 1, "pending signal neither replays nor duplicates")
    a.name = "RenamedAlpha"
    check(a.call("signals") == 0, "signal callback only enqueues readiness")
    await advance()
    check(a.call("signals") == 1 and a.get_signal_connection_list("renamed").size() == 0, "signal resumes and disconnects on completion")
    a.call("task_signal")
    await advance()
    var source = script.source_code
    script.source_code = source.replace("state.signals + 1", "state.signals + 100")
    check(script.reload(true) == OK, "reload with a pending task")
    a.name = "OldSnapshot"
    await advance()
    check(a.call("signals") == 2, "pending old task accesses old State and code")
    var fresh = make(script, "NewSnapshot")
    await advance(8)
    fresh.call("task_signal")
    await advance()
    fresh.name = "NewResult"
    await advance()
    check(fresh.call("signals") == 100, "new instance uses new code")
    fresh.free()
    script.source_code = source
    check(script.reload(true) == OK, "restore compiled snapshot")
    paused = true
    Engine.time_scale = 0.1
    a.call("task_game", 0.06)
    a.call("task_real", 0.06)
    await advance(2)
    await create_timer(0.12, true, false, true).timeout
    await advance()
    check(a.call("games") == 0 and a.call("reals") == 1, "pause stops game time; real time ignores pause and scale")
    paused = false
    Engine.time_scale = 1.0
    await create_timer(0.12).timeout
    await advance()
    check(a.call("games") == 1, "game timer resumes")
    a.call("task_race", 100.0)
    await advance()
    check(a.get_signal_connection_list("renamed").size() == 1, "race retains losing signal branch")
    for child in a.get_children(true):
        if child is Timer and child.is_ignoring_time_scale():
            child.emit_signal("timeout")
    await advance()
    check(a.call("reals") == 2 and a.get_signal_connection_list("renamed").size() == 0, "race stops losing waits")
    check(a.get_children(true).is_empty(), "cancelled long timers leave no engine child")
    a.call("task_group", 0.04)
    a.call("task_signal")
    await advance()
    check(language.active_task_count() == 2, "dynamic scope and separate root remain pending")
    var prior = process_frame.get_connections().size()
    a.queue_free()
    await advance()
    check(language.active_task_count() == 0 and language.active_instance_count() == 1, "deletion cancels root trees before removing State")
    check(process_frame.get_connections().size() <= prior, "deleted roots leave no frame subscription")
    for i in range(60):
        var node = make(script, "Cycle")
        node.call("task_signal")
        node.call("task_group", 10.0)
        await advance(2)
        node.free()
        check(language.active_task_count() == 0, "creation/destruction does not retain roots")
    b.free()
    var old_scene = Node.new()
    var attached = Node.new()
    attached.set_script(script)
    old_scene.add_child(attached)
    root.add_child(old_scene)
    current_scene = old_scene
    attached.call("task_signal")
    attached.call("task_group", 100.0)
    await advance()
    var next = Node.new()
    var packed = PackedScene.new()
    check(packed.pack(next) == OK, "pack replacement scene")
    next.free()
    check(change_scene_to_packed(packed) == OK, "switch scene")
    await advance()
    check(language.active_task_count() == 0, "scene switch cancels all scene tasks")
    check(language.active_instance_count() == 0, "all State roots released")
    print("G2 ASYNC PASS")
    quit()
