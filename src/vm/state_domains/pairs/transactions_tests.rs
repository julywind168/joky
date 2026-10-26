use super::*;
use crate::vm::VmMode;

const SOURCE: &str = r#"import joky/domain
import joky/array.{type Array}
struct Data { values:Array<Int32>, text:String }
struct Counter { n:Int32 }
fn data(n:Int32)->Data {
    var values=Array.new::<Int32>()
    var i=0
    while i<n {values.push(i); i+=1}
    Data{values:values,text:"shared"}
}
fn counter(n:Int32)->Counter {Counter{n:n}}
fn read(s:&Counter)->Int32 {s.n}
fn size(s:&Data)->Int32 {s.values.len().to_i32()!}
fn operation(a:domain.WriteHandle<Counter>,b:domain.ReadHandle<Data>,c:domain.ReadHandle<Data>,mode:Int32) @suspends ->Result<Int32,Int32> {
    when "update" (b,mut a,c) {
        let text=b.text.clone()
        a.n=b.values.get(0)!+c.values.get(c.values.len()-1)!
        if mode==2 {a.n=a.n/(mode-2)}
        let result:Result<Int32,Int32>=if mode==1 {Err(9)} else {Ok(a.n)}
        result
    }.unwrap()
}
fn single(b:domain.ReadHandle<Data>) @suspends ->Int32 {when "read" (b){size(b)}!}
fn shared(b:domain.ReadHandle<Data>,c:domain.ReadHandle<Data>) @suspends ->Int32 {when "read" (b,c){size(b)+size(c)}!}
"#;
const UPDATE: &str = "$when/named/4:main/9:operation/6:757064617465";

