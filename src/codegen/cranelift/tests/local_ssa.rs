use super::*;
use crate::mir::{MirOwnership, MirValueId};
use joky_runtime::host::{testing, RuntimeScope};
use std::time::{Duration, Instant};

// Local assignment syntax is introduced in the next stage. Replace immutable
// test bindings with assignments to the same MIR binding identity for now.
fn assign_locals(mir: &mut MirProgram, name: &str, assignments: &[(&str, &str, bool)]) {
    let function = mir.functions.iter_mut().find(|f| f.name == name).unwrap();
    let mappings = assignments
        .iter()
        .map(|&(replacement, original, consumed)| {
            let find = |name| {
                function
                    .locals
                    .iter()
                    .find(|local| local.name == name)
                    .unwrap()
                    .id
            };
            (find(replacement), find(original), consumed)
        })
        .collect::<Vec<_>>();
    for block in &mut function.blocks {
        let mut rewritten = Vec::new();
        for mut statement in block.statements.drain(..) {
            if matches!(&statement, MirStatement::DropLocal { local, .. }
                if mappings.iter().any(|(replacement, _, consumed)| replacement == local && !consumed))
            {
                continue;
            }
            match &mut statement {
                MirStatement::Bind { local, .. } => {
                    if let Some(&(_, target, consumed)) =
                        mappings.iter().find(|(source, _, _)| source == local)
                    {
                        *local = target;
                        if !consumed
                            && matches!(
                                function.locals[target.0].ownership,
                                MirOwnership::Shared | MirOwnership::Owned
                            )
                        {
                            let destination = MirValueId(function.value_types.len());
                            function.value_types.push(Type::Unit);
                            function.value_ownership.push(MirOwnership::Copy);
                            function.source.values.resize(destination.0 + 1, None);
                            rewritten.push(MirStatement::DropLocal {
                                destination,
                                local: target,
                            });
                        }
                    }
                }
                MirStatement::Read { local, .. }
                | MirStatement::BorrowLocal { local, .. }
                | MirStatement::TakeLocal { local, .. }
                | MirStatement::DropLocal { local, .. } => {
                    if let Some(&(_, target, _)) =
                        mappings.iter().find(|(source, _, _)| source == local)
                    {
                        *local = target;
                    }
                }
                _ => {}
            }
            rewritten.push(statement);
        }
        block.statements = rewritten;
    }
    crate::mir::local_ssa::normalize(function);
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
}

