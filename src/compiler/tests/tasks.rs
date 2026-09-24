use super::*;

#[test]
fn preserves_values_across_time_sleep_codegen_boundary() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(message: String) -> String effects { time } { time.sleep(1ms); message }\n\
                 fn main() effects { time } { println(wait(\"awake\")) }",
        )
        .expect("values used after time.sleep should survive codegen");
}

#[test]
fn executes_captured_parallel_and_race_through_the_task_runtime() {
    Compiler::new()
        .unwrap()
        .run_program(
            "fn first(value: Int32) -> Int32 { value + 1 }\n\
                 fn second(value: Int32) -> Int32 { value + 2 }\n\
                 fn main() {\n\
                     let offset = 40\n\
                     let values = parallel {\n| first(offset)\n| second(offset)\n}\n\
                     println(values.0)\n\
                     let winner = race {\n| first(offset)\n| second(offset)\n}\n\
                     println(winner)\n\
                 }",
        )
        .expect("captured parallel and race tasks should execute through the runtime");
}

#[test]
fn executes_a_shared_string_capture_in_a_task() {
    Compiler::new()
        .unwrap()
        .run_program(
            "fn show(value: String) { println(value) }\n\
                 fn main() { let value = \"task\"; branch { show(value) } }",
        )
        .expect("shared task captures should be retained for the task lifetime");
}

#[test]
fn waits_for_root_branch_before_main_returns() {
    Compiler::new()
        .unwrap()
        .run_program(
            "fn work() { println(7) }\n\
                 fn main() { branch { work() } }",
        )
        .expect("root branch should be joined when the implicit scope exits");
}

#[test]
fn transfers_a_unique_class_capture_into_a_task() {
    Compiler::new()
        .unwrap()
        .run_program(
            "class Token { let value: Int32 }\n\
                 fn consume(token: Token) { println(token.value) }\n\
                 fn main() { let token = Token(value: 9); branch { consume(token) } }",
        )
        .expect("a unique capture should move into its task");
}

#[test]
fn drops_losing_owned_race_results() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Token { let value: Int32 }\n\
                 fn first() -> Token { Token(value: 1) }\n\
                 fn second() -> Token { Token(value: 2) }\n\
                 fn main() { let winner = race {\n| first()\n| second()\n}; println(winner.value) }",
            )
            .expect("race should retain only the winner's owned result");
}

#[test]
fn drops_an_unclaimed_branch_result_at_scope_exit() {
    Compiler::new()
        .unwrap()
        .run_program(
            "class Token { let value: Int32 }\n\
                 fn make() -> Token { Token(value: 3) }\n\
                 fn main() { branch { make() } }",
        )
        .expect("scope exit should dispose an unclaimed branch result");
}

#[test]
fn race_cancels_a_looping_task_at_a_cooperative_poll() {
    Compiler::new()
        .unwrap()
        .run_program(
            "fn fast() {}\n\
                 fn main() { race {\n| fast()\n| loop { }\n} }",
        )
        .expect("race should cancel a looping task at its poll point");
}

#[test]
fn unhandled_aborting_task_cancels_its_sibling_and_reports_failure() {
    let error = Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop() -> Unit }\n\
                 fn main() effects { Failure } { let ignored = parallel {\n| Failure.stop()\n| loop { }\n} }",
            )
            .expect_err("an unhandled child abort should reach the root scope");
    assert!(error
        .to_string()
        .contains("unhandled aborting effect operation 'Failure.stop'"));
}

#[test]
fn unhandled_normal_effect_reports_failure_without_consuming_an_invalid_result() {
    let error = Compiler::new()
        .unwrap()
        .run_program(
            "eff Clock { fn now() -> Int64 }\n\
                 fn main() -> Int64 effects { Clock } { Clock.now() }",
        )
        .expect_err("an unhandled normal effect should reach the root scope");
    assert!(error
        .to_string()
        .contains("unhandled effect operation 'Clock.now'"));
}

#[test]
fn abort_handler_receives_a_payload_without_resuming_the_call_site() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop(message: String) -> Unit }\n\
                 fn main() -> Int32 { do { Failure.stop(message: \"nope\"); 0 } with { Failure.stop(message) => 42 } }",
            )
            .expect("an abort handler should produce the do expression result");
}

#[test]
fn parent_abort_handler_catches_a_child_task_failure() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop() -> Unit }\n\
                 fn fail() effects { Failure } { Failure.stop() }\n\
                 fn main() -> Int32 { do { let ignored = parallel {\n| fail()\n| loop { }\n}; 0 } with { Failure.stop() => 42 } }",
            )
            .expect("parent abort handler should catch a child task failure");
}

#[test]
fn parent_abort_handler_receives_typed_child_payload() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop(code: Int32, message: String) -> Unit }\n\
                 fn fail() effects { Failure } { Failure.stop(code: 41, message: \"child\") }\n\
                 fn main() -> Int32 { do { let ignored = parallel {\n| fail()\n| loop { }\n}; 0 } with { Failure.stop(code, message) => { println(message); code + 1 } } }",
            )
            .expect("parent abort handler should receive typed child payload");
}

