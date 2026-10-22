use super::*;

const SOURCE: &str = r#"
import joky/domain
struct S{n:Int32}
fn make(n:Int32)->S{S{n:n}}
fn read(s:&S)->Int32{s.n}
fn operation(h:domain.WriteHandle<S>,a:Int32,b:Int32) @suspends ->Int32 {
    when "update" (mut h) {h.n+=a+b; h.n}.unwrap()
}
"#;
const ENTRY: &str = "$when/named/4:main/9:operation/6:757064617465";

#[test]
fn state_domains_when_queued_contracts_rebind_and_reject_incompatible_publications() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for change in [
            "algorithm",
            "capture_name",
            "capture_order",
            "result",
            "label",
            "function",
            "module",
            "access",
        ] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(10)])
                .unwrap();
            let old = vm.current.as_ref().unwrap().clone();
            let output = Rc::new(RefCell::new(vec![0]));
            let enqueue = || {
                pairs::submit_values(
                    &vm.domains,
                    &[owner.write_handle()],
                    &old,
                    ENTRY,
                    vec![2, 3],
                    pairs::ValueOutput {
                        words: output.clone(),
                        snapshot: false,
                    },
                    vm.interpreter_limits(),
                )
            };
            let mut request = enqueue().unwrap();
            let next = match change {
                "algorithm" => SOURCE.replace("a+b", "(a+b)*10"),
                "capture_name" => SOURCE.replace("a:Int32", "c:Int32").replace("a+b", "c+b"),
                "capture_order" => SOURCE.replace("a:Int32,b:Int32", "b:Int32,a:Int32"),
                "result" => SOURCE
                    .replace("@suspends ->Int32", "@suspends ->Int64")
                    .replace("; h.n}", "; int64(h.n)}"),
                "label" => SOURCE.replace("\"update\"", "\"other\""),
                "function" => SOURCE.replace("fn operation", "fn other"),
                "access" => SOURCE.replace("(mut h) {h.n+=a+b; h.n}", "(h) {h.n+a+b}"),
                "module" => SOURCE.into(),
                _ => unreachable!(),
            };
            for multiplier in [1, 2] {
                let next = if change == "algorithm" && multiplier == 2 {
                    next.replace("*10", "*100")
                } else {
                    next.clone()
                };
                let mut ast = crate::parser::parse(&next).unwrap();
                if change == "module" {
                    ast.logical_name = "other".into();
                }
                vm.load_module(&ast)
                    .unwrap_or_else(|e| panic!("{change}: {e}"));
                assert_eq!(vm.publish_domain_code().unwrap(), 1);
            }
            vm.poll_tasks(10000).unwrap();
            let (outcome, rejection) = request.take_access_result().unwrap();
            if matches!(change, "algorithm" | "capture_order") {
                assert!(rejection.is_none(), "{change}: {outcome:?}");
                assert_eq!(
                    *output.borrow(),
                    vec![if change == "algorithm" { 510 } else { 15 }]
                );
                // A still-live old flow may enqueue after publication using its original contract.
                let mut again = pairs::submit_values(
                    &vm.domains,
                    &[owner.write_handle()],
                    &old,
                    ENTRY,
                    vec![2, 3],
                    pairs::ValueOutput {
                        words: output.clone(),
                        snapshot: false,
                    },
                    vm.interpreter_limits(),
                )
                .unwrap();
                vm.poll_tasks(10000).unwrap();
                assert!(again.take_access_result().unwrap().1.is_none());
                read(
                    &mut vm,
                    owner.read_handle(),
                    if change == "algorithm" { 1010 } else { 20 },
                );
            } else {
                assert_eq!(
                    rejection,
                    Some(access::Rejection::Incompatible),
                    "{change}: {outcome:?}"
                );
                read(&mut vm, owner.read_handle(), 10);
            }
            owner.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}

#[test]
fn state_domains_when_preflight_budget_preserves_roots_but_body_failure_isolates_writers() {
    use crate::runtime::arena::TRANSFER_FAIL;
    let source = r#"import joky/domain import joky/mem
struct S{n:Int32,text:String}
fn make(n:Int32)->S{S{n:n,text:"state"}}
fn read(s:&S)->Int32{s.n}
fn operation(a:domain.WriteHandle<S>,b:domain.WriteHandle<S>,c:domain.ReadHandle<S>,zero:Int32) @suspends ->Int32 {
    when "update" (mut a,mut b,c) {mem.swap(&mut a.text,&mut b.text); a.n=c.n; b.n=99; a.n/zero}.unwrap()
}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for before_entry in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source).unwrap();
            let owners = (1..=4)
                .map(|n| {
                    vm.create_domain("make", &[InstanceValue::Int32(n)])
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let handles = [
                owners[0].write_handle(),
                owners[1].write_handle(),
                owners[2].read_handle(),
            ];
            let output = Rc::new(RefCell::new(vec![0]));
            let mut request = pairs::submit_values(
                &vm.domains,
                &handles,
                vm.current.as_ref().unwrap(),
                ENTRY,
                vec![0],
                pairs::ValueOutput {
                    words: output,
                    snapshot: false,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            TRANSFER_FAIL.with(|f| f.set(before_entry.then_some(0)));
            vm.poll_tasks(10000).unwrap();
            TRANSFER_FAIL.with(|f| f.set(None));
            let (outcome, rejection) = request.take_access_result().unwrap();
            assert!(matches!(outcome, DomainOutcome::Failed(_)));
            assert_eq!(
                rejection,
                before_entry.then_some(access::Rejection::ResourceLimit)
            );
            for (index, owner) in owners.iter().enumerate() {
                if before_entry || index >= 2 {
                    read(&mut vm, owner.read_handle(), index as i32 + 1);
                } else {
                    assert!(vm
                        .queue_domain_call(owner.read_handle(), "read", &[])
                        .is_err());
                }
            }
            for owner in owners {
                owner.close();
            }
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
