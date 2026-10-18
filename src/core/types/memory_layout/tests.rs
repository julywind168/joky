use super::*;
use crate::parser::ast::{Item, Type};

fn return_type(source: &str) -> Type {
    let registry = crate::TypeRegistry::new().with_prelude(false);
    let module = registry
        .resolve_aliases(&crate::parser::parse(source).unwrap())
        .unwrap();
    let Item::FnDef(function) = &module.items[0] else {
        panic!("expected function")
    };
    function.ret_ty.clone()
}

#[test]
fn record_offsets_are_bytes_and_scalar_slots_remain_separate() {
    let ty = return_type("struct CPoint { x: Int32, y: Int64 } fn f(x: CPoint) -> CPoint { x }");
    let Type::Record(record) = &ty else {
        panic!("expected record")
    };
    assert_eq!(record.fields[0].offset, 0);
    assert_eq!(record.fields[1].offset, 1);
    assert_eq!(record.scalars, vec![Type::Int32, Type::Int64]);

    let layout = ty.memory_layout_for(MemoryLayoutTarget::default()).unwrap();
    assert_eq!((layout.size, layout.align), (16, 8));
    let MemoryLayoutShape::Record(fields) = layout.shape else {
        panic!("expected record layout")
    };
    assert_eq!(
        fields[0],
        FieldMemoryLayout {
            offset: 0,
            size: 4,
            align: 4
        }
    );
    assert_eq!(
        fields[1],
        FieldMemoryLayout {
            offset: 8,
            size: 8,
            align: 8
        }
    );
}

#[test]
fn nested_records_tuples_and_bool_use_target_alignment() {
    let record = return_type(
        "struct Inner { flag: Bool, value: Int32 } struct Outer { head: Int64, inner: Inner, tail: Bool } fn f(x: Outer) -> Outer { x }",
    );
    let layout = record
        .memory_layout_for(MemoryLayoutTarget::default())
        .unwrap();
    assert_eq!((layout.size, layout.align), (24, 8));
    let MemoryLayoutShape::Record(fields) = layout.shape else {
        panic!("expected record layout")
    };
    assert_eq!(
        fields.iter().map(|field| field.offset).collect::<Vec<_>>(),
        [0, 8, 16]
    );

    let tuple = Type::tuple(vec![Type::Bool, Type::Int64, Type::Int32]);
    let layout = tuple
        .memory_layout_for(MemoryLayoutTarget::default())
        .unwrap();
    assert_eq!((layout.size, layout.align), (24, 8));
    let MemoryLayoutShape::Tuple(elements) = layout.shape else {
        panic!("expected tuple layout")
    };
    assert_eq!(
        elements
            .iter()
            .map(|field| field.offset)
            .collect::<Vec<_>>(),
        [0, 8, 16]
    );
}

#[test]
fn enum_layout_has_int32_tag_and_disjoint_payloads() {
    let ty = return_type(
        "enum Value { Empty, Number(Int32), Wide(Int64) } fn f(x: Value) -> Value { x }",
    );
    let layout = ty.memory_layout_for(MemoryLayoutTarget::default()).unwrap();
    assert_eq!((layout.size, layout.align), (16, 8));
    let MemoryLayoutShape::Enum { tag, variants } = layout.shape else {
        panic!("expected enum layout")
    };
    assert_eq!(
        tag,
        FieldMemoryLayout {
            offset: 0,
            size: 4,
            align: 4
        }
    );
    assert_eq!(variants[0].payload, None);
    assert_eq!(variants[1].payload.unwrap().offset, 4);
    assert_eq!(variants[2].payload.unwrap().offset, 8);
}

#[test]
fn unit_and_unsupported_values_are_explicit() {
    let empty = return_type("struct Empty {} fn f(x: Empty) -> Empty { x }");
    assert_eq!(
        empty
            .memory_layout_for(MemoryLayoutTarget::default())
            .unwrap()
            .size,
        0
    );
    let unit_field = return_type("struct R { unit: Unit, value: Int32 } fn f(x: R) -> R { x }");
    let layout = unit_field
        .memory_layout_for(MemoryLayoutTarget::default())
        .unwrap();
    let MemoryLayoutShape::Record(fields) = layout.shape else {
        panic!("expected record")
    };
    assert_eq!(fields[0].size, 0);
    assert_eq!(fields[1].offset, 0);
    assert!(matches!(
        Type::String.memory_layout_for(MemoryLayoutTarget::default()),
        Err(MemoryLayoutError::UnsupportedType)
    ));
}

#[test]
fn target_limits_and_overflow_are_checked() {
    let target = MemoryLayoutTarget {
        int32_align: 2,
        int64_align: 4,
        max_size: 4,
        ..MemoryLayoutTarget::default()
    };
    assert_eq!(Type::Int32.memory_layout_for(target).unwrap().align, 2);
    assert_eq!(Type::Bool.memory_layout_for(target).unwrap().size, 1);
    assert!(matches!(
        Type::Int64.memory_layout_for(target),
        Err(MemoryLayoutError::ResourceLimit)
    ));
    assert!(matches!(
        checked_align_up(usize::MAX, 8, usize::MAX),
        Err(MemoryLayoutError::Overflow)
    ));
    assert!(matches!(
        Type::Int32.memory_layout_for(MemoryLayoutTarget {
            int32_align: 3,
            ..target
        }),
        Err(MemoryLayoutError::InvalidTarget)
    ));
}

