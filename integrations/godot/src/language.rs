//! Script editor integration. Unsupported debugger/profiler features report empty data.
use crate::{
    runtime::{Program, TEMPLATE},
    script::JokyScript,
};
use godot::{
    builtin::*,
    classes::{
        native::ScriptLanguageExtensionProfilingInfo, IScriptLanguageExtension,
        ScriptLanguageExtension,
    },
    global::Error,
    prelude::*,
};
use std::ffi::c_void;

#[derive(GodotClass)]
#[class(init, base=ScriptLanguageExtension, tool)]
pub struct JokyLanguage {
    engine_base: Base<ScriptLanguageExtension>,
}
#[godot_api]
impl JokyLanguage {
    #[func]
    fn enable_file_access(&self, enabled: bool) -> bool {
        crate::files::configure(enabled).is_ok()
    }
    #[func]
    fn active_instance_count(&self) -> i64 {
        crate::LIVE.with(|n| n.get() as i64)
    }
    #[func]
    fn active_task_count(&self) -> i64 {
        crate::tasks::active_task_count() as i64
    }
    #[func]
    fn last_diagnostic(&self) -> GString {
        crate::LAST_ERROR.with(|e| GString::from(e.borrow().as_str()))
    }
}
#[godot_api]
impl IScriptLanguageExtension for JokyLanguage {
    fn get_name(&self) -> GString {
        "Joky".into()
    }
    fn init_ext(&mut self) {}
    fn get_type(&self) -> GString {
        "JokyScript".into()
    }
    fn get_extension(&self) -> GString {
        "jk".into()
    }
    fn finish(&mut self) {}
    fn get_reserved_words(&self) -> PackedStringArray {
        [
            "fn", "struct", "enum", "let", "var", "if", "else", "while", "match", "return",
            "import", "impl", "true", "false",
        ]
        .into_iter()
        .map(GString::from)
        .collect()
    }
    fn is_control_flow_keyword(&self, _keyword: GString) -> bool {
        matches!(
            _keyword.to_string().as_str(),
            "if" | "else" | "while" | "match" | "return" | "break" | "continue"
        )
    }
    fn get_comment_delimiters(&self) -> PackedStringArray {
        ["//"].into_iter().map(GString::from).collect()
    }
    fn get_string_delimiters(&self) -> PackedStringArray {
        ["\" \"", "' '"].into_iter().map(GString::from).collect()
    }
    fn make_template(
        &self,
        _template: GString,
        _class_name: GString,
        _base_class_name: GString,
    ) -> Option<Gd<godot::classes::Script>> {
        if _base_class_name != "Node" {
            return None;
        }
        let mut script = JokyScript::new_gd();
        {
            let mut data = script.bind_mut();
            data.source = TEMPLATE.into();
            data.compile();
        }
        Some(script.upcast())
    }
    fn get_built_in_templates(&self, _object: StringName) -> Array<AnyDictionary> {
        Default::default()
    }
    fn is_using_templates(&mut self) -> bool {
        false
    }
    fn validate(
        &self,
        _script: GString,
        _path: GString,
        _validate_functions: bool,
        _validate_errors: bool,
        _validate_warnings: bool,
        _validate_safe_lines: bool,
    ) -> AnyDictionary {
        let mut result = VarDictionary::new();
        let mut errors = Array::<AnyDictionary>::new();
        let mut functions = PackedStringArray::new();
        match Program::compile(&_path.to_string(), &_script.to_string()) {
            Ok(program) => {
                result.set("valid", true);
                if _validate_functions {
                    for (name, method) in &program.methods {
                        functions.push(&GString::from(&format!("{name}:{}", method.line)));
                    }
                }
            }
            Err(error) => {
                result.set("valid", false);
                if _validate_errors {
                    let mut entry = VarDictionary::new();
                    entry.set("line", error.line);
                    entry.set("column", error.column);
                    entry.set("message", error.message.as_str());
                    errors.push(entry.upcast_any_dictionary());
                }
            }
        }
        result.set("errors", &errors);
        result.set("functions", &functions);
        result.set("warnings", &Array::<AnyDictionary>::new());
        result.set("safe_lines", &PackedInt32Array::new());
        result.upcast_any_dictionary()
    }
    fn validate_path(&self, _path: GString) -> GString {
        if _path.to_string().ends_with(".jk") {
            GString::default()
        } else {
            "Joky scripts use the .jk extension".into()
        }
    }
    fn supports_builtin_mode(&self) -> bool {
        false
    }
    fn supports_documentation(&self) -> bool {
        false
    }
    fn can_inherit_from_file(&self) -> bool {
        false
    }
    fn find_function(&self, _function: GString, _code: GString) -> i32 {
        Program::compile("<editor>", &_code.to_string())
            .ok()
            .and_then(|p| p.methods.get(&_function.to_string()).map(|m| m.line))
            .unwrap_or(-1)
    }
    fn make_function(
        &self,
        _class_name: GString,
        _function_name: GString,
        _function_args: PackedStringArray,
    ) -> GString {
        Default::default()
    }
    fn can_make_function(&self) -> bool {
        false
    }
    fn open_in_external_editor(
        &mut self,
        _script: Option<Gd<godot::classes::Script>>,
        _line: i32,
        _column: i32,
    ) -> godot::global::Error {
        Error::ERR_UNAVAILABLE
    }
    fn overrides_external_editor(&mut self) -> bool {
        false
    }
    fn complete_code(
        &self,
        _code: GString,
        _path: GString,
        _owner: Option<Gd<godot::classes::Object>>,
    ) -> AnyDictionary {
        let mut result = VarDictionary::new();
        result.set("result", Error::ERR_UNAVAILABLE);
        result.set("force", false);
        result.set("call_hint", "");
        result.set("options", &VarArray::new());
        result.upcast_any_dictionary()
    }
    fn lookup_code(
        &self,
        _code: GString,
        _symbol: GString,
        _path: GString,
        _owner: Option<Gd<godot::classes::Object>>,
    ) -> AnyDictionary {
        let mut result = VarDictionary::new();
        result.set("result", Error::ERR_UNAVAILABLE);
        result.set(
            "type",
            godot::classes::script_language_extension::LookupResultType::MAX,
        );
        result.upcast_any_dictionary()
    }
    fn auto_indent_code(&self, _code: GString, _from_line: i32, _to_line: i32) -> GString {
        _code
    }
    fn add_global_constant(&mut self, _name: StringName, _value: Variant) {}
    fn add_named_global_constant(&mut self, _name: StringName, _value: Variant) {}
    fn remove_named_global_constant(&mut self, _name: StringName) {}
    fn thread_enter(&mut self) {}
    fn thread_exit(&mut self) {}
    fn debug_get_error(&self) -> GString {
        self.last_diagnostic()
    }
    fn debug_get_stack_level_count(&self) -> i32 {
        0
    }
    fn debug_get_stack_level_line(&self, _level: i32) -> i32 {
        -1
    }
    fn debug_get_stack_level_function(&self, _level: i32) -> GString {
        Default::default()
    }
    fn debug_get_stack_level_source(&self, _level: i32) -> GString {
        Default::default()
    }
    fn debug_get_stack_level_locals(
        &mut self,
        _level: i32,
        _max_subitems: i32,
        _max_depth: i32,
    ) -> AnyDictionary {
        VarDictionary::new().upcast_any_dictionary()
    }
    fn debug_get_stack_level_members(
        &mut self,
        _level: i32,
        _max_subitems: i32,
        _max_depth: i32,
    ) -> AnyDictionary {
        VarDictionary::new().upcast_any_dictionary()
    }
    unsafe fn debug_get_stack_level_instance_rawptr(
        &mut self,
        _level: i32,
    ) -> godot_core::meta::RawPtr<*mut c_void> {
        unsafe { godot_core::meta::RawPtr::null() }
    }
    fn debug_get_globals(&mut self, _max_subitems: i32, _max_depth: i32) -> AnyDictionary {
        VarDictionary::new().upcast_any_dictionary()
    }
    fn debug_parse_stack_level_expression(
        &mut self,
        _level: i32,
        _expression: GString,
        _max_subitems: i32,
        _max_depth: i32,
    ) -> GString {
        Default::default()
    }
    fn debug_get_current_stack_info(&mut self) -> Array<AnyDictionary> {
        Default::default()
    }
    fn reload_all_scripts(&mut self) {}
    fn reload_scripts(&mut self, _scripts: VarArray, _soft_reload: bool) {}
    fn reload_tool_script(
        &mut self,
        _script: Option<Gd<godot::classes::Script>>,
        _soft_reload: bool,
    ) {
    }
    fn get_recognized_extensions(&self) -> PackedStringArray {
        [GString::from("jk")].into_iter().collect()
    }
    fn get_public_functions(&self) -> Array<AnyDictionary> {
        Default::default()
    }
    fn get_public_constants(&self) -> AnyDictionary {
        VarDictionary::new().upcast_any_dictionary()
    }
    fn get_public_annotations(&self) -> Array<AnyDictionary> {
        Default::default()
    }
    fn profiling_start(&mut self) {}
    fn profiling_stop(&mut self) {}
    fn profiling_set_save_native_calls(&mut self, _enable: bool) {}
    unsafe fn profiling_get_accumulated_data_rawptr(
        &mut self,
        _info_array: godot_core::meta::RawPtr<*mut ScriptLanguageExtensionProfilingInfo>,
        _info_max: i32,
    ) -> i32 {
        0
    }
    unsafe fn profiling_get_frame_data_rawptr(
        &mut self,
        _info_array: godot_core::meta::RawPtr<*mut ScriptLanguageExtensionProfilingInfo>,
        _info_max: i32,
    ) -> i32 {
        0
    }
    fn frame(&mut self) {
        // Diagnostics can reenter language metadata/debug callbacks.
        let _base = self.base_mut();
        crate::drain_releases();
        crate::tasks::drive();
    }
    fn handles_global_class_type(&self, _type_: GString) -> bool {
        false
    }
    fn get_global_class_name(&self, _path: GString) -> AnyDictionary {
        VarDictionary::new().upcast_any_dictionary()
    }
    fn create_script(&self) -> Option<Gd<godot::classes::Object>> {
        Some(JokyScript::new_gd().upcast())
    }
}
