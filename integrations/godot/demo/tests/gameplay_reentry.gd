extends SceneTree
func _initialize() -> void:
    run.call_deferred()
func run() -> void:
    var scene = load("res://movers.tscn").instantiate()
    root.add_child(scene)
    var a = scene.get_node("MoverA")
    var b = scene.get_node("MoverB")
    a.renamed.connect(func():
        a.set("speed", 99.0)
        b.set("speed", 77.0)
    )
    # The Node API may synchronously emit signals. A's root stays Busy while
    # the adapter permits the engine to reenter; B is independently accessible.
    a.call("rename")
    if a.get("speed") != 120.0 or b.get("speed") != 77.0 or a.name != "Renamed":
        push_error("G1 CHECK FAILED: reentry mutated the Busy root or blocked the other instance")
        quit(1)
        return
    scene.free()
    print("G1 REENTRY PASS")
    quit()
