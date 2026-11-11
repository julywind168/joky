use super::*;
use crate::core::types::{memory_layout::MAX_DEPTH, ConstArgument, FixedArrayType};
use crate::vm::{VmEngine, VmMode};
use crate::Type;
use std::sync::Arc;

#[test]
fn place_traversal_maximum_depth_on_small_stack() {
    // Isolate regressions that exhaust the native thread stack from libtest.
    const CHILD: &str = "JOKY_PLACE_TRAVERSAL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let name = std::thread::current().name().unwrap().to_owned();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    let mut ty = Type::String;
    for _ in 0..MAX_DEPTH {
        ty = Type::FixedArray(Arc::new(FixedArrayType {
            element: ty,
            length: ConstArgument::Value(1),
        }));
    }
    // Build the actual compiler descriptor before entering the small stack.
    let nodes = ty.place_copy_program();
    assert_eq!(nodes.len(), MAX_DEPTH + 1);
    std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(move || unsafe {
            let mut arena = Arena::default();
            let owner = super::super::buffer::Buffer::copy_bytes(&mut arena, b"owned")
                .unwrap()
                .as_ptr() as usize;
            let mut source = owner;
            let mut destination = 0usize;
            let src = (&mut source as *mut usize).cast::<u8>();
            let dst = (&mut destination as *mut usize).cast::<u8>();
            copy_fields(dst, src, &nodes, false);
            assert_eq!(source, owner, "snapshot must retain the original owner");
            assert_eq!(destination, owner);
            copy(dst, src, &nodes);
            assert_eq!(source, 0, "move must clear the source lane");
            copy(dst, dst, &nodes);
            assert_eq!(destination, owner, "self-move must preserve ownership");
            let mut visits = 0;
            owners(dst, &nodes, &mut |lane| {
                visits += 1;
                assert_eq!(lane.read_unaligned(), owner);
            });
            assert_eq!(visits, 1);
            // Failure cleanup must not need another task allocation.
            let old = super::super::arena::LIMIT.with(|limit| limit.replace(0));
            dispose(&mut arena, dst, &nodes).unwrap();
            dispose(&mut arena, src, &nodes).unwrap();
            dispose(&mut arena, dst, &nodes).unwrap();
            super::super::arena::LIMIT.with(|limit| limit.set(old));
            assert_eq!(destination, 0);
            super::super::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn place_traversal_nested_moves_and_partial_cleanup_across_backends() {
    // Records require sibling continuations, arrays require element cursors,
    // and enums must skip inactive payloads. Keep all three in one value.
    let mut source = String::from(
        r#"
        import joky/fixed_array.{type FixedArray}
        import joky/string
        enum Leaf { Empty, Text(String) }
        struct Node0 { rows:FixedArray<Leaf,3>, empty:FixedArray<String,0> }
        fn tail(level:Int32, fail:Int32) -> String {
            if level == fail { panic("partial place") }
            "tail"
        }
        "#,
    );
    const DEPTH: usize = 16;
    for i in 1..=DEPTH {
        source.push_str(&format!(
            "struct Node{i} {{ before:Int32, items:FixedArray<Node{},1>, after:String }}\n",
            i - 1
        ));
    }
    source.push_str(&format!("fn make(fail:Int32) -> Node{DEPTH} {{\n"));
    source.push_str(
        "let v0=Node0 { rows:[Leaf.Text(\"hello\"),Leaf.Empty,Leaf.Text(\"world\")], empty:[] }\n",
    );
    for i in 1..=DEPTH {
        source.push_str(&format!(
            "let v{i}=Node{i} {{ before:{i}, items:[v{}], after:tail({i},fail) }}\n",
            i - 1
        ));
    }
    source.push_str(&format!(
        "v{DEPTH}\n}}\nfn consume(v{DEPTH}:Node{DEPTH}) -> Int32 {{\n"
    ));
    for i in (1..=DEPTH).rev() {
        source.push_str(&format!(
            "let Node{i} {{ before:b{i}, items:a{i}, after:t{i} }}=v{i}\nassert_eq(b{i},{i})\nassert_eq(t{i}.byte_length(),4)\nlet [v{}]=a{i}\n",
            i - 1
        ));
    }
    source.push_str(
        r#"
        let Node0 { rows, empty }=v0
        assert_eq(empty.len(),0)
        let [a,b,c]=rows
        match b { Leaf.Empty=>{}, Leaf.Text(text)=>{ panic("inactive variant") } }
        match a { Leaf.Text(text)=>int32(text.byte_length()), Leaf.Empty=>0 } +
        match c { Leaf.Text(text)=>int32(text.byte_length()), Leaf.Empty=>0 }
        }
        fn success() -> Int32 { var value=make(0); value=make(0); let f=consume; f(value) }
        fn partial() -> Int32 { consume(make(8)) }
        fn replacement_fail() -> Int32 { var value=make(0); value=make(8); consume(value) }
        "#,
    );
    let cases = [
        ("success", Some(10)),
        ("partial", None),
        ("replacement_fail", None),
        ("success", Some(10)),
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source)
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        for (name, expected) in cases {
            let result = vm.call_int32_0(name);
            match expected {
                Some(expected) => assert_eq!(result.unwrap(), expected, "{mode:?} {name}"),
                None => assert_eq!(result.unwrap_err().message(), "partial place"),
            }
            super::super::arena::LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?} {name}"));
        }
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_results(
        &crate::parser::parse(&source).unwrap(),
        &cases,
    );
}
