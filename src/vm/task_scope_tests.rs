//! Sequential task scope and branch static checks. Execution lands with the
//! three-backend closure; until then every accepted program must be rejected
//! by the backend entry instead of falling into incomplete execution.
use super::support::{delayed_value, record, EVENTS, READY};
use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};

const NOT_EXECUTABLE: &str = "Sequential task scopes and branch are not executable yet";

fn externs() -> &'static str {
    "extern fn record(x: Int32) -> Int32
extern fn delayed_value(x: Int32) @suspends -> Int32
struct Counter { value: Int32 }
struct Pair { left: Int32 right: Int32 }
fn bump(counter: &mut Counter, id: Int32) @suspends {
    counter.value = counter.value + delayed_value(id)
    record(counter.value);
}
fn read(counter: &Counter, id: Int32) @suspends {
    record(counter.value)
    delayed_value(id);
}
fn send(text: String) @suspends { delayed_value(int32(text.byte_length())); }
fn keep(counter: &mut Counter) @suspends { delayed_value(counter.value); }
"
}

/// Build an engine with the recording host bindings and load `source`.
fn checked_vm(source: &str, mode: VmMode) -> Result<(), crate::Error> {
    READY.with(|ready| ready.set(false));
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_suspending_fn(
            "delayed_value",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            delayed_value,
        )
        .unwrap();
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source(source).map(|_| ())
}

/// A program whose static checks pass must be rejected before execution in
/// every backend, never falling into incomplete execution.
fn assert_not_executable(source: &str) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let error = checked_vm(source, mode).unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::InvalidState, "{error}");
        assert!(error.message().contains(NOT_EXECUTABLE), "{error}");
    }
    let error = crate::aot::AotCompiler::new("task-scope")
        .unwrap()
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_err();
    assert!(error.message().contains(NOT_EXECUTABLE), "{error}");
}

fn assert_rejected(source: &str, message: &str) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let error = checked_vm(source, mode).unwrap_err();
        assert!(
            error.message().contains(message),
            "{mode:?}: {error} (expected {message})"
        );
    }
}

#[test]
fn scope_bodies_with_branches_pass_checks_and_stay_unexecutable() {
    let source = format!(
        "{externs}
fn work(a: Int32, b: Int32) @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: a }})
        read(counter, 1);
        branch {{ bump(counter, b) }}
        branch {{}}
    }}
    record(42);
}}
fn main() @suspends {{ work(1, 2) }}
",
        externs = externs()
    );
    assert_not_executable(&source);
}

#[test]
fn captures_follow_copy_move_and_reference_rules() {
    let copy_snapshot = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        var count = 1
        branch {{ delayed_value(count); }}
        count = 2
        delayed_value(count);
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&copy_snapshot);

    let owner_move = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        let text = \"hello\"
        branch {{ send(text) }}
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&owner_move);

    let reference = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 1 }})
        branch {{ keep(counter) }}
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&reference);
}

#[test]
fn nested_scopes_and_branch_entries_own_their_children() {
    let source = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        let outer = alloc(Counter {{ value: 1 }})
        branch {{
            scope {{
                let inner = alloc(Counter {{ value: 2 }})
                branch {{ bump(inner, 3) }}
                bump(outer, 4);
            }}
        }}
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&source);
}

#[test]
fn loops_may_register_branches_each_iteration() {
    let source = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        var i = 0
        while i < 4 {{
            branch {{ delayed_value(i); }}
            i = i + 1
        }}
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&source);
}

