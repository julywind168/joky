extends SceneTree

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("G1 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func _initialize() -> void:
    run.call_deferred()

func make_node(script: Script, spatial: bool = true) -> Node:
    var node = Node2D.new() if spatial else Node.new()
    var target = Node.new()
    target.name = "Target"
    node.add_child(target)
    node.set_script(script)
    return node

func run() -> void:
    var source = load("res://mover.jk").source_code
    var invalid = ClassDB.instantiate("JokyScript")
    invalid.source_code = source.replace("state.target = node.get_node(node.self(), &path)", "state.target = node.get_node(node.self(), &path); invalid_position(state)")
    check(invalid.reload() == OK, "invalid vector fixture compiles")
    var a = make_node(invalid)
    var before = a.position
    # Engine lifecycle callbacks report a script failure without aborting this harness.
    root.add_child(a)
    check(a.position == before, "invalid vector does not partially mutate a Node")
    var stale = ClassDB.instantiate("JokyScript")
    stale.source_code = source.replace("node.translate(node.self(), Vector2 { x: state.speed * delta, y: 0.0 })", "stale_access(state)")
    check(stale.reload() == OK, "stale capability fixture compiles")
    var b = make_node(stale)
    root.add_child(b)
    b.get_node("Target").free()
    check(not b.call("target_valid"), "freed handle is invalid")
    var wrong_class = make_node(load("res://mover.jk"), false)
    root.add_child(wrong_class)
    # Each failing callback runs once; later frames stay quiet until destruction.
    for i in range(5):
        await process_frame
    a.free()
    b.free()
    wrong_class.free()
    var collision = ClassDB.instantiate("JokyScript")
    collision.source_code = source.replace("default_speed", "default_name").replace("get_speed", "get_name").replace("set_speed", "set_name")
    check(ResourceSaver.save(collision, "user://g1_collision.jk") == OK, "save property collision fixture")
    check(collision.reload() == OK, "collision fixture is valid Joky")
    var native = Node2D.new()
    native.set_script(collision)
    native.free()
    print("G1 DIAGNOSTICS PASS")
    quit()
