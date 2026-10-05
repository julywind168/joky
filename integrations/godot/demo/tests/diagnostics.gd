extends SceneTree

func _initialize() -> void:
    call_deferred("run")

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("G0 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func run() -> void:
    var original = load("res://counter.jk")
    var language = joky_language()
    var script = ClassDB.instantiate("JokyScript")
    script.source_code = original.source_code.replace("state.count + 1", "state.count + true")
    check(ResourceSaver.save(script, "user://bad_type.jk") == OK, "save invalid editable script")
    check(script.reload() != OK, "type error rejected")
    check("user://bad_type.jk:6:" in language.last_diagnostic(), "type error file and line")
    script.source_code = original.source_code.replace("state.count + 1", "1 / state.count")
    check(ResourceSaver.save(script, "user://bad_runtime.jk") == OK, "save runtime fixture")
    check(script.reload() == OK, "runtime fixture compiles")
    var node = Node.new()
    node.set_script(script)
    root.add_child(node)
    check("user://bad_runtime.jk:6:" in language.last_diagnostic(), "runtime error file and line")
    node.free()
    check(language.active_instance_count() == 0, "failed state releases on deletion")
    script.source_code = original.source_code.replace("State { count: 0 }", "State { count: zero() }") + "\nfn zero() -> Int32 { var n = 0; 1 / n }\n"
    check(ResourceSaver.save(script, "user://bad_constructor.jk") == OK, "save failing constructor")
    check(script.reload() == OK, "failing constructor compiles")
    node = Node.new()
    node.set_script(script)
    check(language.active_instance_count() == 0, "failed constructor never publishes state")
    node.free()
    print("G0 DIAGNOSTICS PASS")
    quit()

func joky_language():
    for index in range(Engine.get_script_language_count()):
        var language = Engine.get_script_language(index)
        if language.get_class() == "JokyLanguage":
            return language
    return null
