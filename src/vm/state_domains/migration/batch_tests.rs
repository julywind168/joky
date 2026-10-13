use super::super::{batch, tests::read};
use super::*;
use crate::vm::VmMode;
const OLD: &str = include_str!("../../../../tests/fixtures/migration_batch_old.jk");
const NEW: &str = include_str!("../../../../tests/fixtures/migration_batch_new.jk");
fn setup(mode: VmMode) -> (VmEngine, Vec<DomainOwner>, Vec<Rc<dyn Any>>) {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(OLD).unwrap();
    let domains = [("make", 42), ("make", 42), ("ledger", 100), ("rules", 7)]
        .into_iter()
        .map(|(name, n)| vm.create_domain(name, &[InstanceValue::Int32(n)]).unwrap())
        .collect();
    let owners = vec![vm.script_code_owner().unwrap()];
    (vm, domains, owners)
}
fn staged(vm: &VmEngine, owners: &[Rc<dyn Any>], source: &str) -> Batch {
    let mut batch = batch::begin(vm, &vm.domains, owners, source).unwrap_or_else(|e| panic!("{e}"));
    stage(vm, &vm.domains, owners, &mut batch, "start", "advance").unwrap();
    stage(vm, &vm.domains, owners, &mut batch, "upgrade_ledger", "").unwrap();
    batch
}
fn finish(vm: &VmEngine, owners: &[Rc<dyn Any>], batch: &mut Batch) {
    for i in 0..12 {
        assert_eq!(
            batch::step(vm, &vm.domains, owners, batch, 2).unwrap(),
            i == 11
        );
    }
}
fn check(vm: &VmEngine, domains: &[DomainOwner], upgraded: bool, rules: i32) {
    for d in &domains[..2] {
        assert_eq!(
            read(vm, d, "read"),
            InstanceValue::Int32(if upgraded { 43 } else { 42 })
        );
    }
    assert_eq!(
        read(vm, &domains[2], "ledger_read"),
        InstanceValue::Int32(if upgraded { 101 } else { 100 })
    );
    assert_eq!(
        read(vm, &domains[3], "rules_read"),
        InstanceValue::Int32(rules)
    );
    assert_eq!(
        vm.call_int32_0("version").unwrap(),
        if upgraded { 2 } else { 1 }
    );
}
fn change(vm: &VmEngine, owner: &DomainOwner) {
    let mut r = submit(
        &vm.domains,
        owner.write_handle(),
        "rules_add",
        &[InstanceValue::Int32(1)],
        Default::default(),
    )
    .unwrap();
    vm.poll_tasks(10).unwrap();
    assert!(matches!(
        r.take_result(),
        Some(DomainOutcome::Value(InstanceValue::Int32(8)))
    ));
}
#[test]
fn migration_batch_atomically_publishes_three_roots_and_preserves_read_dependency() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, domains, owners) = setup(mode);
        let registry = vm.domains.clone();
        let before: Vec<_> = domains
            .iter()
            .map(|d| {
                let r = registry.borrow();
                let d = r.get(d.read_handle()).unwrap();
                (
                    d.revision,
                    d.code,
                    d.schema,
                    d.root.as_ref().unwrap().borrow_pointer(&d.ty),
                )
            })
            .collect();
        let baseline = registry.borrow().usage();
        let batch = staged(&vm, &owners, NEW);
        assert!(batch::publish(&mut vm, &registry, &owners, batch).is_err());
        assert_eq!(registry.borrow().usage(), baseline);
        let mut batch = staged(&vm, &owners, NEW);
        for i in 0..12 {
            assert_eq!(
                batch::step(&vm, &registry, &owners, &mut batch, 2).unwrap(),
                i == 11
            );
            check(&vm, &domains, false, 7);
        }
        assert!(batch::step(&vm, &registry, &owners, &mut batch, 2).unwrap());
        batch::publish(&mut vm, &registry, &owners, batch).unwrap();
        check(&vm, &domains, true, 7);
        for (i, owner) in domains.iter().enumerate() {
            let r = registry.borrow();
            let d = r.get(owner.read_handle()).unwrap();
            assert_eq!(d.code, before[i].1 + 1);
            if i == 3 {
                assert_eq!(
                    (
                        d.revision,
                        d.schema,
                        d.root.as_ref().unwrap().borrow_pointer(&d.ty)
                    ),
                    (before[i].0, before[i].2, before[i].3)
                );
            } else {
                assert_eq!(d.revision, before[i].0 + 1);
                assert_ne!(d.schema, before[i].2);
            }
        }
        drop(domains);
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn migration_batch_fourth_domain_conflicts_reject_stage_step_and_final_publication() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for phase in ["stage", "step", "publish"] {
            let (mut vm, domains, owners) = setup(mode);
            let registry = vm.domains.clone();
            let baseline = registry.borrow().usage();
            let mut batch = if phase == "stage" {
                batch::begin(&vm, &registry, &owners, NEW).unwrap()
            } else {
                staged(&vm, &owners, NEW)
            };
            if phase == "publish" {
                finish(&vm, &owners, &mut batch);
            }
            change(&vm, &domains[3]);
            match phase {
                "stage" => {
                    assert!(
                        stage(&vm, &registry, &owners, &mut batch, "start", "advance").is_err()
                    );
                    drop(batch);
                }
                "step" => {
                    assert!(batch::step(&vm, &registry, &owners, &mut batch, 2).is_err());
                    drop(batch);
                }
                _ => assert!(batch::publish(&mut vm, &registry, &owners, batch).is_err()),
            }
            check(&vm, &domains, false, 8);
            assert_eq!(registry.borrow().usage(), baseline);
            let mut batch = staged(&vm, &owners, NEW);
            finish(&vm, &owners, &mut batch);
            batch::publish(&mut vm, &registry, &owners, batch).unwrap();
            check(&vm, &domains, true, 8);
            drop(domains);
            assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
#[test]
fn migration_batch_late_failure_and_counter_exhaustion_never_publish_earlier_roots() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for failure in [
            "constructor",
            "duplicate",
            "missing",
            "close",
            "selection",
            "binding",
            "code",
            "schema",
            "revision_limit",
            "code_limit",
        ] {
            let (mut vm, domains, owners) = setup(mode);
            let registry = vm.domains.clone();
            let baseline = registry.borrow().usage();
            let source = if failure == "constructor" {
                NEW.replace("import joky/domain","import joky/domain import joky/check").replace("Ledger{text:old.text.clone(),total:int64(old.total),generation:1}","let copy=old.text.clone() let message=\"late failure\" check.require(false,&message) Ledger{text:copy,total:int64(old.total),generation:1}")
            } else {
                NEW.into()
            };
            let mut batch = batch::begin(&vm, &registry, &owners, &source).unwrap();
            stage(&vm, &registry, &owners, &mut batch, "start", "advance").unwrap();
            let mut extra = None;
            match failure {
                "constructor" => {
                    assert!(
                        stage(&vm, &registry, &owners, &mut batch, "upgrade_ledger", "").is_err()
                    );
                    drop(batch);
                }
                "duplicate" => {
                    assert!(
                        stage(&vm, &registry, &owners, &mut batch, "start", "advance").is_err()
                    );
                    drop(batch);
                }
                "missing" => {
                    assert!(batch::publish(&mut vm, &registry, &owners, batch).is_err());
                }
                _ => {
                    stage(&vm, &registry, &owners, &mut batch, "upgrade_ledger", "").unwrap();
                    finish(&vm, &owners, &mut batch);
                    let last = domains[3].write_handle().sequence;
                    match failure {
                        "close" => {
                            domains[3].close();
                        }
                        "selection" => {
                            extra = Some(
                                vm.create_domain("rules", &[InstanceValue::Int32(9)])
                                    .unwrap(),
                            );
                        }
                        "binding" => {
                            vm.load_source(OLD).unwrap();
                        }
                        "code" => registry.borrow_mut().entries.get_mut(&last).unwrap().code += 1,
                        "schema" => {
                            registry.borrow_mut().entries.get_mut(&last).unwrap().schema += 1
                        }
                        "revision_limit" => {
                            registry
                                .borrow_mut()
                                .entries
                                .get_mut(&domains[2].write_handle().sequence)
                                .unwrap()
                                .revision = u64::MAX
                        }
                        "code_limit" => {
                            registry.borrow_mut().entries.get_mut(&last).unwrap().code = u64::MAX
                        }
                        _ => unreachable!(),
                    }
                    // Refresh only the expected exhausted counter to reach publication preflight.
                    if failure.ends_with("_limit") {
                        let targets: Vec<_> = batch.entries.iter().map(|e| e.target).collect();
                        batch.reads =
                            reads::ReadSet::capture_many(&registry.borrow(), &targets).unwrap();
                    }
                    let error = batch::publish(&mut vm, &registry, &owners, batch).unwrap_err();
                    if failure.ends_with("_limit") {
                        assert_eq!(error.code(), ErrorCode::ResourceLimit);
                    }
                }
            }
            drop(extra);
            if failure != "close" {
                check(&vm, &domains, false, 7);
                assert_eq!(registry.borrow().usage(), baseline);
            } else {
                for d in &domains[..2] {
                    assert_eq!(read(&vm, d, "read"), InstanceValue::Int32(42));
                }
            }
            drop(domains);
            assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
        }
    }
}