fn run(mut mir: MirProgram, suspended: bool) {
    // Cached MIR and the unoptimized object backend must preserve the same
    // explicit Phi/ownership representation used by the JIT. Release
    // (`opt_level=speed`) emission is checked once below; repeating it for
    // every case pins a core for little extra coverage.
    mir = bincode::deserialize(&bincode::serialize(&mir).unwrap()).unwrap();
    mir.verify().unwrap();
    assert!(!crate::codegen::emit_mir_object_with_entries(&mir, false)
        .unwrap()
        .bytes
        .is_empty());
    let scope = RuntimeScope::new();
    let guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    scope.wait_for_idle();
    if suspended {
        assert!(testing::machine_resumptions(&scope) > 0);
    }
    drop(guard);
    scope.close_and_wait();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let managed = testing::managed_objects(&scope);
        let resources = testing::resource_counts(&scope);
        if managed == 0 && resources == [0; 6] {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "local SSA run did not drain: managed={managed}, resources={resources:?}, continuations={:?}",
            testing::describe_continuations(&scope)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn local_ssa_copy_loop_and_both_branch_paths_execute_latest_values() {
    let mut mir = lower(
        r#"
        eff Check { @aborts fn failed() -> Unit }
        fn count(increment: Bool) -> Int32 {
            let counter = 0
            while counter < 3 {
                let next_counter = counter + 1
            }
            if increment {
                let positive_counter = counter + 10
            } else {
                let negative_counter = counter - 10
            }
            counter
        }
        fn main() effects { Check } {
            if count(true) != 13 { Check.failed() }
            if count(false) != -7 { Check.failed() }
        }
        "#,
    );
    assign_locals(
        &mut mir,
        "count",
        &[
            ("next_counter", "counter", false),
            ("positive_counter", "counter", false),
            ("negative_counter", "counter", false),
        ],
    );
    run(mir, false);
}

#[test]
fn local_ssa_release_object_emission_accepts_rewritten_phis() {
    let mut mir = lower(
        r#"
        eff Check { @aborts fn failed() -> Unit }
        fn count(increment: Bool) -> Int32 {
            let counter = 0
            while counter < 3 {
                let next_counter = counter + 1
            }
            if increment {
                let positive_counter = counter + 10
            } else {
                let negative_counter = counter - 10
            }
            counter
        }
        fn main() effects { Check } {
            if count(true) != 13 { Check.failed() }
            if count(false) != -7 { Check.failed() }
        }
        "#,
    );
    assign_locals(
        &mut mir,
        "count",
        &[
            ("next_counter", "counter", false),
            ("positive_counter", "counter", false),
            ("negative_counter", "counter", false),
        ],
    );
    mir = bincode::deserialize(&bincode::serialize(&mir).unwrap()).unwrap();
    mir.verify().unwrap();
    assert!(!crate::codegen::emit_mir_object_with_entries(&mir, true)
        .unwrap()
        .bytes
        .is_empty());
}

#[test]
fn local_ssa_managed_loop_replacements_survive_repeated_suspension() {
    let mut mir = lower(
        r#"
        class Token { let value: Int32 }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn wait() -> String effects { time, Check } {
            let token = Token(value: 0)
            let text = "a"
            while token.value < 3 {
                time.sleep(1ms)
                let next_text = text + "b"
                let next_token = Token(value: token.value + 1)
                time.sleep(1ms)
            }
            if token.value != 3 { Check.failed() }
            text
        }
        fn main() effects { time, Check } {
            if wait() != "abbb" { Check.failed() }
        }
        "#,
    );
    assign_locals(
        &mut mir,
        "wait",
        &[("next_text", "text", false), ("next_token", "token", false)],
    );
    run(mir, true);
}

#[test]
fn local_ssa_managed_parameters_merge_replacement_with_entry_value() {
    let mut mir = lower(
        r#"
        class Token { let value: Int32 }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn choose(replace: Bool, text: String, token: Token) -> String effects { time, Check } {
            if replace {
                let next_text = text + "b"
                let next_token = Token(value: token.value + 1)
            }
            time.sleep(1ms)
            if replace {
                if token.value != 2 { Check.failed() }
            } else {
                if token.value != 1 { Check.failed() }
            }
            text
        }
        fn main() effects { time, Check } {
            if choose(true, "a", Token(value: 1)) != "ab" { Check.failed() }
            if choose(false, "a", Token(value: 1)) != "a" { Check.failed() }
        }
        "#,
    );
    assign_locals(
        &mut mir,
        "choose",
        &[("next_text", "text", false), ("next_token", "token", false)],
    );
    run(mir, true);
}

#[test]
fn local_ssa_owned_rhs_move_reinitializes_after_suspend() {
    let mut mir = lower(
        r#"
        class Token { let value: Int32 }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn increment(previous: Token) -> Token effects { time } {
            time.sleep(1ms)
            Token(value: previous.value + 1)
        }
        fn wait() -> Int32 effects { time } {
            let token = Token(value: 41)
            let next_token = increment(token)
            time.sleep(1ms)
            next_token.value
        }
        fn main() effects { time, Check } {
            if wait() != 42 { Check.failed() }
        }
        "#,
    );
    assign_locals(&mut mir, "wait", &[("next_token", "token", true)]);
    run(mir, true);
}

#[test]
fn local_ssa_cancellation_releases_replaced_managed_locals() {
    let mut mir = lower(
        r#"
        class Token { let value: Int32 }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn wait() -> Int32 effects { time } {
            let token = Token(value: 0)
            let text = "a"
            while token.value < 3 {
                let next_text = text + "b"
                let next_token = Token(value: token.value + 1)
                time.sleep(1ms)
            }
            time.sleep(60s)
            println(text)
            token.value
        }
        fn fast() -> Int32 effects { time } { time.sleep(20ms); 42 }
        fn main() effects { time, Check } {
            let result = race {
                | wait()
                | fast()
            }
            if result != 42 { Check.failed() }
        }
        "#,
    );
    assign_locals(
        &mut mir,
        "wait",
        &[("next_text", "text", false), ("next_token", "token", false)],
    );
    run(mir, true);
}

#[test]
fn local_ssa_self_assignment_preserves_copy_shared_and_owned_values() {
    let mut mir = lower(
        r#"
        class Token { let value: Int32 }
        eff Check { @aborts fn failed() -> Unit }
        fn assign() -> Int32 effects { Check } {
            let counter = 41
            let same_counter = counter
            let text = "a"
            let same_text = text
            let next_text = same_text + "b"
            let last_text = next_text + "c"
            let token = Token(value: same_counter)
            let same_token = token
            let last_token = same_token
            if last_text != "abc" { Check.failed() }
            last_token.value
        }
        fn main() effects { Check } {
            if assign() != 41 { Check.failed() }
        }
        "#,
    );
    assign_locals(
        &mut mir,
        "assign",
        &[
            ("same_counter", "counter", false),
            ("same_text", "text", false),
            ("next_text", "text", false),
            ("last_text", "text", false),
            ("same_token", "token", true),
            ("last_token", "token", true),
        ],
    );
    run(mir, false);
}

#[test]
fn local_ssa_copy_loop_preserves_updates_across_continue_break_and_suspend() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn count() -> Int32 effects { time } {
            let counter = 0
            while counter < 5 {
                let next_counter = counter + 1
                if counter == 2 { continue }
                time.sleep(1ms)
                if counter == 4 { break }
            }
            counter
        }
        fn main() effects { time, Check } {
            if count() != 4 { Check.failed() }
        }
        "#,
    );
    assign_locals(&mut mir, "count", &[("next_counter", "counter", false)]);
    run(mir, true);
}
