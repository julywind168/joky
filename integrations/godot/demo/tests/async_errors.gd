extends SceneTree
func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("G2 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)
func _initialize() -> void:
    run.call_deferred()
func advance(frames: int = 3) -> void:
    for i in range(frames):
        await process_frame
func make(script) -> Node:
    var node = Node.new()
    var target = Node.new()
    target.name = "Target"
    node.add_child(target)
    node.set_script(script)
    root.add_child(node)
    return node
func run() -> void:
    var script = load("res://async_drop.jk")
    var language
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    var node = make(script)
    await advance(8)
    node.call("task_signal")
    node.call("task_bad")
    await advance(5)
    check(language.active_task_count() == 0, "root failure cancels sibling")
    check(node.name == "Dropped", "failed State destructor retains owner context")
    check(node.get_signal_connection_list("renamed").is_empty(), "failure disconnects sibling signal")
    check(language.last_diagnostic().contains("Division by zero"), "failure is reported once")
    node.free()
    node = make(script)
    await advance(8)
    node.call("task_emitter")
    await advance()
    node.get_node("Target").queue_free()
    await advance()
    check(language.active_task_count() == 0, "emitter exit wakes failed waiter")
    check(language.last_diagnostic().contains("Signal emitter left"), "emitter exit is a diagnostic")
    node.free()
    node = make(script)
    await advance(8)
    node.call("task_real", -1.0)
    await advance()
    check(language.active_task_count() == 0, "invalid timer leaves no root")
    check(language.last_diagnostic().contains("finite and nonnegative"), "duration validated")
    node.free()
    check(language.active_instance_count() == 0, "failed State roots close")
    print("G2 ERRORS PASS")
    quit()
