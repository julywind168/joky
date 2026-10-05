extends SceneTree

func _initialize() -> void:
    call_deferred("run")

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("G0 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func run() -> void:
    var script = load("res://counter.jk")
    check(script is Script, "Joky is a Script resource")
    var language = joky_language()
    check(language.get_class() == "JokyLanguage", "language registered")
    check(language.active_instance_count() == 0, "loading does not construct state")
    var scene = load("res://main.tscn").instantiate()
    root.add_child(scene)
    var a = scene.get_node("CounterA")
    var b = scene.get_node("CounterB")
    check(a.get_class() == "Node", "ordinary Node owner")
    check(a.get_script() == b.get_script(), "shared script resource")
    check(a.call("count") == 1 and b.call("count") == 1, "ready exactly once")
    check(a.call("bump", 10) == 11 and b.call("count") == 1, "independent state")
    check(a.call("echo_float", 0.125) == 0.125, "float boundary")
    check(a.call("echo_rune", "界") == "界", "rune boundary")
    check(a.call("echo_byte", 255) == 255, "unsigned boundary")
    check(a.call("echo_small", -128) == -128, "Int8 boundary")
    check(a.call("echo_short", -32768) == -32768, "Int16 boundary")
    check(a.call("echo_long", -9223372036854775807) == -9223372036854775807, "Int64 boundary")
    check(a.call("echo_ushort", 65535) == 65535, "UInt16 boundary")
    check(a.call("echo_uint", 4294967295) == 4294967295, "UInt32 boundary")
    check(a.call("echo_ulong", 9223372036854775807) == 9223372036854775807, "UInt64 boundary")
    check(a.call("echo_single", 0.125) == 0.125, "Float32 boundary")
    check(a.call("echo_bool", true), "Bool boundary")
    check(a.call("echo_unit", null) == null, "Unit boundary")
    check(script.instance_has(a), "script tracks owners")
    var packed = PackedScene.new()
    check(packed.pack(scene) == OK, "pack scene")
    check(ResourceSaver.save(packed, "user://roundtrip.tscn") == OK, "save scene")
    check(ResourceSaver.save(script, "user://copy.jk") == OK, "save source")
    var old_source = script.source_code
    script.source_code = old_source.replace("state.count + 1", "state.count + 2")
    check(script.reload(true) == OK, "compile replacement snapshot")
    check(a.call("count") == 11, "old instance survives reload")
    var fresh = Node.new()
    fresh.set_script(script)
    root.add_child(fresh)
    check(fresh.call("count") == 2, "new instance uses new snapshot")
    fresh.set_script(null)
    check(language.active_instance_count() == 2, "detaching closes the root")
    fresh.free()
    script.source_code = old_source
    check(script.reload(true) == OK, "restore source")
    scene.free()
    check(language.active_instance_count() == 0, "scene deletion releases roots")
    var copy = load("user://copy.jk")
    var node = Node.new()
    node.set_script(copy)
    root.add_child(node)
    check(node.call("count") == 1, "saved source runs")
    node.free()
    for i in range(100):
        node = Node.new()
        node.set_script(script)
        root.add_child(node)
        check(node.call("count") == 1, "recreated state")
        node.free()
    check(language.active_instance_count() == 0, "repeated destruction releases roots")
    print("G0 HEADLESS PASS")
    quit()

func joky_language():
    for index in range(Engine.get_script_language_count()):
        var language = Engine.get_script_language(index)
        if language.get_class() == "JokyLanguage":
            return language
    return null
