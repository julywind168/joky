extends SceneTree
var language
func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("FS CHECK FAILED: " + message)
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
    for i in range(2400):
        if language.active_task_count() == 0:
            return
        await process_frame
    check(false, "file tasks terminate")
func run() -> void:
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    var missing_script = load("res://files_missing.jk")
    var missing = make(missing_script)
    missing.call("task_missing")
    await idle()
    check(missing.call("completed") == 1, "default capability is Unsupported")
    check(language.enable_file_access(true), "enable engine FileAccess")
    var script = load("res://files.jk")
    check(script.can_instantiate(), "file script compiles")
    var first = make(script)
    first.call("task_files", 8)
    first.call("task_frames", 8)
    await idle()
    check(first.call("completed") == 8 and first.call("ticks") == 8, "file work and frames both progress")
    check(FileAccess.get_file_as_string("user://joky_fs.bin") == "Hello, Joky FS\n", "engine path bytes roundtrip")
    check(language.enable_file_access(false), "disable for future snapshots")
    missing.call("task_missing")
    first.call("task_files", 2)
    await idle()
    check(missing.call("completed") == 2 and first.call("completed") == 10, "capabilities stay with snapshots")
    first.call("task_files", 1000)
    await process_frame
    first.set_script(null)
    await idle()
    var second = make(script)
    second.call("task_files", 2)
    await idle()
    check(second.call("completed") == 2, "new instance cannot receive cancelled results")
    check(language.enable_file_access(true), "enable for reloaded snapshot")
    second.call("task_files", 3)
    await process_frame
    var original = script.source_code
    script.source_code = original.replace("state.completed + 1", "state.completed + 100")
    check(script.reload(true) == OK, "reload during file task")
    await idle()
    check(second.call("completed") == 5, "old task keeps old code and service")
    var fresh = make(script)
    fresh.call("task_files", 1)
    await idle()
    check(fresh.call("completed") == 100, "new snapshot uses new code")
    fresh.free()
    script.source_code = original
    check(script.reload(true) == OK, "restore file script")
    second.call("task_files", 1000)
    await process_frame
    second.free()
    first.free()
    missing.free()
    await idle()
    check(language.active_instance_count() == 0, "file instances release")
    print("FS GODOT PASS")
    quit()
