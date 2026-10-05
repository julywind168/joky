@tool
extends EditorPlugin

func _enter_tree() -> void:
    if OS.get_environment("JOKY_G0_EDITOR_TEST") == "1":
        run.call_deferred()

func run() -> void:
    # Keep Godot's actual editor main loop and wait for its startup scan.
    for i in range(3):
        await get_tree().process_frame
    while EditorInterface.get_resource_filesystem().is_scanning():
        await get_tree().process_frame
    EditorInterface.open_scene_from_path("res://main.tscn")
    await get_tree().process_frame
    var scene = EditorInterface.get_edited_scene_root()
    var language
    for index in range(Engine.get_script_language_count()):
        var candidate = Engine.get_script_language(index)
        if candidate.get_class() == "JokyLanguage":
            language = candidate
    var script = scene.get_node("CounterA").get_script()
    if not Engine.is_editor_hint() or script.can_instantiate() or language.active_instance_count() != 0:
        push_error("G0 editor must not execute script constructors")
        get_tree().quit(1)
        return
    EditorInterface.edit_script(script)
    await get_tree().process_frame
    var code = EditorInterface.get_script_editor().get_current_editor().get_base_editor()
    if not code is CodeEdit:
        push_error("G0 script editor did not open the Joky source")
        get_tree().quit(1)
        return
    code.emit_signal("symbol_validate", "State")
    code.emit_signal("code_completion_requested")
    await get_tree().process_frame
    var packed = PackedScene.new()
    var result = packed.pack(scene)
    if result == OK:
        result = ResourceSaver.save(packed, "user://editor_roundtrip.tscn")
    if result != OK:
        get_tree().quit(1)
        return
    print("G0 EDITOR PASS")
    get_tree().quit()
