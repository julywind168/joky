//! Worker dispatch shares admission only within one frozen root and thread.
use super::*;
use crate::runtime::parallel::{Guard, Storage, TaskBudget};

fn frozen() -> FrozenEntry {
    let ast = crate::parser::parse("fn main() @suspends -> Int32 { 42 }").unwrap();
    compile_with_types(
        &ast,
        &HashMap::new(),
        HashMap::new(),
        ModuleId(1),
        &TypeRegistry::default().with_prelude(false),
    )
    .unwrap()
    .freeze("main")
    .unwrap()
}

#[test]
fn frozen_worker_dispatch_shares_admission_and_refunds() {
    let code = frozen();
    let budget = TaskBudget::new(1 << 20);
    let root = Storage::new(vec![budget.clone()]).unwrap();
    {
        let _guard = Guard::enter(&root);
        let baseline = budget.used();
        let first = code.instantiate().unwrap();
        assert!(budget.used() > baseline);
        let occupied = root.accounts().reserve((1 << 20) - budget.used()).unwrap();
        // Even with no remaining budget, live children on this worker need no
        // second dispatch allocation. This used to fail on the first child.
        let children: Vec<_> = (0..32).map(|_| code.instantiate().unwrap()).collect();
        for child in &children {
            assert!(Rc::ptr_eq(&first, child));
        }
        drop(occupied);
        let cleanup = first.as_ref().clone();
        drop((first, children));
        assert!(budget.used() > baseline);
        drop(cleanup);
        assert_eq!(budget.used(), baseline);
        // An expired cache entry must rebuild and charge again.
        let next = code.instantiate().unwrap();
        assert!(budget.used() > baseline);
        drop(next);
        assert_eq!(budget.used(), baseline);
    }
    drop(root);
    assert_eq!(budget.used(), 0);
}

#[test]
fn frozen_worker_dispatch_isolates_roots_code_and_threads() {
    let code = frozen();
    let other_code = frozen(); // Same ModuleId, but a different code snapshot.
    let budget = TaskBudget::new(1 << 20);
    let root = Storage::new(vec![budget.clone()]).unwrap();
    let other_root = Storage::new(vec![budget.clone()]).unwrap();
    {
        let _guard = Guard::enter(&root);
        let first = code.instantiate().unwrap();
        let occupied = root.accounts().reserve((1 << 20) - budget.used()).unwrap();
        assert!(matches!(other_code.instantiate(), Err(e) if e.code() == ErrorCode::ResourceLimit));
        {
            let _guard = Guard::enter(&other_root);
            assert!(matches!(code.instantiate(), Err(e) if e.code() == ErrorCode::ResourceLimit));
        }
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = Guard::enter(&root);
                    assert!(
                        matches!(code.instantiate(), Err(e) if e.code() == ErrorCode::ResourceLimit)
                    );
                })
                .join()
                .unwrap();
        });
        drop(occupied);
        // Failed admission must not poison either cache key.
        let other = other_code.instantiate().unwrap();
        assert!(!Rc::ptr_eq(&first, &other));
        let _guard = Guard::enter(&other_root);
        let separate = code.instantiate().unwrap();
        assert!(!Rc::ptr_eq(&first, &separate));
    }
    drop((root, other_root));
    assert_eq!(budget.used(), 0);
}
