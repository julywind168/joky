extends SceneTree

func _initialize() -> void:
    run.call_deferred()

func run() -> void:
    var node = Node.new()
    node.set_script(load("res://network_dns.jk"))
    root.add_child(node)
    node.call("task_resolve", 1000)
    await process_frame
    # Quit with a live task; engine teardown must cancel without joining lookup.
    print("NET6 SHUTDOWN PASS")
    quit(0)
