use super::{VmEngine, VmMode};
use crate::runtime::arena::{LIMIT, LIVE, MAX_BYTES};

#[test]
fn sql_templates_preserve_types_snapshots_and_evaluation_order() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../../tests/fixtures/sql_templates.jk"))
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        let result = crate::runtime::driver::block_on(vm.prepare_async("main", &[]).unwrap());
        assert_eq!(result.unwrap_or_else(|e| panic!("{mode:?}: {e}")), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn sql_templates_reject_untyped_fragments_and_unsupported_parameters() {
    for (source, expected) in [
        (
            r#"fn main() { let table = "orders"; let q = sql"SELECT * FROM {table}" }"#,
            "Identifier",
        ),
        (r#"fn main() { let q = sql"SELECT {42}" }"#, "Identifier"),
        (
            r#"struct Data {} fn main() { let q = sql"SELECT ${Data {}}" }"#,
            "BindParam",
        ),
        (
            r#"import joky/postgres fn main() { let q = sql"SELECT ${postgres.ident("orders")!}" }"#,
            "BindParam",
        ),
        (
            r#"import joky/postgres fn main() { let q = postgres.Identifier { quoted: "DROP TABLE orders" } }"#,
            "private",
        ),
    ] {
        let error = crate::core::prepared::check_source(source, &Default::default()).unwrap_err();
        assert!(error.message().contains(expected), "{source}: {error}");
    }
}

const LIFECYCLE: &str = r#"
import joky/postgres
import joky/task
fn later() @suspends -> String { task.yield_now(); "later" }
fn main() @suspends -> Int32 {
    let label = "owned"
    let q = sql"SELECT ${label}, {postgres.ident("column")!}, ${later()}"
    if q.params().len() == 2 { 42 } else { 1 }
}
"#;

#[test]
fn sql_templates_cancel_partial_builders_at_resource_boundaries() {
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(LIFECYCLE).unwrap_or_else(|e| panic!("{e}"));
    super::support::assert_cancellation(
        &mut vm,
        "main",
        10000,
        super::support::CancellationSweep::ResourceBoundaries,
        || {},
        || (),
        || LIVE.with(|n| assert_eq!(n.get(), 0)),
    );
}

#[test]
fn sql_templates_reclaim_snapshots_on_allocation_failure() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(MAX_BYTES));
        }
    }
    let _reset = Reset;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(r#"fn test() -> Int32 { let text = "owned"; let q = sql"SELECT ${text}, ${42}"; if q.params().len() == 2 { 42 } else { 0 } }"#)
            .unwrap_or_else(|e| panic!("{e}"));
        let mut completed = false;
        for limit in (1..16384).step_by(97) {
            LIMIT.with(|n| n.set(limit));
            let result = vm.call_int32_0("test");
            LIMIT.with(|n| n.set(MAX_BYTES));
            LIVE.with(|n| assert_eq!(n.get(), 0));
            if let Ok(result) = result {
                assert_eq!(result, 42);
                completed = true;
                break;
            }
        }
        assert!(completed, "{mode:?}: no complete allocation budget");
    }
}