// Reuse compiled code while giving every attempt fresh candidate ownership.
fn empty(template: &Batch, registry: &Rc<RefCell<Domains>>) -> Batch {
    let entries = template
        .entries
        .iter()
        .map(|e| Entry {
            target: e.target,
            old: e.old.clone(),
            ty: e.ty.clone(),
            root: None,
            retired_module: None,
            step: None,
            schema: e.schema,
        })
        .collect();
    Batch {
        entries,
        registry: template.registry.clone(),
        reads: template.reads.clone(),
        original: template.original.clone(),
        module: template.module.clone(),
        next_version: template.next_version,
        cursor: 0,
        _charge: reads::Charge::new(
            registry.borrow().budget.clone(),
            std::mem::size_of::<Batch>()
                + template.entries.len()
                    * (std::mem::size_of::<Entry>() + std::mem::size_of::<DomainHandle>()),
        )
        .unwrap(),
    }
}
fn allocation_budgets(limits: impl Clone + IntoIterator<Item = usize>) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (vm, domains, owners) = setup(mode);
        let registry = vm.domains.clone();
        let template = batch::begin(&vm, &registry, &owners, NEW).unwrap();
        let budget = registry.borrow().budget.clone();
        let baseline = budget.bytes.get();
        let live = crate::runtime::arena::LIVE.with(|n| n.get());
        for phase in ["stage", "step"] {
            let mut success = false;
            let mut failed = false;
            for extra in limits.clone() {
                budget.limit.set(usize::MAX);
                let mut batch = empty(&template, &registry);
                if phase == "step" {
                    stage(&vm, &registry, &owners, &mut batch, "start", "advance").unwrap();
                    stage(&vm, &registry, &owners, &mut batch, "upgrade_ledger", "").unwrap();
                    assert!(!batch::step(&vm, &registry, &owners, &mut batch, 2).unwrap());
                }
                budget.limit.set(budget.bytes.get() + extra);
                let result = if phase == "stage" {
                    stage(&vm, &registry, &owners, &mut batch, "start", "advance").and_then(|()| {
                        stage(&vm, &registry, &owners, &mut batch, "upgrade_ledger", "")
                    })
                } else {
                    (|| {
                        while !batch::step(&vm, &registry, &owners, &mut batch, 12)? {}
                        Ok(())
                    })()
                };
                match result {
                    Ok(()) => success = true,
                    Err(error) => {
                        assert_eq!(
                            error.code(),
                            ErrorCode::ResourceLimit,
                            "{phase} {extra}: {error}"
                        );
                        failed = true;
                    }
                }
                drop(batch);
                assert_eq!(budget.bytes.get(), baseline, "{mode:?} {phase} {extra}");
                assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), live);
                if success {
                    break;
                }
            }
            assert!(success && failed);
        }
        budget.limit.set(usize::MAX);
        check(&vm, &domains, false, 7);
        drop(template);
        drop(domains);
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn migration_batch_allocation_failures_release_all_earlier_candidates() {
    allocation_budgets([
        0, 1, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
    ]);
}
#[test]
#[ignore = "exhaustive: every byte budget across batch construction and later steps"]
fn exhaustive_migration_batch_allocation_budgets() {
    allocation_budgets(0..=65536);
}
#[test]
fn migration_batch_filters_module_ownership_and_bounds_selection() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, domains, owners) = setup(mode);
        let registry = vm.domains.clone();
        let mut foreign = VmEngine::with_mode(mode);
        foreign.load_source(OLD).unwrap();
        let other = super::super::super::create(
            &registry,
            foreign.current.clone().unwrap(),
            "make",
            &[InstanceValue::Int32(42)],
            Default::default(),
        )
        .unwrap();
        let mut batch = staged(&vm, &owners, NEW);
        finish(&vm, &owners, &mut batch);
        batch::publish(&mut vm, &registry, &owners, batch).unwrap();
        check(&vm, &domains, true, 7);
        assert_eq!(read(&vm, &other, "read"), InstanceValue::Int32(42));
        drop(other);
        drop(domains);
        assert!(batch::begin(&vm, &registry, &owners, NEW).is_err());
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
        vm.load_source(
            "struct Tiny {n:Int32} fn make(n:Int32)->Tiny {Tiny{n:n}} fn read(s:&Tiny)->Int32{s.n}",
        )
        .unwrap();
        let owners = vec![vm.script_code_owner().unwrap()];
        let mut domains = Vec::new();
        for _ in 0..=MAX_DOMAINS {
            domains.push(
                vm.create_domain("make", &[InstanceValue::Int32(1)])
                    .unwrap(),
            );
        }
        let baseline = registry.borrow().usage();
        assert!(batch::begin(&vm, &registry, &owners, NEW).is_err());
        assert_eq!(registry.borrow().usage(), baseline);
        drop(domains);
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn migration_batch_rejects_mixed_old_layouts_of_the_same_nominal_type() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, domains, mut owners) = setup(mode);
        vm.load_source(
            "struct Room {different:Int64} fn make(n:Int32)->Room {Room{different:int64(n)}}",
        )
        .unwrap();
        owners.push(vm.script_code_owner().unwrap());
        let extra = vm
            .create_domain("make", &[InstanceValue::Int32(1)])
            .unwrap();
        let baseline = vm.domains.borrow().usage();
        assert!(batch::begin(&vm, &vm.domains, &owners, NEW).is_err());
        assert_eq!(vm.domains.borrow().usage(), baseline);
        drop(extra);
        drop(domains);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn migration_batch_shares_old_views_across_compatible_module_generations() {
    let old="struct Player {n:Int32} struct Room {player:Player} fn room(n:Int32)->Room {Room{player:Player{n:n}}} fn player(n:Int32)->Player {Player{n:n}}";
    let new="struct Player {n:Int64} struct Room {player:Player,level:Int32} fn room(old:&PreviousRoom)->Room {Room{player:Player{n:int64(old.player.n)},level:1}} fn player(old:&PreviousPlayer)->Player {Player{n:int64(old.n)}} fn read_room(s:&Room)->Int32 {int32(s.player.n)+s.level} fn read_player(s:&Player)->Int32 {int32(s.n)}";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(old).unwrap();
        let room = vm
            .create_domain("room", &[InstanceValue::Int32(7)])
            .unwrap();
        let mut owners = vec![vm.script_code_owner().unwrap()];
        vm.load_source(old).unwrap();
        owners.push(vm.script_code_owner().unwrap());
        let player = vm
            .create_domain("player", &[InstanceValue::Int32(9)])
            .unwrap();
        let registry = vm.domains.clone();
        let mut batch =
            batch::begin(&vm, &registry, &owners, new).unwrap_or_else(|e| panic!("{e}"));
        stage(&vm, &registry, &owners, &mut batch, "room", "").unwrap();
        stage(&vm, &registry, &owners, &mut batch, "player", "").unwrap();
        batch::publish(&mut vm, &registry, &owners, batch).unwrap();
        assert_eq!(read(&vm, &room, "read_room"), InstanceValue::Int32(8));
        assert_eq!(read(&vm, &player, "read_player"), InstanceValue::Int32(9));
        drop((room, player));
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}
