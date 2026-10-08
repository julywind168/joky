//! Plain-text .jk resource loading and saving; scene files retain resource paths.
use crate::script::JokyScript;
use godot::{
    builtin::*,
    classes::{
        file_access::ModeFlags, FileAccess, IResourceFormatLoader, IResourceFormatSaver, Resource,
        ResourceFormatLoader, ResourceFormatSaver,
    },
    global::Error,
    prelude::*,
};

#[derive(GodotClass)]
#[class(init, base=ResourceFormatLoader, tool)]
pub struct JokyLoader {
    engine_base: Base<ResourceFormatLoader>,
}
#[godot_api]
impl IResourceFormatLoader for JokyLoader {
    fn get_recognized_extensions(&self) -> PackedStringArray {
        [GString::from("jk")].into_iter().collect()
    }
    fn handles_type(&self, type_: StringName) -> bool {
        matches!(
            type_.to_string().as_str(),
            "Script" | "JokyScript" | "Resource"
        )
    }
    fn get_resource_type(&self, path: GString) -> GString {
        if path.to_string().ends_with(".jk") {
            "JokyScript".into()
        } else {
            GString::default()
        }
    }
    fn load(
        &self,
        path: GString,
        _original_path: GString,
        use_sub_threads: bool,
        _cache_mode: i32,
    ) -> Variant {
        if use_sub_threads || !crate::is_main_thread() {
            return Error::ERR_UNAVAILABLE.to_variant();
        }
        let Some(file) = FileAccess::open(&path, ModeFlags::READ) else {
            return Error::ERR_CANT_OPEN.to_variant();
        };
        let mut script = JokyScript::new_gd();
        {
            let mut data = script.bind_mut();
            data.path = path.to_string();
            data.source = file.get_as_text();
            data.compile();
        }
        // Invalid scripts remain editable resources with a diagnostic, but cannot instantiate.
        script.set_path(&path);
        script.to_variant()
    }
}
#[derive(GodotClass)]
#[class(init, base=ResourceFormatSaver, tool)]
pub struct JokySaver {
    engine_base: Base<ResourceFormatSaver>,
}
#[godot_api]
impl IResourceFormatSaver for JokySaver {
    fn recognize(&self, resource: Option<Gd<Resource>>) -> bool {
        resource.is_some_and(|r| r.is_class("JokyScript"))
    }
    fn get_recognized_extensions(&self, resource: Option<Gd<Resource>>) -> PackedStringArray {
        if self.recognize(resource) {
            [GString::from("jk")].into_iter().collect()
        } else {
            PackedStringArray::new()
        }
    }
    fn save(&mut self, resource: Option<Gd<Resource>>, path: GString, _flags: u32) -> Error {
        let Some(mut script) = resource.and_then(|r| r.try_cast::<JokyScript>().ok()) else {
            return Error::ERR_INVALID_PARAMETER;
        };
        let Some(mut file) = FileAccess::open(&path, ModeFlags::WRITE) else {
            return Error::ERR_CANT_OPEN;
        };
        if !file.store_string(&script.bind().source) {
            return Error::ERR_FILE_CANT_WRITE;
        }
        script.bind_mut().path = path.to_string();
        Error::OK
    }
}
