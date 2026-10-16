use super::*;

const SOURCE: &str = r#"
import joky/domain
struct S{n:Int32}
enum Input<T>{Skip,Add(T)}
enum Failure{Denied,Code(Int32)}
enum Output{Accepted(Int32),Rejected(Failure)}
fn make(n:Int32)->S{S{n:n}}
fn read(s:&S)->Int32{s.n}
fn apply(a:&mut S,b:&mut S,input:Input<Int32>)->Output {
    match input {
        Skip=>Output.Rejected(Failure.Denied),
        Add(n)=>{a.n+=int32(n) b.n+=int32(n) Output.Accepted(a.n)}
    }
}
fn operation(a:domain.WriteHandle<S>,b:domain.WriteHandle<S>,input:Input<Int32>) @suspends ->Output {
    when "update" (mut a,mut b) {apply(a,b,input)}.unwrap()
}
"#;
const ENTRY: &str = "$when/named/4:main/9:operation/6:757064617465";

#[test]
fn pure_enum_domain_queued_calls_rebind_or_reject_before_writing() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for change in [
            "algorithm",
            "input_order",
            "input_name",
            "input_payload",
            "input_argument",
            "added_variant",
            "result_order",
            "result_payload",
        ] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(10)])
                .unwrap();
            let b = vm
                .create_domain("make", &[InstanceValue::Int32(20)])
                .unwrap();
            let old = vm.current.as_ref().unwrap().clone();
            let output = Rc::new(RefCell::new(vec![0; 4]));
            let mut request = pairs::submit_values(
                &vm.domains,
                &[a.write_handle(), b.write_handle()],
                &old,
                ENTRY,
                vec![1, 2],
                pairs::ValueOutput {
                    words: output.clone(),
                    snapshot: false,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            let next = match change {
                "algorithm" => SOURCE.replace("+=int32(n)", "+=int32(n)*10"),
                "input_order" => SOURCE.replace("Skip,Add(T)", "Add(T),Skip"),
                "input_name" => SOURCE.replace("Input", "Request"),
                "input_payload" => SOURCE.replace("Add(T)", "Add(Int64)"),
                "input_argument" => SOURCE.replace("Input<Int32>", "Input<UInt32>"),
                "added_variant" => SOURCE.replace("Skip,Add(T)", "Skip,Add(T),Extra").replace("Skip=>Output.Rejected(Failure.Denied),", "Skip=>Output.Rejected(Failure.Denied), Extra=>Output.Rejected(Failure.Denied),"),
                "result_order" => SOURCE.replace(
                    "Accepted(Int32),Rejected(Failure)",
                    "Rejected(Failure),Accepted(Int32)",
                ),
                "result_payload" => SOURCE.replace("Code(Int32)", "Code(UInt32)"),
                _ => unreachable!(),
            };
            for next in [&next, &next.replace("*10", "*100")] {
                vm.load_source(next)
                    .unwrap_or_else(|e| panic!("{change}: {e}"));
                assert_eq!(vm.publish_domain_code().unwrap(), 2);
            }
            vm.poll_tasks(10000).unwrap();
            let (_, rejection) = request.take_access_result().unwrap();
            if change == "algorithm" {
                assert_eq!(rejection, None);
                assert_eq!(*output.borrow(), [0, 210, 0, 0]);
                read(&mut vm, a.read_handle(), 210);
                read(&mut vm, b.read_handle(), 220);
            } else {
                assert_eq!(rejection, Some(access::Rejection::Incompatible), "{change}");
                assert_eq!(*output.borrow(), [0; 4]);
                read(&mut vm, a.read_handle(), 10);
                read(&mut vm, b.read_handle(), 20);
                assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
                assert_eq!(vm.domain_revision(b.read_handle()).unwrap(), 0);
                assert!(pairs::submit_values(
                    &vm.domains,
                    &[a.write_handle(), b.write_handle()],
                    &old,
                    ENTRY,
                    vec![1, 2],
                    pairs::ValueOutput {
                        words: output.clone(),
                        snapshot: false
                    },
                    vm.interpreter_limits()
                )
                .is_err());
            }
            a.close();
            b.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}

#[test]
fn pure_enum_domain_results_reject_malformed_tags_and_inactive_storage() {
    let mut vm = VmEngine::new();
    vm.load_source(SOURCE).unwrap();
    let ty = vm.function_signature("operation").unwrap().ret;
    assert_eq!(result_words(&ty, vec![0, 42, 0, 0]).unwrap(), [0, 42, 0, 0]);
    assert_eq!(result_words(&ty, vec![1, 0, 1, 7]).unwrap(), [1, 0, 1, 7]);
    for words in [
        vec![2, 0, 0, 0],
        vec![0, 42, 1, 0],
        vec![1, 0, 2, 0],
        vec![1, 0, 0, 1],
    ] {
        assert!(result_words(&ty, words).is_err());
    }
}
