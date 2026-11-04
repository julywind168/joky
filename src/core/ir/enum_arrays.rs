//! Compact storage for scalar enum elements in closed, by-value buffer uses.
use super::*;
use crate::{core::types::EnumType, parser::ast::BufferIntrinsic as BI};
use std::collections::{hash_map::Entry, HashSet};

fn by_value(op: BI, element: &EnumType, args: &[Slot], dst: Option<&Slot>) -> bool {
    // An identity-only nominal view cannot supply the logical payload lanes.
    let concrete =
        |slot: &Slot| matches!(&slot.ty, Type::Enum(e) if !e.indirect && e.id == element.id);
    match op {
        BI::Store | BI::SliceStore | BI::InitializeNext => args.last().is_some_and(concrete),
        BI::Load | BI::SliceLoad | BI::TakeLast => dst.is_some_and(concrete),
        BI::Allocate
        | BI::CapacityLimit
        | BI::Len
        | BI::Capacity
        | BI::Transfer
        | BI::Replace
        | BI::Release
        | BI::SwapElements
        | BI::Truncate
        | BI::Slice
        | BI::SliceMut
        | BI::Subslice
        | BI::SubsliceMut
        | BI::SliceLen => true,
        _ => false,
    }
}

pub(super) fn candidate(e: &EnumType) -> bool {
    let payloads = e.variants.iter().filter_map(|v| v.payload.as_ref());
    // Bound the generated tag dispatch and only specialize concrete nominal types.
    !e.indirect
        && e.parameters.is_empty()
        && e.arguments.is_empty()
        && e.template.is_none()
        && e.variants.len() <= 32
        && e.owners.is_empty()
        && payloads.clone().all(crate::core::instance::scalar)
        && payloads.filter(|ty| **ty != Type::Unit).nth(1).is_some()
}

fn packed_type(e: &EnumType, registry: &TypeRegistry) -> Result<Type, Error> {
    let width = e
        .variants
        .iter()
        .filter_map(|v| v.payload.as_ref())
        .map(|p| p.memory_layout().expect("scalar payload layout").size)
        .max()
        .expect("nonempty payloads");
    let carrier = match width {
        1 => Type::UInt8,
        2 => Type::UInt16,
        4 => Type::UInt32,
        8 => Type::UInt64,
        _ => unreachable!("scalar payload width"),
    };
    crate::core::types::tuple_type(vec![Type::Int32, carrier], registry)
}

fn closed_signature(info: &FunctionInfo) -> bool {
    use crate::core::types::walk::{TypeEdges, TypeWalk};
    info.signature
        .params
        .iter()
        .chain([&info.signature.ret])
        .all(|ty| {
            TypeWalk::new(ty, TypeEdges::Inline).all(|part| match part {
                Type::Record(r) => !r.indirect && r.closure.is_none() && r.drop_anchor().is_none(),
                Type::Enum(e) => !e.indirect,
                Type::Tuple(_) | Type::String => true,
                _ => crate::core::instance::scalar(part),
            })
        })
}

// No array handle or view can arrive from another compilation or be published
// to a host, task snapshot or persistent root. Ordinary direct calls inside the
// plan use the same decision for an enum, including across source modules.
// FixedArray views (PlaceOp::View) and external slice inputs are excluded:
// admitted slices inherit their stride from a buffer allocated in this plan.
fn closed_module(
    functions: &HashMap<String, FunctionInfo>,
    bodies: &HashMap<String, FunctionPlan>,
) -> bool {
    bodies.iter().all(|(name, body)| {
        let info = &functions[name];
        let external_entry = !info.internal
            || info.instance_entry
            || info.domain_entry
            || info.domain_request_entry
            || info.script_entry
            || info.parallel_entry;
        if info.suspends || external_entry && !closed_signature(info) {
            return false;
        }
        body.ops.iter().all(|op| match op {
            Op::Call {
                name,
                indirect: None,
                state: None,
                ..
            } => bodies.contains_key(name) || closed_signature(&functions[name]),
            Op::Buffer(BufferOp::Storage { intrinsic, .. }) => !intrinsic.is_pair(),
            Op::Cleanup(CleanupOp::Register {
                hooks, elements, ..
            }) => hooks.is_empty() && elements.iter().all(|entry| entry.hooks.is_empty()),
            Op::Buffer(_)
            | Op::Local { .. }
            | Op::ReadMemory { .. }
            | Op::WriteMemory { .. }
            | Op::ReadOwner { .. }
            | Op::StoreOwner { .. }
            | Op::Project { .. }
            | Op::PatternProject { .. }
            | Op::Alloc { .. }
            | Op::Copy(..)
            | Op::Reference { .. }
            | Op::Reserve { .. }
            | Op::Label(_)
            | Op::Jump(_)
            | Op::Branch { .. }
            | Op::Literal(..)
            | Op::Text { .. }
            | Op::Assert { .. }
            | Op::Panic { .. }
            | Op::Binary(..)
            | Op::Unary(..)
            | Op::Convert(..)
            | Op::Float { .. } => true,
            _ => false,
        })
    })
}

