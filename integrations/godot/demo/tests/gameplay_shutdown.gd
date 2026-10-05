extends SceneTree
func _initialize() -> void:
    run.call_deferred()
func run() -> void:
    var scene = load("res://movers.tscn").instantiate()
    root.add_child(scene)
    for i in range(4):
        await process_frame
    # Leave live owning state to the engine's ordinary stop/destruction path.
    print("G1 SHUTDOWN PASS")
    quit()