#[test]
fn nested_parent_abort_handler_catches_a_grandchild_failure() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop(code: Int32) -> Unit }\n\
                 fn fail() effects { Failure } { Failure.stop(code: 7) }\n\
                 fn nested() effects { Failure } { let ignored = parallel {\n| fail()\n| loop { }\n} }\n\
                 fn main() -> Int32 { do { let ignored = parallel {\n| nested()\n| loop { }\n}; 0 } with { Failure.stop(code) => code } }",
            )
            .expect("abort failure should propagate through nested task scopes");
}

#[test]
fn owned_child_abort_payload_is_dropped_once_by_the_parent_handler() {
    Compiler::new()
            .unwrap()
            .run_program(
                "@intrinsic pub class MutList(T: type) {\n\
                     fn length(&self) -> UInt64\n\
                     fn push(&self, item: T) -> Unit\n\
                 }\n\
                 eff Failure { @aborts fn stop(values: MutList(Int32)) -> Unit }\n\
                 fn fail() effects { Failure } {\n\
                     let values = MutList(Int32)()\n\
                     values.push(9)\n\
                     Failure.stop(values: values)\n\
                 }\n\
                 fn main() -> Int32 { do { let ignored = parallel {\n| fail()\n| loop { }\n}; 0 } with { Failure.stop(values) => { let _ = values.length(); 9 } } }",
            )
            .expect("owned abort payload should transfer to the parent handler");
}

#[test]
fn concurrent_losing_abort_payload_is_dropped() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop(message: String) -> Unit }\n\
                 fn fail(message: String) effects { Failure } { Failure.stop(message: message) }\n\
                 fn main() -> Int32 { do { let ignored = parallel {\n| fail(\"first\")\n| fail(\"second\")\n}; 0 } with { Failure.stop(message) => { println(message); 1 } } }",
            )
            .expect("the first concurrent abort should own the propagated payload");
}

#[test]
fn concurrent_losing_nested_rethrow_payload_is_dropped() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop(message: String) -> Unit }\n\
                 fn fail(message: String) effects { Failure } { Failure.stop(message: message) }\n\
                 fn nested(message: String) effects { Failure } { let ignored = parallel {\n| fail(message)\n| loop { }\n} }\n\
                 fn main() -> Int32 { do { let ignored = parallel {\n| nested(\"first\")\n| nested(\"second\")\n}; 0 } with { Failure.stop(message) => { println(message); 1 } } }",
            )
            .expect("nested task failures should transfer or drop their payload exactly once");
}

#[test]
fn successful_sibling_result_is_dropped_when_parallel_aborts() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Token { let value: Int32 }\n\
                 eff Failure { @aborts fn stop() -> Unit }\n\
                 fn succeed() -> Token { Token(value: 1) }\n\
                 fn fail() effects { Failure } { Failure.stop() }\n\
                 fn main() -> Int32 { do { let _ = parallel {\n| succeed()\n| fail()\n}; 0 } with { Failure.stop() => 1 } }",
            )
            .expect("an aborted parallel scope should drop completed sibling results");
}

#[test]
fn unhandled_root_abort_is_reported_and_drops_its_payload() {
    let error = Compiler::new()
        .unwrap()
        .run_program(
            "eff Failure { @aborts fn stop(message: String) -> Unit }\n\
                 fn main() effects { Failure } { Failure.stop(message: \"root\") }",
        )
        .expect_err("an unhandled root abort must not be silently swallowed");
    assert!(error
        .to_string()
        .contains("unhandled aborting effect operation 'Failure.stop'"));
}

#[test]
fn nearest_parent_abort_handler_wins() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop(code: Int32) -> Unit }\n\
                 fn fail() effects { Failure } { Failure.stop(code: 5) }\n\
                 fn main() -> Int32 { do { do { let ignored = parallel {\n| fail()\n| loop { }\n}; 0 } with { Failure.stop(code) => code + 1 } } with { Failure.stop(code) => code + 100 } }",
            )
            .expect("the nearest abort handler should receive the child failure");
}

#[test]
fn race_propagates_child_abort_instead_of_selecting_a_winner() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Failure { @aborts fn stop(message: String) -> Unit }\n\
                 fn fail() effects { Failure } { Failure.stop(message: \"race failed\") }\n\
                 fn main() { do { race {\n| fail()\n| loop { }\n} } with { Failure.stop(message) => println(message) } }",
            )
            .expect("race should propagate a child abort to its parent handler");
}

#[test]
fn cancellation_does_not_drop_a_placeholder_owned_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            "class Token { let value: Int32 }\n\
                 fn main() {\n\
                     let winner = race {\n| Token(value: 1)\n| { loop { }\nToken(value: 2) }\n}\n\
                     println(winner.value)\n\
                 }",
        )
        .expect("a cancelled owned task result is only a placeholder");
}
