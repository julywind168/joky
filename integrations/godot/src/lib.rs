//! Godot 4.7 scripting adapter. Joky scripts execute only in the interpreter.
mod instance;
mod language;
mod node;
mod property;
mod resource;
mod runtime;
mod script;
mod tasks;
mod value;

use godot::{
    classes::{Engine, ResourceLoader, ResourceSaver, ScriptLanguage},
    init::InitStage,
    prelude::*,
};
use instance::Session;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::{Rc, Weak},
    sync::{Mutex, OnceLock},
    thread::ThreadId,
};

struct Registration {
    language: Gd<language::JokyLanguage>,
    loader: Gd<resource::JokyLoader>,
    saver: Gd<resource::JokySaver>,
    sessions: Vec<Weak<Session>>,
}
thread_local! {
    static PROGRAMS: RefCell<HashMap<u64, Rc<runtime::Program>>> = RefCell::new(HashMap::new());
    static NEXT_PROGRAM: Cell<u64> = const { Cell::new(1) };
    static REGISTRATION: RefCell<Option<Registration>> = const { RefCell::new(None) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

// Godot scans resource metadata on worker threads. VM snapshots never leave
// main-thread TLS; resources carry only a key, with deferred off-thread release.
static LANGUAGE_ID: Mutex<Option<godot::obj::InstanceId>> = Mutex::new(None);
// System resolution cannot be interrupted. Retain one OS loader reference for
// this process so Godot teardown cannot unmap code still executing on a worker.
// This adapter already disallows extension reload; script reload remains supported.
static LIBRARY: OnceLock<libloading::Library> = OnceLock::new();
fn retain_library() {
    use godot::sys::GodotFfi;
    LIBRARY.get_or_init(|| {
        // Scene initialization runs on the initialized Godot main thread. The
        // engine initializes the returned String and identifies this very library.
        let path = unsafe {
            GString::new_with_uninit(|path| {
                godot::sys::interface_fn!(get_library_path)(godot::sys::get_library(), path.cast())
            })
        };
        let path = godot::classes::ProjectSettings::singleton().globalize_path(&path);
        // The library is already loaded and initialized; this acquires another
        // loader reference. A static never drops it, even after engine deinit.
        unsafe { libloading::Library::new(path.to_string()) }
            .expect("retain Joky extension for DNS worker lifetime")
    });
}
static MAIN_THREAD: OnceLock<ThreadId> = OnceLock::new();
static RELEASES: Mutex<Vec<u64>> = Mutex::new(Vec::new());
fn is_main_thread() -> bool {
    MAIN_THREAD.get() == Some(&std::thread::current().id())
}
fn publish_program(program: runtime::Program) -> u64 {
    assert!(is_main_thread());
    drain_releases();
    let id = NEXT_PROGRAM.with(|next| {
        let id = next.get();
        next.set(id.checked_add(1).expect("program identity exhausted"));
        id
    });
    PROGRAMS.with(|programs| programs.borrow_mut().insert(id, Rc::new(program)));
    id
}
fn program(id: u64) -> Option<Rc<runtime::Program>> {
    if !is_main_thread() {
        return None;
    }
    PROGRAMS.with(|programs| programs.borrow().get(&id).cloned())
}
fn release_program(id: u64) {
    if id == 0 {
        return;
    }
    if is_main_thread() {
        PROGRAMS.with(|programs| programs.borrow_mut().remove(&id));
    } else {
        RELEASES.lock().expect("release queue").push(id);
    }
}
fn drain_releases() {
    assert!(is_main_thread());
    let ids = std::mem::take(&mut *RELEASES.lock().expect("release queue"));
    PROGRAMS.with(|programs| {
        let mut programs = programs.borrow_mut();
        for id in ids {
            programs.remove(&id);
        }
    });
}

fn language() -> Gd<ScriptLanguage> {
    let id = LANGUAGE_ID
        .lock()
        .expect("language identity")
        .expect("registered language");
    Gd::from_instance_id(id)
}
fn register_session(session: &Rc<Session>) {
    REGISTRATION.with(|r| {
        let mut r = r.borrow_mut();
        let sessions = &mut r.as_mut().expect("registered adapter").sessions;
        sessions.retain(|s| s.strong_count() != 0);
        sessions.push(Rc::downgrade(session));
    });
    LIVE.with(|n| n.set(n.get() + 1));
}
fn report(diagnostic: runtime::Diagnostic) {
    let text = diagnostic.to_string();
    LAST_ERROR.with(|e| *e.borrow_mut() = text.clone());
    godot_error!("{text}");
}
struct JokyExtension;
// Safety: execution and persistent snapshots stay on Godot's main thread.
// Metadata callbacks may run on workers but never access VM snapshots. Godot
// owns script wrappers; Scene teardown closes remaining roots before unregistration.
#[gdextension]
unsafe impl ExtensionLibrary for JokyExtension {
    fn on_stage_init(stage: InitStage) {
        if stage != InitStage::Scene {
            return;
        }
        retain_library();
        MAIN_THREAD
            .set(std::thread::current().id())
            .expect("initialize once");
        let language = language::JokyLanguage::new_alloc();
        *LANGUAGE_ID.lock().expect("language identity") = Some(language.instance_id());
        let loader = resource::JokyLoader::new_gd();
        let saver = resource::JokySaver::new_gd();
        REGISTRATION.with(|r| {
            *r.borrow_mut() = Some(Registration {
                language: language.clone(),
                loader: loader.clone(),
                saver: saver.clone(),
                sessions: vec![],
            })
        });
        let result = Engine::singleton().register_script_language(&language);
        assert_eq!(
            result,
            godot::global::Error::OK,
            "Joky language registration failed"
        );
        ResourceLoader::singleton().add_resource_format_loader(&loader);
        ResourceSaver::singleton().add_resource_format_saver(&saver);
    }
    fn on_stage_deinit(stage: InitStage) {
        if stage != InitStage::Scene {
            return;
        }
        let registration = REGISTRATION.with(|r| r.borrow_mut().take());
        if let Some(registration) = registration {
            for session in registration.sessions.iter().filter_map(Weak::upgrade) {
                session.close();
            }
            drain_releases();
            PROGRAMS.with(|programs| programs.borrow_mut().clear());
            // Cancellation closes language owners immediately. Running system
            // lookups may outlive this callback; retain_library keeps their code.
            joky::runtime::dns::shutdown();
            ResourceLoader::singleton().remove_resource_format_loader(&registration.loader);
            ResourceSaver::singleton().remove_resource_format_saver(&registration.saver);
            Engine::singleton().unregister_script_language(&registration.language);
            *LANGUAGE_ID.lock().expect("language identity") = None;
            registration.language.free();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_release_on_worker_keeps_vm_destruction_on_main_thread() {
        MAIN_THREAD.set(std::thread::current().id()).unwrap();
        let compiled = runtime::Program::compile("res://counter.jk", runtime::TEMPLATE)
            .unwrap_or_else(|e| panic!("{e}"));
        let id = publish_program(compiled);
        let weak = Rc::downgrade(&program(id).unwrap());
        std::thread::spawn(move || {
            assert!(program(id).is_none());
            release_program(id);
        })
        .join()
        .unwrap();
        assert!(weak.upgrade().is_some());
        drain_releases();
        assert!(program(id).is_none());
        assert!(weak.upgrade().is_none());
    }
}
