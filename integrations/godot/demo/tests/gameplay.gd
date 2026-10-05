extends SceneTree

func check(ok: bool, message: String) -> void:
    if not ok:
        push_error("G1 CHECK FAILED: " + message)
        quit(1)
        assert(ok, message)

func _initialize() -> void:
    run.call_deferred()

func run() -> void:
    var scene = load("res://movers.tscn").instantiate()
    var a = scene.get_node("MoverA")
    var b = scene.get_node("MoverB")
    var script = a.get_script()
    var language
    for i in range(Engine.get_script_language_count()):
        if Engine.get_script_language(i).get_class() == "JokyLanguage":
            language = Engine.get_script_language(i)
    check(script.get_property_default_value("speed") == 60.0, "static property default")
    check(script.get_script_property_list().size() == 1, "export metadata")
    check(a.get("speed") == 120.0 and b.get("speed") == 240.0, "scene overrides")
    root.add_child(scene)
    for i in range(12):
        await process_frame
    check(a.call("frames") > 1 and b.call("frames") > 1, "real process callbacks")
    check(a.call("physics") > 0 and b.call("physics") > 0, "real physics callbacks")
    check(is_equal_approx(a.position.x, 120.0 * a.call("elapsed")), "Float64 delta and vector conversion")
    check(is_equal_approx(b.position.x, 240.0 * b.call("elapsed")), "independent speed and state")
    var prior = a.position.x
    a.set("speed", 0.0)
    for i in range(5):
        await process_frame
    check(a.position.x == prior and b.position.x > prior, "property mutation controls movement")
    check(a.call("label_size") == 6, "Node name copied to persistent String")
    a.call("rename")
    check(a.name == "Renamed", "checked Node rename")
    var packed = PackedScene.new()
    check(packed.pack(scene) == OK, "pack property state")
    check(ResourceSaver.save(packed, "user://g1_roundtrip.tscn") == OK, "persist overridden values")
    a.call("forget_target")
    check(not a.call("target_valid") and b.call("target_valid"), "queued deletion invalidates only its capability")
    await process_frame
    check(a.get_node_or_null("Target") == null, "engine deletes target")
    var old_source = script.source_code
    script.source_code = old_source.replace("60.0 }", "30.0 }")
    check(script.reload(true) == OK, "compile updated property defaults")
    check(a.get("speed") == 0.0 and b.get("speed") == 240.0, "old roots keep state")
    var fresh = Node2D.new()
    var target = Node.new()
    target.name = "Target"
    fresh.add_child(target)
    fresh.set_script(script)
    check(fresh.get("speed") == 30.0, "new root applies new default")
    root.add_child(fresh)
    fresh.queue_free()
    await process_frame
    script.source_code = old_source
    check(script.reload(true) == OK, "restore snapshot")
    scene.free()
    check(language.active_instance_count() == 0, "all roots close")
    for i in range(100):
        fresh = Node2D.new()
        fresh.set_script(script)
        fresh.free()
    check(language.active_instance_count() == 0, "repeated creation and destruction")
    if OS.get_environment("JOKY_G1_VISUAL_TEST") == "1":
        var preview = load("res://movers.tscn").instantiate()
        root.add_child(preview)
        for i in range(8):
            await process_frame
        await RenderingServer.frame_post_draw
        check(root.get_texture().get_image().save_png("/tmp/joky-g1.png") == OK, "capture rendered scene")
        preview.free()
    print("G1 GAMEPLAY PASS")
    quit()
