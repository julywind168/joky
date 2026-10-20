//! Prevalidated native adapters and owned CPS arguments.
use super::{abi_error, runtime_error, Value};
use crate::{
    runtime::{
        abi,
        bindings::{HostAbiKind, HostBinding, HostEntry},
        cps::CpsFrame,
    },
    Error, ErrorCode, FunctionSignature, Type,
};
use std::rc::Rc;

type DirectAdapter = fn(*const u8, &[&Value]) -> Result<Value, Error>;

pub(super) struct BoundHost {
    pub name: String,
    binding: HostBinding,
    adapter: Option<DirectAdapter>,
}

impl BoundHost {
    pub fn new(name: &str, binding: HostBinding) -> Result<Self, Error> {
        let adapter = match binding.entry {
            HostEntry::Direct(entry) | HostEntry::C(entry) => {
                if entry.is_null() {
                    return Err(abi_error("null host function entry"));
                }
                let adapter = if matches!(binding.entry, HostEntry::C(_)) {
                    c_adapter(&binding.signature)
                } else {
                    let mut wire = binding.signature.clone();
                    if binding.abi == HostAbiKind::TextViewV2 {
                        for ty in &mut wire.params {
                            if ty
                                .reference_type()
                                .is_some_and(|r| r.target == Type::String)
                            {
                                *ty = Type::String;
                            }
                        }
                    }
                    for ty in &mut wire.params {
                        if let Some(r) = ty.reference_type().filter(|r| !r.mutable) {
                            if let Type::Resource(name) = &r.target {
                                *ty = Type::Named(name.to_string());
                            }
                        }
                    }
                    direct_adapter(&wire)
                };
                Some(adapter.ok_or_else(|| {
                    abi_error(format!(
                        "Host `{name}` has unsupported interpreter host signature ({:?}) -> {}",
                        binding.signature.params, binding.signature.ret,
                    ))
                })?)
            }
            HostEntry::VmCall
            | HostEntry::VmSource
            | HostEntry::Domain(_)
            | HostEntry::Dns
            | HostEntry::Tcp(_)
            | HostEntry::Fs(_)
            | HostEntry::Cps { .. }
            | HostEntry::BorrowingCps(_)
            | HostEntry::TextFfi(_)
            | HostEntry::ResourceFactory(_)
            | HostEntry::ResourceFactoryCps(_)
            | HostEntry::ResourceIo { .. } => None,
        };
        Ok(Self {
            name: name.into(),
            binding,
            adapter,
        })
    }

    pub fn suspends(&self) -> bool {
        matches!(
            self.binding.entry,
            HostEntry::VmCall
                | HostEntry::VmSource
                | HostEntry::Domain(_)
                | HostEntry::Dns
                | HostEntry::Tcp(_)
                | HostEntry::Fs(_)
                | HostEntry::Cps { .. }
                | HostEntry::BorrowingCps(_)
                | HostEntry::ResourceFactoryCps(_)
                | HostEntry::ResourceIo { .. }
        )
    }

    pub fn signature(&self) -> &FunctionSignature {
        &self.binding.signature
    }

    pub fn call<'a>(
        &self,
        args: impl ExactSizeIterator<Item = &'a Value>,
        arena: &mut crate::runtime::arena::Arena,
    ) -> Result<Value, Error> {
        let _float_env = crate::runtime::float_env::Guard::enter();
        if let HostEntry::ResourceFactory(factory) = &self.binding.entry {
            let Some(Value::UInt64(input)) = args.into_iter().next() else {
                return Err(abi_error("Invalid resource factory input"));
            };
            let owner = unsafe { crate::runtime::resource::invoke(arena, factory, *input) }?;
            return Ok(Value::Reference(owner.as_ptr().cast()));
        }
        if let HostEntry::TextFfi(host) = &self.binding.entry {
            let mut owners = Vec::with_capacity(args.len());
            for value in args {
                let Value::Reference(owner) = value else {
                    return Err(abi_error("Invalid text FFI argument"));
                };
                owners.push(owner.cast::<crate::runtime::buffer::Buffer>().cast_const());
            }
            let value = unsafe { crate::runtime::text_ffi::invoke(arena, host, &owners) }?;
            return Ok(match self.binding.signature.ret {
                Type::String => Value::Reference(value as *mut u8),
                Type::Int32 => Value::Int32(value as i32),
                Type::Int64 => Value::Int64(value as i64),
                Type::UInt8 => Value::UInt8(value as u8),
                Type::UInt32 => Value::UInt32(value as u32),
                Type::UInt64 => Value::UInt64(value),
                Type::Unit => Value::Unit,
                _ => unreachable!("validated text FFI result"),
            });
        }
        let (HostEntry::Direct(entry) | HostEntry::C(entry)) = self.binding.entry else {
            return Err(abi_error("suspending host requires a CPS frame"));
        };
        if args.len() != self.binding.signature.params.len() {
            return Err(abi_error("Host argument count mismatch"));
        }
        let mut views = if self.binding.abi == HostAbiKind::TextViewV2 {
            vec![crate::runtime::string::JokyText::default(); args.len()]
        } else {
            Vec::new()
        };
        let mut converted = Vec::new();
        for (index, (value, ty)) in args.zip(&self.binding.signature.params).enumerate() {
            if *ty == Type::Unit {
                continue;
            }
            let value = if *ty == Type::String
                || ty
                    .reference_type()
                    .is_some_and(|r| r.target == Type::String)
            {
                let Value::Reference(owner) = value else {
                    return Err(abi_error("Invalid owning text argument"));
                };
                crate::runtime::error::checked(|| unsafe {
                    crate::runtime::owned_text::view(owner.cast(), &mut views[index]);
                })?;
                Value::HostText(views[index].as_ptr())
            } else if ty
                .reference_type()
                .is_some_and(|r| matches!(r.target, Type::Resource(_)))
            {
                let Value::Reference(owner) = value else {
                    return Err(abi_error("Invalid resource argument"));
                };
                Value::Named(*owner as usize as u64)
            } else {
                value.clone()
            };
            converted.push(value);
        }
        let args: Vec<_> = converted.iter().collect();
        let value = self.adapter.expect("validated direct adapter")(entry, &args)?;
        owning_result(value, arena)
    }
}