#[test]
fn state_domains_when_readonly_roots_need_no_candidates_and_survive_mixed_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let data = vm
            .create_domain("data", &[InstanceValue::Int32(16384)])
            .unwrap();
        let counter = vm
            .create_domain("counter", &[InstanceValue::Int32(7)])
            .unwrap();
        let handles = [
            data.read_handle(),
            counter.write_handle(),
            data.read_handle(),
        ];
        let budget = vm.domains.borrow().budget.clone();
        let baseline = budget.bytes.get();
        let live = crate::runtime::arena::LIVE.with(Cell::get);
        let pointer = vm
            .domains
            .borrow()
            .get(data.read_handle())
            .unwrap()
            .root
            .as_ref()
            .unwrap()
            .scalar_pointer();
        // Compare candidate allocation policies on identical live roots. This
        // measures retained candidate bytes only, excluding construction and calls.
        {
            let domains = vm.domains.borrow();
            let all = Candidates::capture(&domains, &handles, &[true, true, true]).unwrap();
            let all_bytes = budget.bytes.get() - baseline;
            drop(all);
            let writes = Candidates::capture(&domains, &handles, &[false, true, false]).unwrap();
            let write_bytes = budget.bytes.get() - baseline;
            eprintln!("{mode:?}: all-root candidates={all_bytes} bytes; write-only candidates={write_bytes} bytes");
            assert!(all_bytes > 65536 && write_bytes < 16384);
            drop(writes);
        }
        // A large read graph fits without snapshot headroom; only the small
        // counter is captured. The same read root may appear more than once.
        budget.limit.set(baseline + 16384);
        {
            let domains = vm.domains.borrow();
            let pure = Candidates::capture(&domains, &handles[..1], &[false]).unwrap();
            assert!(pure.roots.is_empty());
            assert_eq!(budget.bytes.get(), baseline);
            let mixed = Candidates::capture(&domains, &handles, &[false, true, false]).unwrap();
            assert_eq!(mixed.roots.len(), 1);
            assert_eq!(mixed.roots[0].0, counter.write_handle().sequence);
            assert!(Candidates::capture(&domains, &handles, &[true, true, true]).is_err());
        }
        assert_eq!(budget.bytes.get(), baseline);
        let mut admitted = false;
        let mut failures = 0;
        for allowance in (0..=16384).step_by(97) {
            let output = Rc::new(RefCell::new(vec![0; 3]));
            let mut request = submit_values(
                &vm.domains,
                &handles,
                vm.current.as_ref().unwrap(),
                UPDATE,
                vec![1],
                ValueOutput {
                    words: output.clone(),
                    snapshot: false,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            budget.limit.set(baseline + allowance);
            vm.poll_tasks(10000).unwrap();
            let (outcome, rejection) = request.take_access_result().unwrap();
            budget.limit.set(DomainLimits::default().max_state_bytes);
            match outcome {
                DomainOutcome::Value(_) => {
                    assert_eq!(rejection, None);
                    assert_eq!(*output.borrow(), vec![1, 0, 9]);
                    admitted = true;
                }
                DomainOutcome::Failed(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit);
                    assert_eq!(rejection, Some(access::Rejection::ResourceLimit));
                    failures += 1;
                }
                other => panic!("{mode:?}: {other:?}"),
            }
            let mut check = vm
                .queue_domain_call(data.read_handle(), "size", &[])
                .unwrap();
            vm.poll_tasks(10000).unwrap();
            assert!(matches!(
                check.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(16384)))
            ));
            assert_eq!(vm.domain_revision(counter.read_handle()).unwrap(), 0);
            assert_eq!(
                budget.bytes.get(),
                baseline,
                "{mode:?}: allowance={allowance}"
            );
            assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), live);
            if admitted {
                break;
            }
        }
        assert!(admitted && failures > 0);
        budget.limit.set(baseline + 16384);
        for (entry, selected, input, width, expected, revision) in [
            (
                "$when/named/4:main/6:single/4:72656164",
                vec![handles[0]],
                vec![],
                1,
                Some(vec![16384]),
                0,
            ),
            (
                "$when/named/4:main/6:shared/4:72656164",
                vec![handles[0], handles[2]],
                vec![],
                1,
                Some(vec![32768]),
                0,
            ),
            (UPDATE, handles.to_vec(), vec![1], 3, Some(vec![1, 0, 9]), 0),
            (UPDATE, handles.to_vec(), vec![2], 3, None, 0),
            (
                UPDATE,
                handles.to_vec(),
                vec![0],
                3,
                Some(vec![0, 16383, 0]),
                1,
            ),
        ] {
            let output = Rc::new(RefCell::new(vec![0; width]));
            let mut request = submit_values(
                &vm.domains,
                &selected,
                vm.current.as_ref().unwrap(),
                entry,
                input,
                ValueOutput {
                    words: output.clone(),
                    snapshot: false,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            vm.poll_tasks(10000).unwrap();
            let (outcome, rejection) = request.take_access_result().unwrap();
            assert_eq!(rejection, None, "{mode:?} {entry}: {outcome:?}");
            if let Some(expected) = expected {
                assert!(matches!(outcome, DomainOutcome::Value(_)), "{outcome:?}");
                assert_eq!(*output.borrow(), expected);
            } else {
                assert!(matches!(outcome, DomainOutcome::Failed(_)));
            }
            assert_eq!(vm.domain_revision(counter.read_handle()).unwrap(), revision);
            assert_eq!(vm.domain_revision(data.read_handle()).unwrap(), 0);
            let domains = vm.domains.borrow();
            let root = domains
                .get(data.read_handle())
                .unwrap()
                .root
                .as_ref()
                .unwrap();
            assert_eq!(root.scalar_pointer(), pointer);
            assert_eq!(budget.bytes.get(), baseline);
            assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), live);
        }
        budget.limit.set(DomainLimits::default().max_state_bytes);
        for (handle, method, expected) in [
            (data.read_handle(), "size", 16384),
            (counter.read_handle(), "read", 16383),
        ] {
            let mut request = vm.queue_domain_call(handle, method, &[]).unwrap();
            vm.poll_tasks(10000).unwrap();
            assert!(
                matches!(request.take_result(),Some(DomainOutcome::Value(InstanceValue::Int32(n))) if n==expected)
            );
        }
        drop(data);
        drop(counter);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
