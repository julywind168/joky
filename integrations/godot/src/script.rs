//! Script resources hold source and compiled snapshots, never node state.
use crate::{
    instance::{method_dictionary, property_dictionary, JokyInstance, Session},
    runtime::Program,
};
use godot::sys::GodotFfi;
use godot::{
    builtin::*,
    classes::{ClassDb, Engine, IScriptExtension, Object, Script, ScriptExtension, ScriptLanguage},
    global::Error,
    obj::script::create_script_instance,
    prelude::*,
};
use godot_core::meta::RawPtr;
use std::{ffi::c_void, rc::Rc, sync::Mutex};

#[derive(Clone)]
struct PropertyMetadata {
    name: String,
    ty: VariantType,
    default: joky::vm::InstanceValue,
    line: i32,
}

#[derive(GodotClass)]
#[class(init, base=ScriptExtension, tool)]
pub struct JokyScript {
    base: Base<ScriptExtension>,
    pub source: GString,
    program_id: u64,
    pub path: String,
    properties: Vec<PropertyMetadata>,
    placeholders: Mutex<Vec<usize>>,
}
impl Drop for JokyScript {
    fn drop(&mut self) {
        crate::release_program(self.program_id);
    }
}
impl JokyScript {
    fn update_placeholder(&self, pointer: usize) {
        let properties = self.get_script_property_list();
        let mut values = VarDictionary::new();
        for property in &self.properties {
            if let Ok(value) = crate::value::output(property.default) {
                values.set(property.name.as_str(), &value);
            }
        }
        // Only engine-created placeholders are registered, and erased callbacks
        // remove them before their storage dies. Godot consumes metadata synchronously.
        unsafe {
            godot::sys::interface_fn!(placeholder_script_instance_update)(
                pointer as _,
                properties.sys(),
                values.sys(),
            );
        }
    }
    fn refresh_exports(&mut self) {
        if !crate::is_main_thread() {
            return;
        }
        let pointers = self
            .placeholders
            .lock()
            .expect("placeholder registry")
            .clone();
        let properties = self.get_script_property_list();
        let mut values = VarDictionary::new();
        for property in &self.properties {
            if let Ok(value) = crate::value::output(property.default) {
                values.set(property.name.as_str(), &value);
            }
        }
        // Updating an engine placeholder reads constants back from this Script.
        // Release the mutable Rust binding while Godot makes those callbacks.
        let _base = self.base_mut();
        for pointer in pointers {
            unsafe {
                godot::sys::interface_fn!(placeholder_script_instance_update)(
                    pointer as _,
                    properties.sys(),
                    values.sys(),
                );
            }
        }
    }
    fn program(&self) -> Option<Rc<Program>> {
        crate::program(self.program_id)
    }

    pub fn language(&self) -> Gd<ScriptLanguage> {
        crate::language()
    }
    pub fn compile(&mut self) -> Error {
        if !crate::is_main_thread() {
            return Error::ERR_UNAVAILABLE;
        }
        let resource_path = self.base().get_path().to_string();
        let path = if resource_path.is_empty() {
            self.path.clone()
        } else {
            resource_path
        };
        match Program::compile(&path, &self.source.to_string()) {
            Ok(program) => {
                self.properties = program
                    .properties
                    .iter()
                    .map(|(name, property)| PropertyMetadata {
                        name: name.clone(),
                        ty: crate::value::variant_type(&property.ty),
                        default: property.default,
                        line: property.line,
                    })
                    .collect();
                crate::release_program(self.program_id);
                self.program_id = crate::publish_program(program);
                self.refresh_exports();
                Error::OK
            }
            Err(error) => {
                crate::release_program(self.program_id);
                self.program_id = 0;
                self.properties.clear();
                self.refresh_exports();
                crate::report(error);
                Error::ERR_PARSE_ERROR
            }
        }
    }
    unsafe fn instantiate(&self, object: Gd<Object>, placeholder: bool) -> RawPtr<*mut c_void> {
        if !crate::is_main_thread() || !object.is_class("Node") {
            return unsafe { RawPtr::null() };
        }
        let native = ClassDb::singleton()
            .class_get_property_list(&StringName::from(object.get_class().to_string().as_str()));
        if let Some(property) = self.properties.iter().find(|p| {
            native.iter_shared().any(|d| {
                d.get("name")
                    .is_some_and(|n| n.try_to::<GString>().is_ok_and(|n| n.to_string() == p.name))
            })
        }) {
            crate::report(crate::runtime::Diagnostic {
                path: self.path.clone(),
                line: property.line,
                column: 1,
                message: format!(
                    "Exported property `{}` conflicts with an engine property",
                    property.name
                ),
            });
            return unsafe { RawPtr::null() };
        }
        if placeholder {
            let language = self.language();
            let script = self.to_gd();
            // This callback requires an actual Godot PlaceHolderScriptInstance,
            // not a ScriptInstanceExtension reporting is_placeholder=true.
            // All three Godot objects remain alive while ownership is transferred.
            let pointer = unsafe {
                godot::sys::interface_fn!(placeholder_script_instance_create)(
                    language.obj_sys(),
                    script.obj_sys(),
                    object.obj_sys(),
                )
            };
            self.placeholders
                .lock()
                .expect("placeholder registry")
                .push(pointer as usize);
            self.update_placeholder(pointer as usize);
            return unsafe { RawPtr::new(pointer.cast()) };
        }
        let Some(program) = self.program() else {
            return unsafe { RawPtr::null() };
        };
        let owner = object.instance_id();
        let session = match Session::new(program.clone(), owner) {
            Ok(session) => session,
            Err(error) => {
                crate::report(program.diagnostic(&error));
                return unsafe { RawPtr::null() };
            }
        };
        let instance = JokyInstance {
            script: self.to_gd().upcast(),
            session,
            owner,
        };
        // Godot owns object throughout this callback and takes the returned wrapper immediately.
        unsafe { create_script_instance(instance, object) }
    }
}

