extends SceneTree

func _initialize() -> void:
    call_deferred("run")

func run() -> void:
    var packed = load("user://roundtrip.tscn")
    if packed == null:
        quit(1)
        return
    var scene = packed.instantiate()
    root.add_child(scene)
    var a = scene.get_node("CounterA")
    var b = scene.get_node("CounterB")
    if a.get_script().resource_path != "res://counter.jk" or a.call("count") != 1 or b.call("count") != 1:
        push_error("G0 saved scene binding failed")
        scene.free()
        quit(1)
        return
    scene.free()
    print("G0 REOPEN PASS")
    quit()
