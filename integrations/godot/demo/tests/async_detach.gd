extends SceneTree
func _initialize() -> void:
    run.call_deferred()
func run() -> void:
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
    if names != ["DetachNow", "StateDropped"]:
        push_error("G2 CHECK FAILED: deferred close lost cleanup context: " + str(names))
        quit(1)
        return
    node.free()
    print("G2 DETACH PASS")
    quit()
