//! Closed fixture for the D2 layout experiment. Only by-value Array accesses
//! are admitted: a raw element borrow or parallel snapshot needs a different
//! contract. No normal compilation can select this representation.
use super::*;
use crate::parser::ast::BufferIntrinsic as BI;
use std::cell::Cell;

thread_local! { static ENABLED: Cell<bool> = const { Cell::new(false) }; }

fn selected(op: &Op) -> bool {
    let Op::Buffer(BufferOp::Storage {
        element: Type::Enum(e),
        ..
    }) = op
    else {
        return false;
    };
    let payloads = e.variants.iter().filter_map(|v| v.payload.as_ref());
    !e.indirect
        && e.owners.is_empty()
        && payloads.clone().all(crate::core::instance::scalar)
        && payloads.filter(|ty| **ty != Type::Unit).take(2).count() == 2
}

pub(super) fn lower(bodies: &mut HashMap<String, FunctionPlan>) -> Result<(), Error> {
    if !ENABLED.get() {
        return Ok(());
    }
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
            let width = e
                .variants
                .iter()
                .filter_map(|v| v.payload.as_ref())
                .map(|p| p.memory_layout().expect("checked scalar").size)
                .max()
                .expect("selected payloads");
            let carrier = match width {
                1 => Type::UInt8,
                2 => Type::UInt16,
                4 => Type::UInt32,
                8 => Type::UInt64,
                _ => panic!("expected a nonempty payload"),
            };
            let physical =
                crate::core::types::tuple_type(vec![Type::Int32, carrier], &body.type_registry)?;
            let mut unpack = None;
            match intrinsic {
                BI::InitializeNext | BI::Store => {
                    let value = args.last_mut().unwrap();
                    let packed = body.slot(physical.clone())?;
                    convert(body, value, &packed, Direction::Pack)?;
                    *value = packed;
                }
                BI::Load | BI::TakeLast => {
                    let packed = body.slot(physical.clone())?;
                    unpack = Some((dst.replace(packed.clone()).unwrap(), packed));
                }
                BI::Allocate
                | BI::CapacityLimit
                | BI::Len
                | BI::Capacity
                | BI::Transfer
                | BI::Replace
                | BI::Release
                | BI::SwapElements
                | BI::Truncate => {}
                _ => {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidCall,
                        "The compact enum array probe only supports by-value access",
                    ))
                }
            }
            body.emit(Op::Buffer(BufferOp::Storage {
                intrinsic,
                element: physical,
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
        let active = body.label_count;
        let next = active + 1;
        body.label_count += 2;
        let expected = body.slot(Type::Int32)?;
        let condition = body.slot(Type::Bool)?;
        body.emit(Op::Literal(expected.clone(), Constant::Int32(index as i32)));
        body.emit(Op::Binary(
            condition.clone(),
            BinOp::Eq,
            tag.clone(),
            expected,
        ));
        body.emit(Op::Branch {
            condition,
            then_label: active,
            else_label: next,
        });
        body.emit(Op::Label(active));
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
fn compact_enum_array_probe() {
    use crate::vm::{VmEngine, VmMode};
    let source = include_str!("../../../tests/fixtures/compact_enum_array.jk");
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ENABLED.set(false);
        }
    }
    for packed in [false, true] {
        let _reset = Reset;
        ENABLED.set(packed);
        // At this capacity a one-byte change in actual stride dwarfs the
        // buffer header. Bracket the limit on both sides to distinguish every
        // integer stride, using real allocations in all three backends.
        let stride = if packed { 8 } else { 20 };
        let capacity = crate::runtime::arena::MAX_BYTES / stride;
        let fixture = format!(
            "{source}\nfn fits() -> Int32 {{ reserve_words({}) }}\nfn exceeds() -> Int32 {{ reserve_words({}) }}",
            capacity - 1024, capacity + 1
        );
        let ast = crate::TypeRegistry::default()
            .resolve_aliases(&crate::parser::parse(&fixture).unwrap())
            .unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_module(&ast).unwrap();
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
        // A real &T would still address the expanded enum. Refuse to run
        // that combination even in the experimental compiler.
        let borrowed = format!("{source}\nfn borrowed() -> Int32 {{ let xs = [ProbeWord.A(42)]; read(*xs.borrow_ref(0)) }}");
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        let compiled = vm.load_source(&borrowed);
        if packed {
            assert!(compiled
                .unwrap_err()
                .message()
                .contains("only supports by-value access"));
        } else {
            compiled.unwrap();
            assert_eq!(vm.call_int32_0("borrowed").unwrap(), 42);
        }
        #[cfg(unix)]
        crate::backends::aot::tests::assert_sync_results(
            &ast,
            &[
                ("roundtrip", Some(42)),
                ("fits", Some(42)),
                ("exceeds", None),
            ],
        );
    }
}
