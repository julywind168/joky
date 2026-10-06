extends SceneTree
func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("G2 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)
func _initialize() -> void:
    run.call_deferred()
func run() -> void:
    Engine.time_scale = 0.01
    var scene = load("res://async.tscn").instantiate()
    root.add_child(scene)
    for i in range(20):
        await process_frame
    var a = scene.get_node("MoverA")
    var b = scene.get_node("MoverB")
    check(a.position.x > 160.0, "frame branch moves Node2D")
    check(is_equal_approx(b.position.x - 160.0, (a.position.x - 160.0) * 2.0), "independent scene strides")
    scene.free()
    print("G2 MOTION PASS")
    quit()
