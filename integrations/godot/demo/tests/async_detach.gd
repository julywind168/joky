extends SceneTree
func _initialize() -> void:
    run.call_deferred()
func run() -> void:
    var language
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    var node = Node.new()
    node.set_script(load("res://async_detach.jk"))
    root.add_child(node)
    var names: Array[String] = []
    node.renamed.connect(func():
        names.append(str(node.name))
        if node.name == "DetachNow":
            node.set_script(null)
    )
    node.call("task_detach")
    for i in range(5):
        await process_frame
    if names != ["DetachNow"] or node.get_script() != null or language.active_instance_count() != 0 or language.active_task_count() != 0:
        push_error("G2 CHECK FAILED: reentrant detach did not close tasks and State: " + str(names))
        quit(1)
        return
    node.free()
    print("G2 DETACH PASS")
    quit()