#[test]
fn abrupt_exits_route_through_crossed_scope_joins() {
    let return_across = format!(
        "{externs}
fn main() @suspends -> String {{
    scope {{
        branch {{ delayed_value(1); }}
        return \"owned\"
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&return_across);

    let break_across = format!(
        "{externs}
fn main() @suspends {{
    var n = 0
    while n < 3 {{
        scope {{
            let counter = alloc(Counter {{ value: n }})
            branch {{ bump(counter, n) }}
            if n == 1 {{ break }}
        }}
        n = n + 1
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&break_across);

    let continue_across = format!(
        "{externs}
fn main() @suspends {{
    var n = 0
    while n < 3 {{
        scope {{
            branch {{ delayed_value(n); }}
            if n == 1 {{ continue }}
            delayed_value(n + 10);
        }}
        n = n + 1
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&continue_across);

    let break_inside_scope_loop = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        var n = 0
        while n < 3 {{
            branch {{ delayed_value(n); }}
            if n == 1 {{ break }}
            n = n + 1
        }}
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&break_inside_scope_loop);
}

#[test]
fn branch_requires_an_enclosing_scope_in_the_same_function() {
    let bare = format!(
        "{externs}
fn main() @suspends {{ branch {{ delayed_value(1); }} }}
",
        externs = externs()
    );
    assert_rejected(
        &bare,
        "branch requires an enclosing scope in the same function",
    );

    let called_helper = format!(
        "{externs}
fn helper() @suspends {{ branch {{ delayed_value(1); }} }}
fn main() @suspends {{ scope {{ helper() }} }}
",
        externs = externs()
    );
    assert_rejected(
        &called_helper,
        "branch requires an enclosing scope in the same function",
    );
}

#[test]
fn scope_and_branch_require_suspending_callers() {
    let sync_scope = format!(
        "{externs}
fn main() {{ scope {{ record(1); }} }}
",
        externs = externs()
    );
    assert_rejected(&sync_scope, "scope must be inside an @suspends function");

    let sync_branch = format!(
        "{externs}
fn main() {{ branch {{ record(1); }} }}
",
        externs = externs()
    );
    assert_rejected(&sync_branch, "branch must be inside an @suspends function");
}

#[test]
fn parent_access_conflicting_with_child_borrows_is_rejected() {
    let exclusive_after_branch = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 1 }})
        branch {{ keep(counter) }}
        bump(counter, 2);
    }}
}}
",
        externs = externs()
    );
    assert_rejected(
        &exclusive_after_branch,
        "Reference access conflicts with a borrow retained by the task arena",
    );

    let second_branch_conflict = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 1 }})
        branch {{ keep(counter) }}
        branch {{ bump(counter, 2) }}
    }}
}}
",
        externs = externs()
    );
    assert_rejected(
        &second_branch_conflict,
        "Reference access conflicts with a borrow retained by the task arena",
    );
}

#[test]
fn cross_iteration_conflicts_are_rejected_across_iterations() {
    let conflicting_loop = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 0 }})
        var i = 0
        while i < 4 {{
            branch {{ keep(counter) }}
            bump(counter, i);
            i = i + 1
        }}
    }}
}}
",
        externs = externs()
    );
    assert_rejected(
        &conflicting_loop,
        "Reference access conflicts with a borrow retained by the task arena",
    );
}

#[test]
fn owner_captures_move_and_reject_later_uses() {
    let moved = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        let text = \"hello\"
        branch {{ send(text) }}
        send(text);
    }}
}}
",
        externs = externs()
    );
    let error = checked_vm(&moved, VmMode::Jit).unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::InvalidAssignment, "{error}");

    let conditional = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        let text = \"hello\"
        if true {{
            branch {{ send(text) }}
        }} else {{
            send(text);
        }}
    }}
}}
",
        externs = externs()
    );
    assert_not_executable(&conditional);
}

#[test]
fn scope_bodies_and_branch_results_stay_unit() {
    let value_body = format!(
        "{externs}
fn main() @suspends {{
    let result = scope {{ delayed_value(1) 42 }}
}}
",
        externs = externs()
    );
    let error = checked_vm(&value_body, VmMode::Jit).unwrap_err();
    assert!(error.message().contains("Unit"), "{error}");
}

#[test]
fn branch_bodies_cannot_jump_to_parent_loops() {
    let break_out = format!(
        "{externs}
fn main() @suspends {{
    scope {{
        var i = 0
        while i < 3 {{
            branch {{
                if i == 1 {{ break }}
                delayed_value(i);
            }}
            i = i + 1
        }}
    }}
}}
",
        externs = externs()
    );
    assert_rejected(&break_out, "break must be inside a while loop");
}