pub(super) struct HostPending {
    pub host: Rc<BoundHost>,
    frame: CpsFrame,
    notification: Option<crate::runtime::executor::WakeObservation>,
    pub(super) cleanups: crate::runtime::cleanup::Cleanups,
}

impl HostPending {
    pub(super) fn wait_plan(&self) -> crate::runtime::driver::WaitPlan {
        let mut plan = self.frame.wait_plan();
        // A wake may have resumed a sibling's instruction slice rather than
        // this callback. Retain readiness until this callback has observed it.
        if self.notification.as_ref().is_some_and(|n| n.changed()) {
            plan.add(Some(crate::runtime::driver::Wake::Immediate));
        }
        plan
    }

    pub fn new<'a>(
        host: Rc<BoundHost>,
        args: impl ExactSizeIterator<Item = &'a Value>,
    ) -> Result<Self, Error> {
        let frame_slots = match host.binding.entry {
            HostEntry::Cps { frame_slots, .. } => frame_slots,
            HostEntry::VmCall
            | HostEntry::VmSource
            | HostEntry::Domain(_)
            | HostEntry::Dns
            | HostEntry::Tcp(_)
            | HostEntry::Fs(_)
            | HostEntry::BorrowingCps(_)
            | HostEntry::ResourceFactoryCps(_)
            | HostEntry::ResourceIo { .. } => None,
            _ => return Err(abi_error("direct host cannot create a CPS frame")),
        };
        if args.len() != host.binding.signature.params.len() {
            return Err(abi_error("Host argument count mismatch"));
        }
        let count = frame_slots.unwrap_or(
            args.len()
                + if matches!(
                    host.binding.entry,
                    HostEntry::Domain(op) if op.status_payload()
                ) {
                    2
                } else {
                    0
                },
        );
        if count < args.len() {
            return Err(abi_error("Host frame has too few slots"));
        }
        let mut frame = CpsFrame::new(count);
        if host.binding.abi == HostAbiKind::TextViewV2 {
            frame.text_views = vec![crate::runtime::string::JokyText::default(); args.len()];
        }
        for (index, (value, ty)) in args.zip(&host.binding.signature.params).enumerate() {
            frame.slots[index] = if matches!(
                host.binding.entry,
                HostEntry::VmCall
                    | HostEntry::VmSource
                    | HostEntry::Domain(_)
                    | HostEntry::Dns
                    | HostEntry::Tcp(_)
                    | HostEntry::Fs(_)
                    | HostEntry::BorrowingCps(_)
                    | HostEntry::ResourceIo { .. }
            ) && (ty.reference_type().is_some() || ty.is_owner())
            {
                let Value::Reference(pointer) = value else {
                    return Err(abi_error("Borrowing host requires reference arguments"));
                };
                *pointer as usize as u64
            } else if ty
                .reference_type()
                .is_some_and(|r| matches!(r.target, Type::Resource(_)))
            {
                let Value::Reference(owner) = value else {
                    return Err(abi_error("Invalid resource argument"));
                };
                *owner as usize as u64
            } else if *ty == Type::String {
                let Value::Reference(owner) = value else {
                    return Err(abi_error("Invalid owning text argument"));
                };
                crate::runtime::error::checked(|| unsafe {
                    crate::runtime::owned_text::view(owner.cast(), &mut frame.text_views[index]);
                })?;
                frame.text_views[index].as_ptr() as usize as u64
            } else {
                encode_host_value(value)?
            };
        }
        Ok(Self {
            host,
            frame,
            notification: None,
            cleanups: Default::default(),
        })
    }

    pub(super) fn aggregate_result(&self) -> Option<[Option<Value>; 2]> {
        if matches!(
            self.host.binding.entry,
            HostEntry::Domain(op) if op.status_payload()
        ) {
            let offset = self.host.binding.signature.params.len();
            return Some([
                Some(Value::Int32(self.frame.slots[offset] as i32)),
                Some(Value::UInt64(self.frame.slots[offset + 1])),
            ]);
        }
        (matches!(
            self.host.binding.entry,
            HostEntry::Domain(crate::runtime::domain::Operation::OutboxMessageBytes)
        ) || matches!(self.host.binding.entry, HostEntry::Dns)
            || matches!(self.host.binding.entry, HostEntry::Fs(op) if op.returns_bytes())
            || matches!(
                self.host.binding.entry,
                HostEntry::Tcp(
                    crate::runtime::tcp::Operation::Read | crate::runtime::tcp::Operation::ReadHalf
                )
            )
            || matches!(
                self.host.binding.entry,
                HostEntry::ResourceIo {
                    kind: crate::runtime::byte_request::ByteIoKind::Read,
                    ..
                }
            ))
        .then_some([
            Some(Value::Reference(self.frame.result as usize as *mut u8)),
            None,
        ])
    }

    pub fn finish(&mut self, arena: &mut crate::runtime::arena::Arena) -> Result<(), Error> {
        self.frame.release();
        unsafe { self.cleanups.finish(arena) }
    }

    pub fn poll(
        &mut self,
        arena: &mut crate::runtime::arena::Arena,
    ) -> Result<Option<Value>, Error> {
        self.notification = crate::runtime::executor::current_waker().map(|w| w.observe());
        if matches!(self.host.binding.entry, HostEntry::VmSource) {
            let result = unsafe { crate::stdlib::source_poll(arena, &mut self.frame) }?;
            self.frame.result = result;
            return Ok(Some(Value::Reference(result as usize as *mut u8)));
        }
        if matches!(self.host.binding.entry, HostEntry::VmCall) {
            return unsafe { crate::stdlib::call_poll(&mut self.frame) }
                .map(|value| value.map(|_| Value::Unit));
        }
        if let HostEntry::Domain(op) = self.host.binding.entry {
            return unsafe { crate::runtime::domain::poll(arena, &mut self.frame, op) }.and_then(
                |value| {
                    value
                        .map(|bits| {
                            self.frame.result = bits;
                            if op.status_payload() {
                                Ok(Value::Unit)
                            } else if matches!(
                                op,
                                crate::runtime::domain::Operation::TimerCreate
                                    | crate::runtime::domain::Operation::DirectoryCreate
                                    | crate::runtime::domain::Operation::OutboxCreate
                                    | crate::runtime::domain::Operation::OutboxBatch
                                    | crate::runtime::domain::Operation::OutboxReceive
                                    | crate::runtime::domain::Operation::OutboxMessageBytes
                                    | crate::runtime::domain::Operation::AdmissionCreate
                                    | crate::runtime::domain::Operation::AdmissionTry
                                    | crate::runtime::domain::Operation::AdmissionAcquire
                                    | crate::runtime::domain::Operation::Create
                                    | crate::runtime::domain::Operation::CreateValues
                                    | crate::runtime::domain::Operation::Snapshot
                                    | crate::runtime::domain::Operation::SnapshotPair
                                    | crate::runtime::domain::Operation::SnapshotValues
                                    | crate::runtime::domain::Operation::SnapshotPairValues
                                    | crate::runtime::domain::Operation::Candidate
                                    | crate::runtime::domain::Operation::CandidateValues
                                    | crate::runtime::domain::Operation::CandidateAt
                                    | crate::runtime::domain::Operation::CandidateAtValues
                                    | crate::runtime::domain::Operation::CandidatePairAt
                                    | crate::runtime::domain::Operation::CandidatePairAtValues
                                    | crate::runtime::domain::Operation::CandidatePair
                                    | crate::runtime::domain::Operation::CandidatePairValues
                                    | crate::runtime::domain::Operation::AccessSet
                                    | crate::runtime::domain::Operation::SnapshotSet
                                    | crate::runtime::domain::Operation::SetEditSnapshot
                                    | crate::runtime::domain::Operation::SetEditCandidate
                                    | crate::runtime::domain::Operation::SetCandidate
                                    | crate::runtime::domain::Operation::SetCandidateValues
                                    | crate::runtime::domain::Operation::SetAdd
                                    | crate::runtime::domain::Operation::SetAddValues
                                    | crate::runtime::domain::Operation::CommitSet
                                    | crate::runtime::domain::Operation::CommitSetBefore
                                    | crate::runtime::domain::Operation::CommitPair
                                    | crate::runtime::domain::Operation::CommitPairBefore
                                    | crate::runtime::domain::Operation::DeadlineAfter
                                    | crate::runtime::domain::Operation::CommitBefore
                                    | crate::runtime::domain::Operation::Commit
                            ) {
                                Ok(Value::Reference(bits as usize as *mut u8))
                            } else {
                                decode_host_value(&self.host.binding.signature.ret, bits)
                            }
                        })
                        .transpose()
                },
            );
        }
        if let HostEntry::Fs(op) = self.host.binding.entry {
            return unsafe { crate::runtime::fs::poll(arena, &mut self.frame, op) }.map(|value| {
                value.map(|bits| {
                    self.frame.result = bits;
                    if op.scalar() {
                        Value::UInt64(bits)
                    } else {
                        Value::Reference(bits as usize as *mut u8)
                    }
                })
            });
        }
        if matches!(self.host.binding.entry, HostEntry::Dns) {
            return unsafe { crate::runtime::dns::poll(arena, &mut self.frame) }.map(|owner| {
                owner.map(|owner| {
                    self.frame.result = owner.as_ptr() as usize as u64;
                    Value::Reference(owner.as_ptr().cast())
                })
            });
        }
        if let HostEntry::Tcp(op) = self.host.binding.entry {
            return unsafe { crate::runtime::tcp::poll(arena, &mut self.frame, op) }.map(
                |output| {
                    output.map(|output| match output {
                        crate::runtime::tcp::Output::Owner(owner) => {
                            self.frame.result = owner.as_ptr() as usize as u64;
                            Value::Reference(owner.as_ptr().cast())
                        }
                        crate::runtime::tcp::Output::Scalar(value) => Value::UInt64(value),
                    })
                },
            );
        }
        if let HostEntry::ResourceIo { descriptor, kind } = &self.host.binding.entry {
            return unsafe {
                crate::runtime::byte_request::poll_io(arena, &mut self.frame, descriptor, *kind)
            }
            .map(|result| {
                result.map(|result| match result {
                    crate::runtime::byte_request::IoOutput::Read(owner) => {
                        self.frame.result = owner.as_ptr() as usize as u64;
                        Value::Reference(owner.as_ptr().cast())
                    }
                    crate::runtime::byte_request::IoOutput::Write(count) => Value::UInt64(count),
                })
            });
        }
        if let HostEntry::ResourceFactoryCps(factory) = &self.host.binding.entry {
            return unsafe {
                crate::runtime::resource::poll_factory(arena, &mut self.frame, factory)
            }
            .map(|owner| owner.map(|owner| Value::Reference(owner.as_ptr().cast())));
        }
        // Registration establishes the ABI/lifetime contract. `self` owns the
        // frame, argument buffers and binding owner until this call is finished.
        let status = match &self.host.binding.entry {
            HostEntry::Cps { entry, .. } => unsafe { abi::poll_cps(*entry, &mut self.frame) },
            HostEntry::BorrowingCps(host) => crate::runtime::error::checked(|| unsafe {
                crate::runtime::borrowed_host::joky_cps_poll_borrowing_v1(
                    &mut self.frame,
                    host.as_ref(),
                )
            }),
            _ => unreachable!("validated CPS binding"),
        }?;
        match status {
            0 => decode_host_value(&self.host.binding.signature.ret, self.frame.result)
                .and_then(|value| owning_result(value, arena))
                .map(Some),
            1 => Ok(None),
            status => Err(runtime_error(
                ErrorCode::HostError,
                format!(
                    "Suspending host `{}` returned failure status {status}",
                    self.host.name,
                ),
            )),
        }
    }
}

