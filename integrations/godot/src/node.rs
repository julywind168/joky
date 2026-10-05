//! Main-thread, program-scoped Node capabilities. No engine pointer enters Joky.
use godot::{
    builtin::*,
    classes::{Node, Node2D},
    obj::InstanceId,
    prelude::*,
};
use joky::{
    runtime::string::{self, JokyText},
    vm::VmEngine,
    FunctionSignature, Type,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
    sync::atomic::{AtomicI32, Ordering},
};

pub const SOURCE: &str = include_str!("../stdlib/node.jk");
#[derive(Default)]
pub struct Bridge {
    handles: RefCell<HashMap<i32, InstanceId>>,
}
#[derive(Clone)]
struct Context {
    bridge: Rc<Bridge>,
    owner: InstanceId,
}
thread_local! {
    static CONTEXTS: RefCell<Vec<Context>> = const { RefCell::new(Vec::new()) };
    static TEXT: RefCell<(String, JokyText)> = RefCell::new((String::new(), JokyText::default()));
}
pub struct Scope;
impl Scope {
    pub fn enter(bridge: Rc<Bridge>, owner: InstanceId) -> Self {
        CONTEXTS.with(|contexts| contexts.borrow_mut().push(Context { bridge, owner }));
        Self
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        CONTEXTS.with(|contexts| {
            contexts.borrow_mut().pop();
        });
    }
}
fn context() -> Option<Context> {
    CONTEXTS.with(|contexts| contexts.borrow().last().cloned())
}
fn checked<T: Default>(f: impl FnOnce(Context) -> T) -> T {
    // No Rust panic may unwind through generated language code.
    catch_unwind(AssertUnwindSafe(|| context().map_or_else(T::default, f))).unwrap_or_default()
}
impl Bridge {
    fn issue(&self, node: Gd<Node>) -> i32 {
        let id = node.instance_id();
        let mut handles = self.handles.borrow_mut();
        handles.retain(|_, id| {
            Gd::<Node>::try_from_instance_id(*id).is_ok_and(|n| !n.is_queued_for_deletion())
        });
        if let Some((&token, _)) = handles.iter().find(|(_, existing)| **existing == id) {
            return token;
        }
        if handles.len() >= 4096 || node.is_queued_for_deletion() {
            return 0;
        }
        static NEXT: AtomicI32 = AtomicI32::new(1);
        let Ok(token) =
            NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        else {
            return 0;
        };
        handles.insert(token, id);
        token
    }
    fn get(&self, token: i32) -> Option<Gd<Node>> {
        let id = *self.handles.borrow().get(&token)?;
        let node = Gd::<Node>::try_from_instance_id(id).ok()?;
        (!node.is_queued_for_deletion()).then_some(node)
    }
}
extern "C" fn host_self() -> i32 {
    checked(|c| Gd::<Node>::try_from_instance_id(c.owner).map_or(0, |n| c.bridge.issue(n)))
}
extern "C" fn host_valid(token: i32) -> i32 {
    checked(|c| i32::from(c.bridge.get(token).is_some()))
}
extern "C" fn host_kind(token: i32) -> i32 {
    checked(|c| i32::from(c.bridge.get(token).is_some_and(|n| n.is_class("Node2D"))))
}
unsafe extern "C" fn host_child(token: i32, path: *const u8) -> i32 {
    checked(|c| {
        let Some(node) = c.bridge.get(token) else {
            return 0;
        };
        let Ok(path) = string::borrow(path) else {
            return 0;
        };
        if path.is_empty() || path.contains('\0') {
            return 0;
        }
        node.get_node_or_null(&NodePath::from(path))
            .map_or(0, |n| c.bridge.issue(n))
    })
}
extern "C" fn host_name(token: i32) -> *const u8 {
    checked(|c| {
        let text = c
            .bridge
            .get(token)
            .map_or_else(String::new, |n| n.get_name().to_string());
        TEXT.with(|slot| {
            let mut slot = slot.borrow_mut();
            slot.0 = text;
            slot.1 = JokyText::new(&slot.0);
            slot.1.as_ptr()
        })
    })
}
unsafe extern "C" fn host_rename(token: i32, name: *const u8) -> i32 {
    checked(|c| {
        let Some(mut node) = c.bridge.get(token) else {
            return 0;
        };
        let Ok(name) = string::borrow(name) else {
            return 0;
        };
        if name.is_empty() || name.contains(['\0', '/', ':', '@', '"', '%', '.']) {
            return 0;
        }
        node.set_name(name);
        1
    })
}
extern "C" fn host_free(token: i32) -> i32 {
    checked(|c| {
        let Some(mut node) = c.bridge.get(token) else {
            return 0;
        };
        node.queue_free();
        1
    })
}
extern "C" fn host_x(token: i32) -> f64 {
    checked(|c| {
        c.bridge
            .get(token)
            .and_then(|n| n.try_cast::<Node2D>().ok())
            .map_or(0.0, |n| f64::from(n.get_position().x))
    })
}
extern "C" fn host_y(token: i32) -> f64 {
    checked(|c| {
        c.bridge
            .get(token)
            .and_then(|n| n.try_cast::<Node2D>().ok())
            .map_or(0.0, |n| f64::from(n.get_position().y))
    })
}
fn vector(x: f64, y: f64) -> Option<Vector2> {
    let value = Vector2::new(x as real, y as real);
    (x.is_finite() && y.is_finite() && value.x.is_finite() && value.y.is_finite()).then_some(value)
}
extern "C" fn host_position(token: i32, x: f64, y: f64) -> i32 {
    checked(|c| {
        let Some(mut node) = c
            .bridge
            .get(token)
            .and_then(|n| n.try_cast::<Node2D>().ok())
        else {
            return 0;
        };
        let Some(value) = vector(x, y) else {
            return 0;
        };
        node.set_position(value);
        1
    })
}
pub fn register(vm: &mut VmEngine) -> Result<(), joky::Error> {
    let bindings: &[(&str, Vec<Type>, Type, *const u8)] = &[
        ("host_self", vec![], Type::Int32, host_self as *const u8),
        (
            "host_valid",
            vec![Type::Int32],
            Type::Int32,
            host_valid as *const u8,
        ),
        (
            "host_kind",
            vec![Type::Int32],
            Type::Int32,
            host_kind as *const u8,
        ),
        (
            "host_child",
            vec![Type::Int32, Type::reference(Type::String, false)],
            Type::Int32,
            host_child as *const u8,
        ),
        (
            "host_name",
            vec![Type::Int32],
            Type::String,
            host_name as *const u8,
        ),
        (
            "host_rename",
            vec![Type::Int32, Type::reference(Type::String, false)],
            Type::Int32,
            host_rename as *const u8,
        ),
        (
            "host_free",
            vec![Type::Int32],
            Type::Int32,
            host_free as *const u8,
        ),
        (
            "host_x",
            vec![Type::Int32],
            Type::Float64,
            host_x as *const u8,
        ),
        (
            "host_y",
            vec![Type::Int32],
            Type::Float64,
            host_y as *const u8,
        ),
        (
            "host_position",
            vec![Type::Int32, Type::Float64, Type::Float64],
            Type::Int32,
            host_position as *const u8,
        ),
    ];
    for (name, params, ret, entry) in bindings {
        let name = format!("godot_node_{name}");
        let signature = FunctionSignature::new(params.clone(), ret.clone());
        // Exact C signatures; code is part of this extension. Text views are
        // borrowed only inside callbacks, and the TLS result lives until copied.
        unsafe {
            if signature.ret == Type::String
                || signature.params.iter().any(|t| {
                    *t == Type::String
                        || matches!(t, Type::Reference(r) if r.target == Type::String)
                })
            {
                vm.register_text_fn(&name, signature, *entry)?;
            } else {
                vm.register_fn(&name, signature, *entry)?;
            }
        }
    }
    Ok(())
}