#[test]
fn zero_sized_member_count_is_still_bounded() {
    let ty = Type::tuple(vec![Type::Unit; MAX_MEMBERS + 1]);
    assert!(matches!(
        ty.memory_layout_for(MemoryLayoutTarget::default()),
        Err(MemoryLayoutError::ResourceLimit)
    ));
}

#[test]
fn concrete_generic_instances_use_their_substituted_layout() {
    let ty = return_type(
        "struct Pair<T> { first: T, second: T } fn f(x: Pair<Int64>) -> Pair<Int64> { x }",
    );
    let layout = ty.memory_layout().unwrap();
    assert_eq!((layout.size, layout.align), (16, 8));
    let MemoryLayoutShape::Record(fields) = layout.shape else {
        panic!("expected record layout")
    };
    assert_eq!([fields[0].offset, fields[1].offset], [0, 8]);
}

#[test]
fn shared_zero_sized_dags_are_cached_and_depth_is_bounded() {
    let mut shallow = Type::Unit;
    for _ in 0..(MAX_DEPTH - 1) {
        shallow = Type::tuple(vec![shallow.clone(), shallow]);
    }
    assert_eq!(shallow.memory_layout().unwrap().size, 0);

    let too_deep = Type::tuple(vec![Type::tuple(vec![shallow])]);
    assert!(matches!(
        too_deep.memory_layout(),
        Err(MemoryLayoutError::ResourceLimit)
    ));
}

#[test]
fn target_changes_offsets_and_tail_padding_counts_toward_size_limit() {
    let ty = Type::tuple(vec![Type::Bool, Type::UInt64, Type::UInt32]);
    let target = MemoryLayoutTarget {
        int64_align: 4,
        ..MemoryLayoutTarget::default()
    };
    let layout = ty.memory_layout_for(target).unwrap();
    assert_eq!((layout.size, layout.align), (16, 4));
    let MemoryLayoutShape::Tuple(fields) = layout.shape else {
        panic!("expected tuple")
    };
    assert_eq!(
        fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
        [0, 4, 12]
    );
    assert_eq!(ty.memory_layout().unwrap().size, 24);
    let padded = Type::tuple(vec![Type::UInt64, Type::Bool]);
    assert_eq!(
        padded.memory_layout_for(MemoryLayoutTarget {
            max_size: 9,
            ..target
        }),
        Err(MemoryLayoutError::ResourceLimit)
    );
    assert_eq!(
        padded
            .memory_layout_for(MemoryLayoutTarget {
                max_size: 12,
                ..target
            })
            .unwrap()
            .size,
        12
    );
}

#[test]
fn cached_subtree_cannot_bypass_depth_limit() {
    let mut shared = Type::Unit;
    for _ in 0..128 {
        shared = Type::tuple(vec![shared]);
    }
    let mut deep = shared.clone();
    for _ in 0..128 {
        deep = Type::tuple(vec![deep]);
    }
    let root = Type::tuple(vec![shared, deep]);
    assert_eq!(root.memory_layout(), Err(MemoryLayoutError::ResourceLimit));
}

#[test]
fn reference_fields_have_target_pointer_layout_without_recursive_expansion() {
    let registry = crate::TypeRegistry::new();
    let module = registry
        .resolve_aliases(
            &crate::parser::parse(
                "struct Node { value: Int32, next: Option<&Node> } fn view(p: &Node) {}",
            )
            .unwrap(),
        )
        .unwrap();
    let nominals = crate::core::types::nominal_types(&registry, &module).unwrap();
    let node = &nominals["Node"];
    for width in [4, 8] {
        let target = MemoryLayoutTarget {
            pointer_size: width,
            pointer_align: width,
            ..MemoryLayoutTarget::default()
        };
        let layout = node.memory_layout_for(target).unwrap();
        assert_eq!(layout.align, width);
        assert_eq!(layout.size, if width == 4 { 12 } else { 24 });
    }
    assert!(matches!(
        node.memory_layout_for(MemoryLayoutTarget {
            pointer_size: 3,
            ..MemoryLayoutTarget::default()
        }),
        Err(MemoryLayoutError::InvalidTarget)
    ));
}

#[test]
fn layout_queries_reject_dyn_inside_signatures_buffers_and_phantom_arguments() {
    let view = return_type(
        "trait Read { fn get(&self) @noescape -> Int32 } fn f(x: &dyn Read) -> &dyn Read { x }",
    );
    let mut phantom = return_type("struct Empty<T> {} fn f(x: Empty<Int32>) -> Empty<Int32> { x }");
    let Type::Record(record) = &mut phantom else {
        panic!("expected record")
    };
    Arc::get_mut(record)
        .expect("unshared test record")
        .arguments = vec![view.clone().into()];
    for ty in [
        view.clone(),
        Type::function(vec![view.clone()], Type::Unit),
        Type::Buffer(Arc::new(view.clone())),
        Type::tuple(vec![Type::Unit, view]),
        phantom,
    ] {
        assert_eq!(ty.memory_layout(), Err(MemoryLayoutError::UnsupportedType));
        assert_eq!(ty.storage_layout(), Err(MemoryLayoutError::UnsupportedType));
        assert_eq!(ty.memory_scalars(), Err(MemoryLayoutError::UnsupportedType));
        assert_eq!(
            ty.storage_scalars(),
            Err(MemoryLayoutError::UnsupportedType)
        );
    }
}
