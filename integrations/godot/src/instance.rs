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
}
impl Session {
    pub fn new(program: Rc<Program>) -> Result<Rc<Self>, joky::Error> {
        let id = program.vm.create_instance("create", &[])?;
        let result = Rc::new(Self {
            program,
            id: Cell::new(Some(id)),
        });
        crate::register_session(&result);
        Ok(result)
    }
    pub fn close(&self) {
        if let Some(id) = self.id.take() {
            let _ = self.program.vm.close_instance(id);
            crate::LIVE.with(|n| n.set(n.get() - 1));
        }
    }
    pub fn call(&self, method: &str, args: &[&Variant]) -> Result<Variant, CallErrorType> {
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
        let result = self
            .program
            .vm
            .call_instance(id, method, &args)
            .map_err(|error| {
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
    fn set_property(_this: SiMut<Self>, _name: StringName, _value: &Variant) -> bool {
        false
    }
    fn get_property(&self, _name: StringName) -> Option<Variant> {
        None
    }
    fn get_property_list(&self) -> Vec<PropertyInfo> {
        vec![]
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
        this: SiMut<Self>,
        method: StringName,
        args: &[&Variant],
    ) -> Result<Variant, CallErrorType> {
        if !crate::is_main_thread() {
            return Err(CallErrorType::InvalidMethod);
        }
        this.session.call(&method.to_string(), args)
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
    fn get_property_type(&self, _name: StringName) -> VariantType {
        VariantType::NIL
    }
    fn to_string(&self) -> GString {
        "Joky script instance".into()
    }
    fn get_property_state(&self) -> Vec<(StringName, Variant)> {
        vec![]
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
