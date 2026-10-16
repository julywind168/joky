extends SceneTree
func _initialize() -> void:
    run.call_deferred()
func run() -> void:
    var language
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    assert(language != null and language.enable_file_access(true))
    var node = Node.new()
    node.set_script(load("res://files.jk"))
    root.add_child(node)
    node.call("task_files", 100000)
    await process_frame
    assert(language.active_task_count() > 0)
    print("FS SHUTDOWN PASS")
    quit()
