use super::*;

const SOURCE: &str = r#"
import joky/domain
struct S{n:Int32}
struct Marker<T>{}
struct Input<T>{left:T,right:Int32,marker:Marker<Bool>}
struct Reply<T>{left:T,right:T}
fn make(n:Int32)->S{S{n:n}}
fn read(s:&S)->Int32{s.n}
fn apply(a:&mut S,b:&mut S,input:Input<(Int32,UInt32)>)->(Reply<Int32>,Unit) {
    a.n+=(int32(input.left.0)+int32(input.left.1))*1
    b.n+=input.right*1
    (Reply{left:a.n,right:b.n},{})
}
fn operation(a:domain.WriteHandle<S>,b:domain.WriteHandle<S>,input:Input<(Int32,UInt32)>) @suspends ->(Reply<Int32>,Unit) {
    when "update" (mut a,mut b) {apply(a,b,input)}.unwrap()
}
"#;
const ENTRY: &str = "$when/named/4:main/9:operation/6:757064617465";

#[test]
fn pure_composite_domain_queued_calls_rebind_or_reject_before_writing() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for change in [
            "algorithm",
            "tuple_order",
            "tuple_type",
            "field_order",
            "field_name",
            "privacy",
            "nominal",
            "phantom",
            "result_order",
            "result_tuple",
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
            let output = Rc::new(RefCell::new(vec![0; 2]));
            let mut request = pairs::submit_values(
                &vm.domains,
                &[a.write_handle(), b.write_handle()],
                &old,
                ENTRY,
                vec![2, 3, 4],
                pairs::ValueOutput {
                    words: output.clone(),
                    snapshot: false,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            let next = match change {
                "algorithm" => SOURCE.replace("*1", "*10"),
                "tuple_order" => SOURCE.replace("Int32,UInt32", "UInt32,Int32"),
                "tuple_type" => SOURCE.replace("Int32,UInt32", "Int32,Int32"),
                "field_order" => SOURCE.replace("left:T,right:Int32", "right:Int32,left:T"),
                "field_name" => SOURCE.replace("marker", "tag"),
                "privacy" => SOURCE.replace("left:T,right:Int32", "pub left:T,right:Int32"),
                "nominal" => SOURCE.replace("Input", "Command"),
                "phantom" => SOURCE.replace("Marker<Bool>", "Marker<UInt32>"),
                "result_order" => SOURCE.replace("left:T,right:T", "right:T,left:T"),
                "result_tuple" => SOURCE
                    .replace("Reply<Int32>,Unit", "Unit,Reply<Int32>")
                    .replace(
                        "(Reply{left:a.n,right:b.n},{})",
                        "({},Reply{left:a.n,right:b.n})",
                    ),
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
                assert_eq!(*output.borrow(), [510, 420]);
                read(&mut vm, a.read_handle(), 510);
                read(&mut vm, b.read_handle(), 420);
            } else {
                assert_eq!(rejection, Some(access::Rejection::Incompatible), "{change}");
                assert_eq!(*output.borrow(), [0; 2]);
                read(&mut vm, a.read_handle(), 10);
                read(&mut vm, b.read_handle(), 20);
                assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
                assert_eq!(vm.domain_revision(b.read_handle()).unwrap(), 0);
                assert!(pairs::submit_values(
                    &vm.domains,
                    &[a.write_handle(), b.write_handle()],
                    &old,
                    ENTRY,
                    vec![2, 3, 4],
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
