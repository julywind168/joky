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
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};

pub struct Session {
    pub program: Rc<Program>,
    id: Cell<Option<InstanceId>>,
    pub(crate) owner: godot::obj::InstanceId,
    failed: Cell<bool>,
    ready: Cell<bool>,
    tasks_closed: Cell<bool>,
    pending: RefCell<Vec<(String, Vec<joky::vm::InstanceValue>)>>,
    tasks: RefCell<HashSet<joky::runtime::executor::TaskId>>,
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
            ready: Cell::new(false),
            tasks_closed: Cell::new(false),
            pending: RefCell::default(),
            tasks: RefCell::default(),
            id: Cell::new(Some(id)),
        });
        crate::register_session(&result);
        crate::tasks::register_session(id, &result);
        for property in result.program.properties.values() {
            result
                .program
                .vm
                .call_instance(id, &property.setter, &[property.default])?;
        }
        Ok(result)
    }
    pub fn close(&self) {
        self.pending.borrow_mut().clear();
        self.tasks_closed.set(true);
        if let Some(id) = self.id.take() {
            let _scope = crate::node::Scope::enter(self.program.bridge.clone(), self.owner);
            if let Err(error) = self.program.vm.close_instance(id) {
                crate::report(self.program.diagnostic(&error));
            }
            crate::tasks::unregister_session(id);
            crate::LIVE.with(|n| n.set(n.get() - 1));
        }
    }
    pub(crate) fn accepts_tasks(&self) -> bool {
        !self.tasks_closed.get() && !self.failed.get() && self.id.get().is_some()
    }
    pub(crate) fn start(
        &self,
        method: &str,
        args: &[joky::vm::InstanceValue],
    ) -> Result<(), joky::Error> {
        if self.tasks_closed.get()
            || self.failed.get()
            || !self.program.methods.get(method).is_some_and(|m| m.task)
        {
            return Err(joky::Error::new(
                joky::ErrorPhase::Runtime,
                "Task owner is closed or entry is not a task_ method",
            ));
        }
        let owner = Gd::<godot::classes::Node>::try_from_instance_id(self.owner).ok();
        if !owner.is_some_and(|n| n.is_inside_tree() && !n.is_queued_for_deletion()) {
            return Err(joky::Error::new(
                joky::ErrorPhase::Runtime,
                "Task owner is outside the tree or being deleted",
            ));
        }
        let id = self
            .id
            .get()
            .ok_or_else(|| joky::Error::new(joky::ErrorPhase::Runtime, "Instance is closed"))?;
        if self.program.methods[method].signature.params
            != args.iter().map(|v| v.value_type()).collect::<Vec<_>>()
        {
            return Err(joky::Error::new(
                joky::ErrorPhase::Runtime,
                "Task input signature mismatch",
            ));
        }
        if self.program.vm.instance_status(id)? == joky::vm::InstanceStatus::Busy {
            let mut pending = self.pending.borrow_mut();
            if pending.len() + self.tasks.borrow().len() >= 256 {
                return Err(joky::Error::new(
                    joky::ErrorPhase::Runtime,
                    "Pending task limit exceeded",
                ));
            }
            pending.push((method.into(), args.to_vec()));
            return Ok(());
        }
        let task = self.program.vm.spawn_instance_task_with_limits(
            id,
            method,
            args,
            joky::vm::InterpreterLimits {
                max_frames: 256,
                instruction_budget: None,
                poll_budget: Some(10_000),
                ..Default::default()
            },
        )?;
        self.tasks.borrow_mut().insert(task);
        Ok(())
    }
    pub(crate) fn stop_tasks(&self) {
        let _scope = crate::node::Scope::enter(self.program.bridge.clone(), self.owner);
        self.tasks_closed.set(true);
        self.pending.borrow_mut().clear();
        let tasks = std::mem::take(&mut *self.tasks.borrow_mut());
        for task in tasks {
            self.program.vm.cancel_instance_task(task);
        }
    }
    pub(crate) fn before_drive(&self) {
        if !Gd::<godot::classes::Node>::try_from_instance_id(self.owner)
            .is_ok_and(|n| !n.is_queued_for_deletion())
        {
            self.stop_tasks();
        }
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        for (method, args) in pending {
            if let Err(error) = self.start(&method, &args) {
                crate::report(self.program.diagnostic(&error));
                self.failed.set(true);
                self.stop_tasks();
                break;
            }
        }
    }
    pub(crate) fn completed(&self, id: joky::runtime::executor::TaskId) {
        self.tasks.borrow_mut().remove(&id);
    }
    pub(crate) fn after_drive(&self) {
        if self.id.get().is_some_and(|id| {
            self.program.vm.instance_status(id).ok() == Some(joky::vm::InstanceStatus::Failed)
        }) {
            self.failed.set(true);
            self.stop_tasks();
        }
    }
    pub fn call(&self, method: &str, args: &[&Variant]) -> Result<Variant, CallErrorType> {
        let frame = matches!(method, "_process" | "_physics_process");
        let owner = Gd::<godot::classes::Node>::try_from_instance_id(self.owner)
            .map_err(|_| CallErrorType::InstanceIsNull)?;
        if method == "_ready" && self.ready.replace(true) {
            return Ok(Variant::nil());
        }
        if frame && (owner.is_queued_for_deletion() || self.failed.get()) {
            return Ok(Variant::nil());
        }
        if self.failed.get() {
            return Err(CallErrorType::InvalidMethod);
        }
        let entry = self
            .program
            .methods
            .get(method)
            .ok_or(CallErrorType::InvalidMethod)?;
        let offset = usize::from(!entry.task);
        let signature = &entry.signature;
        let expected = signature.params.len() - offset;
        if args.len() < expected {
            return Err(CallErrorType::TooFewArguments);
        }
        if args.len() > expected {
            return Err(CallErrorType::TooManyArguments);
        }
        let args = args
            .iter()
            .zip(&signature.params[offset..])
            .map(|(v, t)| value::input(v, t))
            .collect::<Result<Vec<_>, _>>()?;
        if entry.task {
            self.start(method, &args).map_err(|error| {
                crate::report(self.program.diagnostic(&error));
                CallErrorType::InvalidMethod
            })?;
            return Ok(Variant::nil());
        }
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
        arguments: method.signature.params[usize::from(!method.task)..]
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
    let args: Array<AnyDictionary> = method.signature.params[usize::from(!method.task)..]
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
    fn on_notification(mut this: SiMut<Self>, what: i32, _reversed: bool) {
        if !crate::is_main_thread() {
            return;
        }
        let session = this.session.clone();
        let _base = this.base_mut();
        if godot::classes::notify::NodeNotification::from(what)
            == godot::classes::notify::NodeNotification::EXIT_TREE
        {
            session.stop_tasks();
        }
        if godot::classes::notify::ObjectNotification::from(what)
            == godot::classes::notify::ObjectNotification::PREDELETE
        {
            session.close();
        }
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
            .map(|m| (m.signature.params.len() - usize::from(!m.task)) as u32)
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
