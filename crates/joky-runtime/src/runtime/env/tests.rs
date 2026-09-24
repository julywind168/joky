use super::*;
use crate::runtime::continuation::{
    jk_continuation_cancel, jk_continuation_free, jk_continuation_new,
};
use crate::runtime::scope::RuntimeScope;
use std::ffi::OsString;
use std::sync::atomic::Ordering;

fn snapshot() -> EnvSnapshot {
    EnvSnapshot {
        vars: vec![
            ("EMPTY".into(), "".into()),
            ("APP_MODE".into(), "开发".into()),
        ],
        args: vec!["first".into(), "".into(), "参数".into()],
    }
}

#[test]
fn lookup_distinguishes_missing_empty_invalid_and_unicode() {
    let snapshot = snapshot();
    assert_eq!(lookup(&snapshot, "MISSING"), Ok(None));
    assert_eq!(lookup(&snapshot, "EMPTY"), Ok(Some(String::new())));
    assert_eq!(lookup(&snapshot, "APP_MODE"), Ok(Some("开发".into())));
    for name in ["", "A=B", "A\0B"] {
        assert!(lookup(&snapshot, name).is_err());
    }
    #[cfg(unix)]
    assert_eq!(lookup(&snapshot, "app_mode"), Ok(None));
    #[cfg(windows)]
    assert_eq!(lookup(&snapshot, "app_mode"), Ok(Some("开发".into())));
}

#[test]
fn collections_preserve_order_values_and_release_owned_strings() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let snapshot = snapshot();
    let list = make_args(&snapshot).unwrap().0;
    assert_eq!(super::super::list::jk_list_length(list), 3);
    let mut node = jk_dup(list);
    for expected in &snapshot.args {
        let mut head = [0u64; 2];
        assert_eq!(
            super::super::list::jk_list_head(node, head.as_mut_ptr(), 2),
            1
        );
        let bytes = unsafe { std::slice::from_raw_parts(head[0] as *const u8, head[1] as usize) };
        assert_eq!(
            std::str::from_utf8(bytes).unwrap(),
            expected.to_str().unwrap()
        );
        jk_drop(head[0] as *mut u8);
        let next = super::super::list::jk_list_tail(node);
        jk_drop(node);
        node = next;
    }
    assert!(node.is_null());
    jk_drop(list);
    let map = make_vars(&snapshot).unwrap().0;
    let key = "APP_MODE";
    let key_pointer = make_string(key);
    let words = [key_pointer as u64, key.len() as u64];
    let mut output = [0u64; 2];
    assert_eq!(
        super::super::map::jk_map_get(map, words.as_ptr(), 2, 1, output.as_mut_ptr(), 2),
        1
    );
    let bytes = unsafe { std::slice::from_raw_parts(output[0] as *const u8, output[1] as usize) };
    assert_eq!(std::str::from_utf8(bytes).unwrap(), "开发");
    jk_drop(output[0] as *mut u8);
    jk_drop(key_pointer);
    jk_drop(map);
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
    let empty = EnvSnapshot {
        vars: vec![],
        args: vec![],
    };
    assert!(make_args(&empty).unwrap().0.is_null());
    assert!(make_vars(&empty).unwrap().0.is_null());
}

#[cfg(unix)]
#[test]
fn invalid_encoding_does_not_leak_partially_built_collections() {
    use std::os::unix::ffi::OsStringExt;
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let invalid = OsString::from_vec(vec![0xff]);
    let mut snapshot = snapshot();
    snapshot.args.insert(1, invalid.clone());
    snapshot.vars.push(("INVALID".into(), invalid.clone()));
    assert!(make_args(&snapshot).is_err());
    assert!(make_vars(&snapshot).is_err());
    assert!(lookup(&snapshot, "INVALID").is_err());
    assert_eq!(lookup(&snapshot, "EMPTY"), Ok(Some(String::new())));
    snapshot.vars.pop();
    snapshot.vars.push((invalid.clone(), "valid".into()));
    assert!(make_vars(&snapshot).is_err());
    assert!(path_string(PathBuf::from(invalid)).is_err());
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
}

#[test]
fn hooks_reject_bad_layouts_without_consuming_arguments() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let handle = jk_continuation_new(1);
    let name = make_string("APP_MODE");
    let arguments = [name as usize, 8usize];
    unsafe {
        for (_, hook) in hooks() {
            for size in [0, WORD, 3 * WORD, 7 * WORD, 100 * WORD] {
                assert_eq!(
                    hook(handle, 0, std::ptr::null(), 0, std::ptr::null_mut(), size),
                    0
                );
            }
        }
        assert_eq!(
            get_start(
                handle,
                0,
                arguments.as_ptr().cast(),
                WORD,
                std::ptr::null_mut(),
                6 * WORD
            ),
            0
        );
        let wrong_length = [name as usize, 999usize];
        assert_eq!(
            get_start(
                handle,
                0,
                wrong_length.as_ptr().cast(),
                2 * WORD,
                std::ptr::null_mut(),
                6 * WORD
            ),
            0
        );
        jk_continuation_free(handle);
    }
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 1);
    jk_drop(name);
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
}

#[test]
fn rejected_completion_releases_every_result_shape() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let handle = jk_continuation_new(2);
    let continuation = unsafe { Continuation::retain_registered(handle) }.unwrap();
    unsafe {
        assert_eq!(jk_continuation_cancel(handle), 1);
        jk_continuation_free(handle);
    }
    complete(&continuation, handle, 0, Ok(Some("value".to_owned())));
    complete::<Option<String>>(&continuation, handle, 0, Ok(None));
    complete::<Option<String>>(&continuation, handle, 0, Err("error".into()));
    complete(&continuation, handle, 0, Ok("directory".to_owned()));
    complete::<String>(&continuation, handle, 0, Err("error".into()));
    complete(&continuation, handle, 0, make_args(&snapshot()));
    complete(&continuation, handle, 0, make_vars(&snapshot()));
    complete::<Collection>(&continuation, handle, 0, Err("error".into()));
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
}
