//! Godot owns the script-instance wrapper; each wrapper owns one Joky root.
use crate::{runtime::Program, script::JokyScript, value};
use godot::{
    builtin::*,
    classes::{Object, Script, ScriptLanguage},
    meta::{error::CallErrorType, ClassId},
    obj::script::{ScriptInstance, SiMut},
    prelude::*,
    register::info::{MethodFlags, MethodInfo, PropertyHintInfo, PropertyInfo, PropertyUsageFlags},
};
use joky::vm::InstanceId;
use std::{cell::Cell, rc::Rc};

pub struct Session {
    pub program: Rc<Program>,
    id: Cell<Option<InstanceId>>,
    owner: godot::obj::InstanceId,
    failed: Cell<bool>,
}
impl Session {
    pub fn new(
        program: Rc<Program>,
        owner: godot::obj::InstanceId,
    ) -> Result<Rc<Self>, joky::Error> {
        let _scope = crate::node::Scope::enter(program.bridge.clone(), owner);
        let id = program.vm.create_instance("create", &[])?;
        let result = Rc::new(Self {
            program,
            owner,
            failed: Cell::new(false),
            id: Cell::new(Some(id)),
        });
        crate::register_session(&result);
        for property in result.program.properties.values() {
            result
                .program
                .vm
                .call_instance(id, &property.setter, &[property.default])?;
        }
        Ok(result)
    }
    pub fn close(&self) {
        if let Some(id) = self.id.take() {
            let _scope = crate::node::Scope::enter(self.program.bridge.clone(), self.owner);
            if let Err(error) = self.program.vm.close_instance(id) {
                crate::report(self.program.diagnostic(&error));
            }
            crate::LIVE.with(|n| n.set(n.get() - 1));
        }
    }
    pub fn call(&self, method: &str, args: &[&Variant]) -> Result<Variant, CallErrorType> {
        let frame = matches!(method, "_process" | "_physics_process");
        let owner = Gd::<godot::classes::Node>::try_from_instance_id(self.owner)
            .map_err(|_| CallErrorType::InstanceIsNull)?;
        if frame && (owner.is_queued_for_deletion() || self.failed.get()) {
            return Ok(Variant::nil());
        }
        if self.failed.get() {
            return Err(CallErrorType::InvalidMethod);
        }
        let signature = &self
            .program
            .methods
            .get(method)
            .ok_or(CallErrorType::InvalidMethod)?
            .signature;
        let expected = signature.params.len() - 1;
        if args.len() < expected {
            return Err(CallErrorType::TooFewArguments);
        }
        if args.len() > expected {
            return Err(CallErrorType::TooManyArguments);
        }
        let args = args
            .iter()
            .zip(&signature.params[1..])
            .map(|(v, t)| value::input(v, t))
            .collect::<Result<Vec<_>, _>>()?;
        let id = self.id.get().ok_or(CallErrorType::InstanceIsNull)?;
        let _scope = crate::node::Scope::enter(self.program.bridge.clone(), self.owner);
        let result = self
            .program
            .vm
            .call_instance(id, method, &args)
            .map_err(|error| {
                if self.program.vm.instance_status(id).ok()
                    == Some(joky::vm::InstanceStatus::Failed)
                {
                    self.failed.set(true);
                }
                crate::report(self.program.diagnostic(&error));
                CallErrorType::InvalidMethod
            })?;
        value::output(result).inspect_err(|_| {
            crate::report(crate::runtime::Diagnostic {
                path: self.program.path.clone(),
                line: self.program.methods[method].line,
                column: 1,
                message: "Result is outside the Godot Variant range".into(),
            })
        })
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn method_info(name: &str, method: &crate::runtime::Method) -> MethodInfo {
    let property = |ty: &joky::Type, name: &str| PropertyInfo {
        variant_type: value::variant_type(ty),
        class_name: StringName::default(),
        property_name: name.into(),
        hint_info: PropertyHintInfo::none(),
        usage: PropertyUsageFlags::DEFAULT,
    };
    MethodInfo {
        id: 0,
        method_name: name.into(),
        class_name: ClassId::none(),
        return_type: property(&method.signature.ret, ""),
        arguments: method.signature.params[1..]
            .iter()
            .zip(&method.parameters)
            .map(|(ty, name)| property(ty, name))
            .collect(),
        default_arguments: vec![],
        flags: MethodFlags::NORMAL,
    }
}
pub fn method_dictionary(name: &str, method: &crate::runtime::Method) -> AnyDictionary {
    let property = |ty: &joky::Type, name: &str| {
        let mut d = VarDictionary::new();
        d.set("name", name);
        d.set("type", value::variant_type(ty).ord());
        d.set("usage", PropertyUsageFlags::DEFAULT);
        d.upcast_any_dictionary()
    };
    let mut d = VarDictionary::new();
    d.set("name", name);
    d.set("flags", MethodFlags::NORMAL);
    let args: Array<AnyDictionary> = method.signature.params[1..]
        .iter()
        .zip(&method.parameters)
        .map(|(ty, name)| property(ty, name))
        .collect();
    d.set("args", &args);
    d.set("default_args", &VarArray::new());
    d.set("return", property(&method.signature.ret, ""));
    d.upcast_any_dictionary()
}

pub struct JokyInstance {
    pub script: Gd<Script>,
    pub session: Rc<Session>,
    pub owner: godot::obj::InstanceId,
}
impl ScriptInstance for JokyInstance {
    type Base = Object;
    fn class_name(&self) -> GString {
        "JokyScript".into()
    }
    fn set_property(mut this: SiMut<Self>, name: StringName, value: &Variant) -> bool {
        if !crate::is_main_thread() {
            return false;
        }
        let session = this.session.clone();
        let Some(property) = session.program.properties.get(&name.to_string()) else {
            return false;
        };
        let setter = property.setter.clone();
        let _base = this.base_mut();
        session.call(&setter, &[value]).is_ok()
    }
    fn get_property(&self, name: StringName) -> Option<Variant> {
        if !crate::is_main_thread() {
            return None;
        }
        let property = self.session.program.properties.get(&name.to_string())?;
        self.session.call(&property.getter, &[]).ok()
    }
    fn get_property_list(&self) -> Vec<PropertyInfo> {
        self.session
            .program
            .properties
            .iter()
            .map(|(name, p)| property_info(name, value::variant_type(&p.ty)))
            .collect()
    }
    fn get_method_list(&self) -> Vec<MethodInfo> {
        self.session
            .program
            .methods
            .iter()
            .map(|(n, m)| method_info(n, m))
            .collect()
    }
    fn call(
        mut this: SiMut<Self>,
        method: StringName,
        args: &[&Variant],
    ) -> Result<Variant, CallErrorType> {
        if !crate::is_main_thread() {
            return Err(CallErrorType::InvalidMethod);
        }
        let session = this.session.clone();
        let _base = this.base_mut();
        session.call(&method.to_string(), args)
    }
    fn is_placeholder(&self) -> bool {
        false
    }
    fn has_method(&self, method: StringName) -> bool {
        self.session
            .program
            .methods
            .contains_key(&method.to_string())
    }
    fn get_script(&self) -> &Gd<Script> {
        &self.script
    }
    fn get_property_type(&self, name: StringName) -> VariantType {
        self.session
            .program
            .properties
            .get(&name.to_string())
            .map_or(VariantType::NIL, |p| value::variant_type(&p.ty))
    }
    fn to_string(&self) -> GString {
        "Joky script instance".into()
    }
    fn get_property_state(&self) -> Vec<(StringName, Variant)> {
        self.session
            .program
            .properties
            .keys()
            .filter_map(|name| {
                self.get_property(name.as_str().into())
                    .map(|v| (name.as_str().into(), v))
            })
            .collect()
    }
    fn get_language(&self) -> Gd<ScriptLanguage> {
        self.script.clone().cast::<JokyScript>().bind().language()
    }
    fn on_refcount_decremented(&self) -> bool {
        true
    }
    fn on_refcount_incremented(&self) {}
    fn property_get_fallback(&self, _name: StringName) -> Option<Variant> {
        None
    }
    fn property_set_fallback(_this: SiMut<Self>, _name: StringName, _value: &Variant) -> bool {
        false
    }
    fn get_method_argument_count(&self, method: StringName) -> Option<u32> {
        self.session
            .program
            .methods
            .get(&method.to_string())
            .map(|m| (m.signature.params.len() - 1) as u32)
    }
    fn get_owner(&self) -> Option<Gd<Object>> {
        Gd::try_from_instance_id(self.owner).ok()
    }
}

pub fn property_info(name: &str, variant_type: VariantType) -> PropertyInfo {
    PropertyInfo {
        variant_type,
        class_name: StringName::default(),
        property_name: name.into(),
        hint_info: PropertyHintInfo::none(),
        usage: PropertyUsageFlags::DEFAULT,
    }
}
pub fn property_dictionary(name: &str, variant_type: VariantType) -> AnyDictionary {
    let mut d = VarDictionary::new();
    d.set("name", name);
    d.set("type", variant_type.ord());
    d.set("usage", PropertyUsageFlags::DEFAULT);
    d.set("class_name", "");
    d.set("hint", 0);
    d.set("hint_string", "");
    d.upcast_any_dictionary()
}
