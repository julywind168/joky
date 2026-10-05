extends SceneTree
func _initialize() -> void:
    var scene = load("user://g1_roundtrip.tscn").instantiate()
    var a = scene.get_node("Renamed")
    var b = scene.get_node("MoverB")
    if not a or a.get("speed") != 0.0 or b.get("speed") != 240.0:
        push_error("G1 CHECK FAILED: properties did not survive a separate process")
        quit(1)
        return
    root.add_child(scene)
    scene.free()
    var editor_scene = load("user://g1_editor_roundtrip.tscn").instantiate()
    if editor_scene.get_node("MoverA").get("speed") != 90.0:
        push_error("G1 CHECK FAILED: editor override did not survive scene save")
        quit(1)
        return
    editor_scene.free()
    print("G1 REOPEN PASS")
    quit()