#[godot_api]
impl IScriptExtension for JokyScript {
    fn editor_can_reload_from_file(&mut self) -> bool {
        true
    }
    fn can_instantiate(&self) -> bool {
        self.program_id != 0 && !Engine::singleton().is_editor_hint()
    }
    fn get_base_script(&self) -> Option<Gd<Script>> {
        None
    }
    fn get_global_name(&self) -> StringName {
        StringName::default()
    }
    fn inherits_script(&self, _script: Gd<Script>) -> bool {
        false
    }
    fn get_instance_base_type(&self) -> StringName {
        "Node".into()
    }
    unsafe fn instance_create_rawptr(&self, for_object: Gd<Object>) -> RawPtr<*mut c_void> {
        unsafe { self.instantiate(for_object, Engine::singleton().is_editor_hint()) }
    }
    unsafe fn placeholder_instance_create_rawptr(
        &self,
        for_object: Gd<Object>,
    ) -> RawPtr<*mut c_void> {
        unsafe { self.instantiate(for_object, true) }
    }
    fn has_source_code(&self) -> bool {
        true
    }
    fn get_source_code(&self) -> GString {
        self.source.clone()
    }
    fn set_source_code(&mut self, code: GString) {
        self.source = code;
    }
    fn reload(&mut self, _keep_state: bool) -> Error {
        self.compile()
    }
    fn get_doc_class_name(&self) -> StringName {
        StringName::default()
    }
    fn get_documentation(&self) -> Array<AnyDictionary> {
        Array::new()
    }
    fn has_method(&self, method: StringName) -> bool {
        self.program()
            .as_ref()
            .is_some_and(|p| p.methods.contains_key(&method.to_string()))
    }
    fn has_static_method(&self, _method: StringName) -> bool {
        false
    }
    fn get_script_method_argument_count(&self, method: StringName) -> Variant {
        self.program()
            .as_ref()
            .and_then(|p| p.methods.get(&method.to_string()))
            .map_or_else(Variant::nil, |m| {
                ((m.signature.params.len() - 1) as i64).to_variant()
            })
    }
    fn get_method_info(&self, method: StringName) -> AnyDictionary {
        self.program()
            .as_ref()
            .and_then(|p| p.methods.get(&method.to_string()))
            .map_or_else(
                || VarDictionary::new().upcast_any_dictionary(),
                |m| method_dictionary(&method.to_string(), m),
            )
    }
    fn is_tool(&self) -> bool {
        false
    }
    fn is_valid(&self) -> bool {
        self.program_id != 0
    }
    fn get_language(&self) -> Option<Gd<ScriptLanguage>> {
        Some(self.language())
    }
    fn has_script_signal(&self, _signal: StringName) -> bool {
        false
    }
    fn get_script_signal_list(&self) -> Array<AnyDictionary> {
        Array::new()
    }
    fn has_property_default_value(&self, property: StringName) -> bool {
        self.properties
            .iter()
            .any(|p| p.name == property.to_string())
    }
    fn get_property_default_value(&self, property: StringName) -> Variant {
        self.properties
            .iter()
            .find(|p| p.name == property.to_string())
            .and_then(|p| crate::value::output(p.default).ok())
            .unwrap_or_else(Variant::nil)
    }
    fn update_exports(&mut self) {
        self.refresh_exports();
    }
    unsafe fn placeholder_erased_rawptr(&mut self, placeholder: RawPtr<*mut c_void>) {
        self.placeholders
            .lock()
            .expect("placeholder registry")
            .retain(|p| *p != placeholder.ptr() as usize);
    }
    fn get_script_method_list(&self) -> Array<AnyDictionary> {
        self.program().as_ref().map_or_else(Array::new, |p| {
            p.methods
                .iter()
                .map(|(n, m)| method_dictionary(n, m))
                .collect()
        })
    }
    fn get_script_property_list(&self) -> Array<AnyDictionary> {
        self.properties
            .iter()
            .map(|p| property_dictionary(&p.name, p.ty))
            .collect()
    }
    fn get_member_line(&self, member: StringName) -> i32 {
        self.properties
            .iter()
            .find(|p| p.name == member.to_string())
            .map_or_else(
                || {
                    self.program()
                        .as_ref()
                        .and_then(|p| p.methods.get(&member.to_string()))
                        .map_or(-1, |m| m.line)
                },
                |p| p.line,
            )
    }
    fn get_constants(&self) -> AnyDictionary {
        VarDictionary::new().upcast_any_dictionary()
    }
    fn get_members(&self) -> Array<StringName> {
        self.properties
            .iter()
            .map(|p| StringName::from(p.name.as_str()))
            .collect()
    }
    fn is_placeholder_fallback_enabled(&self) -> bool {
        false
    }
    fn get_rpc_config(&self) -> Variant {
        Variant::nil()
    }
    fn instance_has(&self, object: Gd<Object>) -> bool {
        object
            .get_script()
            .is_some_and(|script| script.instance_id() == self.to_gd().instance_id())
    }
}
