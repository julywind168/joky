extends SceneTree
func _initialize() -> void:
    run.call_deferred()
func run() -> void:
    var node = Node.new()
    node.set_script(load("res://async.jk"))
    root.add_child(node)
    node.call("task_signal")
    node.call("task_group", 100.0)
    for i in range(3):
        await process_frame
    print("G2 SHUTDOWN PASS")
    quit()