impl Drop for HostPending {
    fn drop(&mut self) {
        // The host field (including its code owner) otherwise drops before frame.
        self.frame.release();
    }
}

fn c_adapter(signature: &FunctionSignature) -> Option<DirectAdapter> {
    crate::core::ffi::validate_registration(signature).ok()?;
    fn pointers(args: &[&Value]) -> Result<[*mut u8; 4], Error> {
        let mut pointers = [std::ptr::null_mut(); 4];
        for (dst, arg) in pointers.iter_mut().zip(args) {
            let pointer = match arg {
                Value::Ptr(pointer) => {
                    *dst = *pointer;
                    continue;
                }
                Value::Reference(pointer) => pointer,
                _ => return Err(abi_error("Expected a C reference or Ptr argument")),
            };
            if pointer.is_null() {
                return Err(abi_error("Inactive C reference"));
            }
            *dst = *pointer;
        }
        Ok(pointers)
    }
    Some(match signature.ret {
        Type::Ptr => |entry, args| {
            Ok(Value::Ptr(unsafe {
                abi::call_c_ptr(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::Int32 => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_c_i32(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::Int64 => |entry, args| {
            Ok(Value::Int64(unsafe {
                abi::call_c_i64(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::UInt8 => |entry, args| {
            Ok(Value::UInt8(unsafe {
                abi::call_c_u8(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::UInt32 => |entry, args| {
            Ok(Value::UInt32(unsafe {
                abi::call_c_u32(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::UInt64 => |entry, args| {
            Ok(Value::UInt64(unsafe {
                abi::call_c_u64(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::Float64 => |entry, args| {
            Ok(Value::Float64(unsafe {
                abi::call_c_f64(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::Int8 => |entry, args| {
            Ok(Value::Int8(unsafe {
                abi::call_c_i8(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::Int16 => |entry, args| {
            Ok(Value::Int16(unsafe {
                abi::call_c_i16(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::UInt16 => |entry, args| {
            Ok(Value::UInt16(unsafe {
                abi::call_c_u16(entry, &pointers(args)?[..args.len()])
            }?))
        },
        Type::Unit => |entry, args| {
            unsafe { abi::call_c_unit(entry, &pointers(args)?[..args.len()]) }?;
            Ok(Value::Unit)
        },
        _ => return None,
    })
}

fn direct_adapter(signature: &FunctionSignature) -> Option<DirectAdapter> {
    let params: Vec<_> = signature
        .params
        .iter()
        .filter(|ty| **ty != Type::Unit)
        .cloned()
        .collect();
    // Select once at load time. Execution never repeats signature matching.
    Some(match (params.as_slice(), &signature.ret) {
        ([Type::Int32], Type::Float64) => |entry, args| {
            Ok(Value::Float64(unsafe {
                abi::call_i32_f64_0(entry, as_i32(args[0])?)
            }?))
        },
        ([Type::Int32, Type::Float64, Type::Float64], Type::Int32) => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_i32_f64_f64_i32(
                    entry,
                    as_i32(args[0])?,
                    as_f64(args[1])?,
                    as_f64(args[2])?,
                )
            }?))
        },
        ([Type::Int32, Type::String], Type::Int32) => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_i32_string_i32(entry, as_i32(args[0])?, string_bytes(args[1])?)
            }?))
        },
        ([Type::Int32], Type::String) => |entry, args| {
            Ok(Value::String(decode_string_ptr(unsafe {
                abi::call_i32_string(entry, as_i32(args[0])?)
            }?)?))
        },
        ([], Type::Float32) => |entry, _| Ok(Value::Float32(unsafe { abi::call_f32_0(entry) }?)),
        ([Type::Float32], Type::Float32) => |entry, args| {
            Ok(Value::Float32(unsafe {
                abi::call_f32_1(entry, as_f32(args[0])?)
            }?))
        },
        ([Type::Float32, Type::Float32], Type::Float32) => |entry, args| {
            Ok(Value::Float32(unsafe {
                abi::call_f32_2(entry, as_f32(args[0])?, as_f32(args[1])?)
            }?))
        },
        ([Type::Float32], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_f32_1(entry, as_f32(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::Float32, Type::Float32], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_f32_2(entry, as_f32(args[0])?, as_f32(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::Float64) => |entry, _| Ok(Value::Float64(unsafe { abi::call_f64_0(entry) }?)),
        ([Type::Float64], Type::Float64) => |entry, args| {
            Ok(Value::Float64(unsafe {
                abi::call_f64_1(entry, as_f64(args[0])?)
            }?))
        },
        ([Type::Float64, Type::Float64], Type::Float64) => |entry, args| {
            Ok(Value::Float64(unsafe {
                abi::call_f64_2(entry, as_f64(args[0])?, as_f64(args[1])?)
            }?))
        },
        ([Type::Float64, Type::Float64, Type::Float64], Type::Float64) => |entry, args| {
            Ok(Value::Float64(unsafe {
                abi::call_f64_3(entry, as_f64(args[0])?, as_f64(args[1])?, as_f64(args[2])?)
            }?))
        },
        ([Type::Float64, Type::Int32], Type::Float64) => |entry, args| {
            Ok(Value::Float64(unsafe {
                abi::call_f64_i32(entry, as_f64(args[0])?, as_i32(args[1])?)
            }?))
        },
        ([Type::Int32, Type::Float64], Type::Float64) => |entry, args| {
            Ok(Value::Float64(unsafe {
                abi::call_i32_f64(entry, as_i32(args[0])?, as_f64(args[1])?)
            }?))
        },
        ([Type::Float64], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_f64_1(entry, as_f64(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::Float64, Type::Float64], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_f64_2(entry, as_f64(args[0])?, as_f64(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::Int8) => |entry, _| Ok(Value::Int8(unsafe { abi::call_i8_0(entry) }?)),
        ([Type::Int8], Type::Int8) => |entry, args| {
            Ok(Value::Int8(unsafe {
                abi::call_i8_1(entry, as_i8(args[0])?)
            }?))
        },
        ([Type::Int8, Type::Int8], Type::Int8) => |entry, args| {
            Ok(Value::Int8(unsafe {
                abi::call_i8_2(entry, as_i8(args[0])?, as_i8(args[1])?)
            }?))
        },
        ([Type::Int8], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_i8_1(entry, as_i8(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::Int8, Type::Int8], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_i8_2(entry, as_i8(args[0])?, as_i8(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::Int16) => |entry, _| Ok(Value::Int16(unsafe { abi::call_i16_0(entry) }?)),
        ([Type::Int16], Type::Int16) => |entry, args| {
            Ok(Value::Int16(unsafe {
                abi::call_i16_1(entry, as_i16(args[0])?)
            }?))
        },
        ([Type::Int16, Type::Int16], Type::Int16) => |entry, args| {
            Ok(Value::Int16(unsafe {
                abi::call_i16_2(entry, as_i16(args[0])?, as_i16(args[1])?)
            }?))
        },
        ([Type::Int16], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_i16_1(entry, as_i16(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::Int16, Type::Int16], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_i16_2(entry, as_i16(args[0])?, as_i16(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::UInt16) => |entry, _| Ok(Value::UInt16(unsafe { abi::call_u16_0(entry) }?)),
        ([Type::UInt16], Type::UInt16) => |entry, args| {
            Ok(Value::UInt16(unsafe {
                abi::call_u16_1(entry, as_u16(args[0])?)
            }?))
        },
        ([Type::UInt16, Type::UInt16], Type::UInt16) => |entry, args| {
            Ok(Value::UInt16(unsafe {
                abi::call_u16_2(entry, as_u16(args[0])?, as_u16(args[1])?)
            }?))
        },
        ([Type::UInt16], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_u16_1(entry, as_u16(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::UInt16, Type::UInt16], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_u16_2(entry, as_u16(args[0])?, as_u16(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::UInt8) => |entry, _| Ok(Value::UInt8(unsafe { abi::call_u8_0(entry) }?)),
        ([Type::UInt8], Type::UInt8) => |entry, args| {
            Ok(Value::UInt8(unsafe {
                abi::call_u8_1(entry, as_u8(args[0])?)
            }?))
        },
        ([Type::UInt8, Type::UInt8], Type::UInt8) => |entry, args| {
            Ok(Value::UInt8(unsafe {
                abi::call_u8_2(entry, as_u8(args[0])?, as_u8(args[1])?)
            }?))
        },
        ([Type::UInt8], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_u8_1(entry, as_u8(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::UInt8, Type::UInt8], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_u8_2(entry, as_u8(args[0])?, as_u8(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::UInt32) => |entry, _| Ok(Value::UInt32(unsafe { abi::call_u32_0(entry) }?)),
        ([Type::UInt32], Type::UInt32) => |entry, args| {
            Ok(Value::UInt32(unsafe {
                abi::call_u32_1(entry, as_u32(args[0])?)
            }?))
        },
        ([Type::UInt32, Type::UInt32], Type::UInt32) => |entry, args| {
            Ok(Value::UInt32(unsafe {
                abi::call_u32_2(entry, as_u32(args[0])?, as_u32(args[1])?)
            }?))
        },
        ([Type::UInt32], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_u32_1(entry, as_u32(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::UInt32, Type::UInt32], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_u32_2(entry, as_u32(args[0])?, as_u32(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::UInt64) => |entry, _| Ok(Value::UInt64(unsafe { abi::call_u64_0(entry) }?)),
        ([Type::UInt64], Type::UInt64) => |entry, args| {
            Ok(Value::UInt64(unsafe {
                abi::call_u64_1(entry, as_u64(args[0])?)
            }?))
        },
        ([Type::UInt64, Type::UInt64], Type::UInt64) => |entry, args| {
            Ok(Value::UInt64(unsafe {
                abi::call_u64_2(entry, as_u64(args[0])?, as_u64(args[1])?)
            }?))
        },
        ([Type::UInt64], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_u64_1(entry, as_u64(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::UInt64, Type::UInt64], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_u64_2(entry, as_u64(args[0])?, as_u64(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::Int64) => |entry, _| Ok(Value::Int64(unsafe { abi::call_i64_0(entry) }?)),
        ([Type::Int64], Type::Int64) => |entry, args| {
            Ok(Value::Int64(unsafe {
                abi::call_i64_1(entry, as_i64(args[0])?)
            }?))
        },
        ([Type::Int64, Type::Int64], Type::Int64) => |entry, args| {
            Ok(Value::Int64(unsafe {
                abi::call_i64_2(entry, as_i64(args[0])?, as_i64(args[1])?)
            }?))
        },
        ([Type::Int64], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_i64_1(entry, as_i64(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::Int64, Type::Int64], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_i64_2(entry, as_i64(args[0])?, as_i64(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::Int32) => |entry, _| Ok(Value::Int32(unsafe { abi::call_i32_0(entry) }?)),
        ([Type::Int32], Type::Int32) => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_i32_1(entry, as_i32(args[0])?)
            }?))
        },
        ([Type::Int32, Type::Int32], Type::Int32) => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_i32_2(entry, as_i32(args[0])?, as_i32(args[1])?)
            }?))
        },
        ([], Type::Unit) => |entry, _| {
            unsafe { abi::call_unit_0(entry) }?;
            Ok(Value::Unit)
        },
        ([Type::Int32], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_1(entry, as_i32(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::Int32, Type::Int32], Type::Unit) => |entry, args| {
            unsafe { abi::call_unit_2(entry, as_i32(args[0])?, as_i32(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([], Type::Named(_)) => |entry, _| Ok(Value::Named(unsafe { abi::call_named_0(entry) }?)),
        ([Type::Named(_)], Type::UInt64) => |entry, args| {
            Ok(Value::UInt64(unsafe {
                abi::call_u64_1(entry, as_named(args[0])?)
            }?))
        },
        ([Type::Named(_), Type::UInt64], Type::Int32) => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_named_u64_i32(entry, as_named(args[0])?, as_u64(args[1])?)
            }?))
        },
        ([Type::Named(_), Type::UInt64, Type::String, Type::String, Type::String], Type::Int32) => {
            |entry, args| {
                Ok(Value::Int32(unsafe {
                    abi::call_named_u64_three_strings_i32(
                        entry,
                        as_named(args[0])?,
                        as_u64(args[1])?,
                        string_bytes(args[2])?,
                        string_bytes(args[3])?,
                        string_bytes(args[4])?,
                    )
                }?))
            }
        }
        ([Type::Named(_), Type::UInt64, Type::String, Type::String], Type::Int32) => {
            |entry, args| {
                Ok(Value::Int32(unsafe {
                    abi::call_named_u64_strings_i32(
                        entry,
                        as_named(args[0])?,
                        as_u64(args[1])?,
                        string_bytes(args[2])?,
                        string_bytes(args[3])?,
                    )
                }?))
            }
        }
        ([Type::Named(_)], Type::Int32) => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_named_i32(entry, as_named(args[0])?)
            }?))
        },
        ([Type::Named(_), Type::String], Type::Int32) => |entry, args| {
            let text = string_bytes(args[1])?;
            Ok(Value::Int32(unsafe {
                abi::call_named_string_i32(entry, as_named(args[0])?, text)
            }?))
        },
        ([Type::Named(_), Type::String, Type::String], Type::Int32) => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_named_strings_i32(
                    entry,
                    as_named(args[0])?,
                    string_bytes(args[1])?,
                    string_bytes(args[2])?,
                )
            }?))
        },
        ([Type::Named(_), Type::String, Type::Int32], Type::Int32) => |entry, args| {
            let text = string_bytes(args[1])?;
            Ok(Value::Int32(unsafe {
                abi::call_named_string_i32_i32(entry, as_named(args[0])?, text, as_i32(args[2])?)
            }?))
        },
        ([Type::Named(_), Type::String, Type::Int32, Type::Int32], Type::Int32) => |entry, args| {
            let text = string_bytes(args[1])?;
            Ok(Value::Int32(unsafe {
                abi::call_named_string_i32_i32_i32(
                    entry,
                    as_named(args[0])?,
                    text,
                    as_i32(args[2])?,
                    as_i32(args[3])?,
                )
            }?))
        },
        ([Type::Named(_), Type::String, Type::Float64], Type::Float64) => |entry, args| {
            let text = string_bytes(args[1])?;
            Ok(Value::Float64(unsafe {
                abi::call_named_string_f64(entry, as_named(args[0])?, text, as_f64(args[2])?)
            }?))
        },
        ([Type::Named(_), Type::String, Type::Float64, Type::Float64], Type::Float64) => {
            |entry, args| {
                let text = string_bytes(args[1])?;
                Ok(Value::Float64(unsafe {
                    abi::call_named_string_f64_f64(
                        entry,
                        as_named(args[0])?,
                        text,
                        as_f64(args[2])?,
                        as_f64(args[3])?,
                    )
                }?))
            }
        }
        ([Type::String], Type::Unit) => |entry, args| {
            unsafe { abi::call_string_unit(entry, string_bytes(args[0])?) }?;
            Ok(Value::Unit)
        },
        ([Type::String, Type::Int32], Type::Unit) => |entry, args| {
            unsafe { abi::call_string_i32_unit(entry, string_bytes(args[0])?, as_i32(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([Type::String], Type::Float64) => |entry, args| {
            Ok(Value::Float64(unsafe {
                abi::call_string_f64(entry, string_bytes(args[0])?)
            }?))
        },
        ([Type::String, Type::Float64], Type::Unit) => |entry, args| {
            unsafe { abi::call_string_f64_unit(entry, string_bytes(args[0])?, as_f64(args[1])?) }?;
            Ok(Value::Unit)
        },
        ([Type::String], Type::Int32) => |entry, args| {
            let text = string_bytes(args[0])?;
            Ok(Value::Int32(unsafe { abi::call_string_i32(entry, text) }?))
        },
        ([Type::String, Type::String], Type::Int32) => |entry, args| {
            let text = string_bytes(args[0])?;
            let other = string_bytes(args[1])?;
            Ok(Value::Int32(unsafe {
                abi::call_string_string_i32(entry, text, other)
            }?))
        },
        ([], Type::String) => |entry, _| {
            Ok(Value::String(decode_string_ptr(unsafe {
                abi::call_string_0(entry)
            }?)?))
        },
        ([Type::String], Type::String) => |entry, args| {
            let text = string_bytes(args[0])?;
            Ok(Value::String(decode_string_ptr(unsafe {
                abi::call_string_string(entry, text)
            }?)?))
        },
        _ => return None,
    })
}

fn decode_host_value(ty: &Type, bits: u64) -> Result<Value, Error> {
    match ty {
        Type::Float32 => Ok(Value::Float32(f32::from_bits(bits as u32))),
        Type::Float64 => Ok(Value::Float64(f64::from_bits(bits))),
        Type::Int8 => Ok(Value::Int8(bits as u8 as i8)),
        Type::Int16 => Ok(Value::Int16(bits as u16 as i16)),
        Type::Int32 => Ok(Value::Int32(bits as u32 as i32)),
        Type::Int64 => Ok(Value::Int64(bits as i64)),
        Type::UInt8 => Ok(Value::UInt8(bits as u8)),
        Type::UInt16 => Ok(Value::UInt16(bits as u16)),
        Type::UInt32 => Ok(Value::UInt32(bits as u32)),
        Type::Rune => Err(abi_error("Rune requires a checked UInt32 host adapter")),
        Type::UInt64 => Ok(Value::UInt64(bits)),
        Type::Bool => Ok(Value::Bool(bits != 0)),
        Type::Named(_) => Ok(Value::Named(bits)),
        Type::Unit => Ok(Value::Unit),
        Type::Function(_)
        | Type::DynTrait(_)
        | Type::Parameter(_)
        | Type::Projection(_)
        | Type::Ptr
        | Type::Buffer(_)
        | Type::BufferElement(_)
        | Type::BufferSlice(_)
        | Type::FixedArray(_)
        | Type::FixedArrayInit(_)
        | Type::Applied(_)
        | Type::Record(_)
        | Type::Enum(_)
        | Type::Reference(_)
        | Type::Tuple(_) => Err(abi_error(
            "Aggregates and type parameters are not supported by the host ABI",
        )),
        Type::String => Ok(Value::String(decode_string_ptr(bits as *const u8)?)),
        Type::Resource(_) => Err(abi_error("Resource requires its managed ABI")),
    }
}

fn encode_host_value(value: &Value) -> Result<u64, Error> {
    if let Some(bits) = value.pack_scalar() {
        return Ok(bits);
    }
    match value {
        Value::Bool(value) => Ok(u64::from(*value)),
        Value::Named(value) => Ok(*value),
        _ => unreachable!("pack_scalar covers Unit and the integer widths"),
    }
}

fn string_bytes(value: &Value) -> Result<*const u8, Error> {
    match value {
        Value::HostText(pointer) => Ok(*pointer),
        _ => Err(abi_error("expected host text descriptor")),
    }
}

fn owning_result(value: Value, arena: &mut crate::runtime::arena::Arena) -> Result<Value, Error> {
    match value {
        Value::String(text) => Ok(Value::Reference(
            crate::runtime::buffer::Buffer::copy_bytes(arena, text.as_bytes())?
                .as_ptr()
                .cast(),
        )),
        value => Ok(value),
    }
}

fn as_i8(value: &Value) -> Result<i8, Error> {
    match value {
        Value::Int8(value) => Ok(*value),
        _ => Err(abi_error("Expected an Int8 host argument")),
    }
}

fn as_i16(value: &Value) -> Result<i16, Error> {
    match value {
        Value::Int16(value) => Ok(*value),
        _ => Err(abi_error("Expected an Int16 host argument")),
    }
}

fn as_u16(value: &Value) -> Result<u16, Error> {
    match value {
        Value::UInt16(value) => Ok(*value),
        _ => Err(abi_error("Expected a UInt16 host argument")),
    }
}

fn as_i32(value: &Value) -> Result<i32, Error> {
    match value {
        Value::Int32(value) => Ok(*value),
        _ => Err(abi_error("expected Int32 host argument")),
    }
}

fn as_named(value: &Value) -> Result<u64, Error> {
    match value {
        Value::Named(value) => Ok(*value),
        _ => Err(abi_error("expected named host argument")),
    }
}

fn decode_string_ptr(pointer: *const u8) -> Result<Rc<str>, Error> {
    let text = unsafe { crate::runtime::string::borrow(pointer) }?;
    Ok(Rc::from(text))
}

fn as_i64(value: &Value) -> Result<i64, Error> {
    match value {
        Value::Int64(value) => Ok(*value),
        _ => Err(abi_error("expected Int64 host argument")),
    }
}

fn as_u8(value: &Value) -> Result<u8, Error> {
    match value {
        Value::UInt8(value) => Ok(*value),
        _ => Err(abi_error("expected UInt8 host argument")),
    }
}

fn as_u32(value: &Value) -> Result<u32, Error> {
    match value {
        Value::UInt32(value) => Ok(*value),
        _ => Err(abi_error("expected UInt32 host argument")),
    }
}

fn as_u64(value: &Value) -> Result<u64, Error> {
    match value {
        Value::UInt64(value) => Ok(*value),
        _ => Err(abi_error("expected UInt64 host argument")),
    }
}

fn as_f32(value: &Value) -> Result<f32, Error> {
    match value {
        Value::Float32(n) => Ok(*n),
        _ => Err(abi_error("expected float host argument")),
    }
}

fn as_f64(value: &Value) -> Result<f64, Error> {
    match value {
        Value::Float64(n) => Ok(*n),
        _ => Err(abi_error("expected float host argument")),
    }
}