pub(super) fn lower(
    functions: &HashMap<String, FunctionInfo>,
    bodies: &mut HashMap<String, FunctionPlan>,
) -> Result<(), Error> {
    let mut elements = HashMap::new();
    let mut excluded = HashSet::new();
    for body in bodies.values() {
        for op in &body.ops {
            let Op::Buffer(BufferOp::Storage {
                intrinsic,
                element: element @ Type::Enum(e),
                args,
                dst,
                ..
            }) = op
            else {
                continue;
            };
            // A single addressable use requires the canonical stride at every
            // allocation and access of this nominal enum in the compilation.
            if e.indirect || !by_value(*intrinsic, e, args, dst.as_ref()) {
                excluded.insert(e.id);
            } else if *intrinsic == BI::Allocate && candidate(e) {
                if let Entry::Vacant(entry) = elements.entry(e.id) {
                    let physical = packed_type(e, &body.type_registry)?;
                    if physical.storage_layout().unwrap().size
                        < element.storage_layout().unwrap().size
                    {
                        entry.insert(physical);
                    }
                }
            }
        }
    }
    elements.retain(|id, _| !excluded.contains(id));
    if elements.is_empty() || !closed_module(functions, bodies) {
        return Ok(());
    }
    let selected = |op: &Op| {
        matches!(op, Op::Buffer(BufferOp::Storage {
        element: Type::Enum(e), ..}) if elements.contains_key(&e.id))
    };
    for body in bodies.values_mut().filter(|b| b.ops.iter().any(selected)) {
        let spans = std::mem::take(&mut body.op_spans);
        for (op, span) in std::mem::take(&mut body.ops).into_iter().zip(spans) {
            if !selected(&op) {
                body.ops.push(op);
                body.op_spans.push(span);
                continue;
            }
            body.current_span = span;
            let Op::Buffer(BufferOp::Storage {
                intrinsic,
                element: Type::Enum(e),
                mut args,
                mut dst,
                scratch,
            }) = op
            else {
                unreachable!()
            };
            let physical = &elements[&e.id];
            let mut unpack = None;
            match intrinsic {
                BI::InitializeNext | BI::Store | BI::SliceStore => {
                    let value = args.last_mut().unwrap();
                    let packed = body.slot(physical.clone())?;
                    convert(body, value, &packed, Direction::Pack)?;
                    *value = packed;
                }
                BI::Load | BI::SliceLoad | BI::TakeLast => {
                    let packed = body.slot(physical.clone())?;
                    unpack = Some((dst.replace(packed.clone()).unwrap(), packed));
                }
                _ => {}
            }
            body.emit(Op::Buffer(BufferOp::Storage {
                intrinsic,
                element: physical.clone(),
                args,
                dst,
                scratch,
            }));
            if let Some((logical, packed)) = unpack {
                convert(body, &logical, &packed, Direction::Unpack)?;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Direction {
    Pack,
    Unpack,
}

fn convert(
    body: &mut FunctionPlan,
    logical: &Slot,
    packed: &Slot,
    direction: Direction,
) -> Result<(), Error> {
    let Type::Enum(e) = &logical.ty else {
        unreachable!()
    };
    let mut compact = packed.scalars();
    let packed_tag = compact.next().unwrap();
    let payload = compact.next().unwrap();
    let lanes: Vec<_> = logical.scalars().collect();
    let tag = lanes[0].clone();
    let bits = body.slot(Type::UInt64)?;
    let zero = body.slot(Type::UInt64)?;
    body.emit(Op::Literal(zero.clone(), Constant::UInt64(0)));
    match direction {
        Direction::Pack => {
            body.emit(Op::Copy(packed_tag, tag.clone()));
            body.emit(Op::Convert(payload.clone(), zero));
        }
        Direction::Unpack => {
            body.emit(Op::Copy(tag.clone(), packed_tag));
            body.emit(Op::Convert(bits.clone(), payload.clone()));
            for lane in &lanes[1..] {
                body.emit(Op::DomainBits {
                    dst: lane.clone(),
                    src: zero.clone(),
                    decode: true,
                });
            }
        }
    }
    let exit = body.label_count;
    body.label_count += 1;
    for (index, variant) in e.variants.iter().enumerate() {
        if variant.payload.as_ref().is_none_or(|ty| *ty == Type::Unit) {
            continue;
        }
        let next = body.label_count;
        body.label_count += 1;
        body.literal_pattern_test(tag.clone(), Constant::Int32(index as i32), next)?;
        let lane = lanes[variant.offset].clone();
        match direction {
            Direction::Pack => {
                body.emit(Op::DomainBits {
                    dst: bits.clone(),
                    src: lane,
                    decode: false,
                });
                body.emit(Op::Convert(payload.clone(), bits.clone()));
            }
            Direction::Unpack => {
                body.emit(Op::DomainBits {
                    dst: lane,
                    src: bits.clone(),
                    decode: true,
                });
            }
        }
        body.emit(Op::Jump(exit));
        body.emit(Op::Label(next));
    }
    body.emit(Op::Jump(exit));
    body.emit(Op::Label(exit));
    Ok(())
}

#[test]
fn compact_enum_array_stride_and_borrow_fallback() {
    use crate::vm::{VmEngine, VmMode};
    let source = include_str!("../../../tests/fixtures/compact_enum_array.jk");
    for (packed, extra) in [
        (true, ""),
        (false, "fn borrowed() -> Int32 { var xs = [ProbeWord.A(0)]; let p = &mut xs[0]; *p = ProbeWord.D(39); read(xs[0]) }"),
    ] {
        // At this capacity a one-byte change in actual stride dwarfs the
        // buffer header. Bracket the limit on both sides to distinguish every
        // integer stride, using real allocations in all three backends.
        let stride = if packed { 8 } else { 20 };
        let capacity = crate::runtime::arena::MAX_BYTES / stride;
        let fixture = format!(
            "{source}\n{extra}\nfn fits() -> Int32 {{ reserve_words({}) }}\nfn exceeds() -> Int32 {{ reserve_words({}) }}",
            capacity - 1024, capacity + 1
        );
        let ast = crate::TypeRegistry::default()
            .resolve_aliases(&crate::parser::parse(&fixture).unwrap())
            .unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_module(&ast).unwrap();
            if !packed {
                assert_eq!(vm.call_int32_0("borrowed").unwrap(), 42);
            }
            let error = vm.call_int32_0("bad_index").unwrap_err();
            assert!(error.message().contains("outside the initialized prefix"));
            assert_eq!(error.span().unwrap().slice(&fixture), Some("words[1]"));
            assert_eq!(vm.call_int32_0("roundtrip").unwrap(), 42);
            assert_eq!(
                vm.call_int32_0("fits").unwrap(),
                42,
                "{mode:?}, stride {stride}"
            );
            assert_eq!(
                vm.call_int32_0("exceeds").unwrap_err().code(),
                ErrorCode::ResourceLimit
            );
        }
        #[cfg(unix)]
        crate::backends::aot::tests::assert_sync_results(
            &ast,
            &[
                ("roundtrip", Some(42)),
                ("bad_index", None),
                ("fits", Some(42)),
                ("exceeds", None),
            ],
        );
    }
}

#[test]
fn compact_enum_arrays_preserve_mixed_slice_origins() {
    use crate::vm::{VmEngine, VmMode};
    let fixture = format!(
        "import joky/fixed_array.{{type FixedArray}}\nimport joky/slice.{{type Slice}}\n{}\n{}",
        include_str!("../../../tests/fixtures/compact_enum_array.jk"),
        r#"
        fn second(xs: &Slice<ProbeWord>) -> Int32 { read(xs[1]) }
        fn both() -> Int32 {
            let xs = [ProbeWord.A(1), ProbeWord.C(18)]
            let ys: FixedArray<ProbeWord, 2> = [ProbeWord.A(2), ProbeWord.D(19)]
            second(&xs[0..2]) + second(&ys[0..2])
        }
        fn large() -> Int32 { reserve_words(4000000) }
        "#
    );
    let ast = crate::TypeRegistry::default()
        .resolve_aliases(&crate::parser::parse(&fixture).unwrap())
        .unwrap();
    for mode in [VmMode::Interpreter, VmMode::Jit] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        assert_eq!(vm.call_int32_0("both").unwrap(), 42);
        assert_eq!(
            vm.call_int32_0("large").unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_results(&ast, &[("both", Some(42)), ("large", None)]);
}

#[test]
fn compact_enum_arrays_keep_the_persistent_instance_layout() {
    use crate::vm::{InstanceValue, VmEngine, VmMode};
    let source = format!(
        "{}\n{}",
        include_str!("../../../tests/fixtures/compact_enum_array.jk"),
        r#"
        struct Root { words: Array<ProbeWord> }
        fn make() -> Root { Root { words: [ProbeWord.B(41)] } }
        fn value(root: &Root) -> Int32 { read(root.words.get(0)!) }
        fn large() -> Int32 { reserve_words(4000000) }
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap();
        let id = vm.create_instance("make", &[]).unwrap();
        assert_eq!(
            vm.call_instance(id, "value", &[]).unwrap(),
            InstanceValue::Int32(42)
        );
        vm.close_instance(id).unwrap();
        assert_eq!(
            vm.call_int32_0("large").unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
    }
}
