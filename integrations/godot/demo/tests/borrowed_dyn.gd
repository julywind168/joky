extends SceneTree

func _initialize() -> void:
    call_deferred("run")

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("DYN CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func run() -> void:
    var script = load("res://borrowed_dyn.jk")
    check(script is Script, "dyn script loads")
    var a = Node.new()
    var b = Node.new()
    a.set_script(script)
    b.set_script(script)
    root.add_child(a)
    root.add_child(b)
    check(a.call("answer") == 42 and b.call("answer") == 42, "shared views")
    check(a.call("change", 22) == 44 and b.call("answer") == 42, "exclusive reborrow and independent state")
    var original = script.source_code
    script.source_code = original.replace("self.value() * 2", "self.value() * 3")
    check(script.reload(true) == OK, "new dyn table compiles")
    var fresh = Node.new()
    fresh.set_script(script)
    root.add_child(fresh)
    check(a.call("answer") == 44 and fresh.call("answer") == 63, "old and new method tables")
    script.source_code = original
    check(script.reload(true) == OK, "restore script")
    a.free()
    b.free()
    fresh.free()
    for i in range(32):
        var node = Node.new()
        node.set_script(script)
        root.add_child(node)
        check(node.call("change", 21) == 42, "repeat short views")
        node.free()
    for index in range(Engine.get_script_language_count()):
        var language = Engine.get_script_language(index)
        if language.get_class() == "JokyLanguage":
            check(language.active_instance_count() == 0, "roots released")
    print("DYN GODOT PASS")
    quit()
