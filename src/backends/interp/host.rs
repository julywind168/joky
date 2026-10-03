//! Prevalidated native adapters and owned CPS arguments.
use super::{abi_error, runtime_error, Value};
use crate::{
    runtime::{
        abi,
        bindings::{HostBinding, HostEntry},
        cps::CpsFrame,
        string as string_abi,
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
                    direct_adapter(&binding.signature)
                };
                Some(adapter.ok_or_else(|| {
                    abi_error(format!(
                        "Host `{name}` has unsupported interpreter host signature ({:?}) -> {}",
                        binding.signature.params, binding.signature.ret,
                    ))
                })?)
            }
            HostEntry::Cps { .. } | HostEntry::BorrowingCps(_) => None,
        };
        Ok(Self {
            name: name.into(),
            binding,
            adapter,
        })
    }

    pub fn suspends(&self) -> bool {
        self.adapter.is_none()
    }

    pub fn call<'a>(&self, args: impl ExactSizeIterator<Item = &'a Value>) -> Result<Value, Error> {
        let (HostEntry::Direct(entry) | HostEntry::C(entry)) = self.binding.entry else {
            return Err(abi_error("suspending host requires a CPS frame"));
        };
        if args.len() != self.binding.signature.params.len() {
            return Err(abi_error("Host argument count mismatch"));
        }
        // Unit has no native ABI argument. Its source position is retained only
        // by CPS; direct adapters receive the non-Unit arguments in order.
        let mut native_args = [&Value::Unit; 4];
        let mut count = 0;
        for (value, ty) in args.zip(&self.binding.signature.params) {
            if *ty != Type::Unit {
                native_args[count] = value;
                count += 1;
            }
        }
        self.adapter.expect("validated direct adapter")(entry, &native_args[..count])
    }
}

pub(super) struct HostPending {
    pub host: Rc<BoundHost>,
    frame: CpsFrame,
    _strings: Vec<Box<[u8]>>,
}

impl HostPending {
    pub(super) fn wait_plan(&self) -> crate::runtime::driver::WaitPlan {
        self.frame.wait_plan()
    }

    pub fn new<'a>(
        host: Rc<BoundHost>,
        args: impl ExactSizeIterator<Item = &'a Value>,
    ) -> Result<Self, Error> {
        let frame_slots = match host.binding.entry {
            HostEntry::Cps { frame_slots, .. } => frame_slots,
            HostEntry::BorrowingCps(_) => None,
            _ => return Err(abi_error("direct host cannot create a CPS frame")),
        };
        if args.len() != host.binding.signature.params.len() {
            return Err(abi_error("Host argument count mismatch"));
        }
        let count = frame_slots.unwrap_or(args.len());
        if count < args.len() {
            return Err(abi_error("Host frame has too few slots"));
        }
        let mut frame = CpsFrame::new(count);
        let mut strings = Vec::new();
        for (index, value) in args.enumerate() {
            frame.slots[index] = if matches!(host.binding.entry, HostEntry::BorrowingCps(_)) {
                let Value::Reference(pointer) = value else {
                    return Err(abi_error("Borrowing host requires reference arguments"));
                };
                *pointer as usize as u64
            } else {
                encode_host_value(value, &mut strings)?
            };
        }
        Ok(Self {
            host,
            frame,
            _strings: strings,
        })
    }

    pub fn poll(&mut self) -> Result<Option<Value>, Error> {
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
            0 => decode_host_value(&self.host.binding.signature.ret, self.frame.result).map(Some),
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
        ([Type::Named(_)], Type::Int32) => |entry, args| {
            Ok(Value::Int32(unsafe {
                abi::call_named_i32(entry, as_named(args[0])?)
            }?))
        },
        ([Type::Named(_), Type::String], Type::Int32) => |entry, args| {
            let text = string_bytes(args[1])?;
            Ok(Value::Int32(unsafe {
                abi::call_named_string_i32(entry, as_named(args[0])?, text.as_ptr())
            }?))
        },
        ([Type::Named(_), Type::String, Type::Int32], Type::Int32) => |entry, args| {
            let text = string_bytes(args[1])?;
            Ok(Value::Int32(unsafe {
                abi::call_named_string_i32_i32(
                    entry,
                    as_named(args[0])?,
                    text.as_ptr(),
                    as_i32(args[2])?,
                )
            }?))
        },
        ([Type::Named(_), Type::String, Type::Int32, Type::Int32], Type::Int32) => |entry, args| {
            let text = string_bytes(args[1])?;
            Ok(Value::Int32(unsafe {
                abi::call_named_string_i32_i32_i32(
                    entry,
                    as_named(args[0])?,
                    text.as_ptr(),
                    as_i32(args[2])?,
                    as_i32(args[3])?,
                )
            }?))
        },
        ([Type::String], Type::Int32) => |entry, args| {
            let text = string_bytes(args[0])?;
            Ok(Value::Int32(unsafe {
                abi::call_string_i32(entry, text.as_ptr())
            }?))
        },
        ([Type::String, Type::String], Type::Int32) => |entry, args| {
            let text = string_bytes(args[0])?;
            let other = string_bytes(args[1])?;
            Ok(Value::Int32(unsafe {
                abi::call_string_string_i32(entry, text.as_ptr(), other.as_ptr())
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
                abi::call_string_string(entry, text.as_ptr())
            }?)?))
        },
        _ => return None,
    })
}

fn decode_host_value(ty: &Type, bits: u64) -> Result<Value, Error> {
    match ty {
        Type::Int32 => Ok(Value::Int32(bits as u32 as i32)),
        Type::Int64 => Ok(Value::Int64(bits as i64)),
        Type::UInt8 => Ok(Value::UInt8(bits as u8)),
        Type::UInt32 => Ok(Value::UInt32(bits as u32)),
        Type::Rune => Err(abi_error("Rune requires a checked UInt32 host adapter")),
        Type::UInt64 => Ok(Value::UInt64(bits)),
        Type::Bool => Ok(Value::Bool(bits != 0)),
        Type::Named(_) => Ok(Value::Named(bits)),
        Type::Unit => Ok(Value::Unit),
        Type::Function(_)
        | Type::Parameter(_)
        | Type::Ptr
        | Type::Buffer(_)
        | Type::BufferElement(_)
        | Type::BufferSlice(_)
        | Type::Applied(_)
        | Type::Record(_)
        | Type::Enum(_)
        | Type::Reference(_)
        | Type::Tuple(_) => Err(abi_error(
            "Aggregates and type parameters are not supported by the host ABI",
        )),
        Type::String => {
            let text = unsafe { crate::runtime::string::borrow(bits as *const u8) }?;
            Ok(Value::String(Rc::from(text)))
        }
    }
}

fn encode_host_value(value: &Value, strings: &mut Vec<Box<[u8]>>) -> Result<u64, Error> {
    if let Some(bits) = value.pack_scalar() {
        return Ok(bits);
    }
    match value {
        Value::Bool(value) => Ok(u64::from(*value)),
        Value::Named(value) => Ok(*value),
        Value::String(value) => {
            let bytes = string_abi::encode(value);
            let pointer = bytes.as_ptr() as u64;
            strings.push(bytes);
            Ok(pointer)
        }
        _ => unreachable!("pack_scalar covers Unit and the integer widths"),
    }
}

fn string_bytes(value: &Value) -> Result<Box<[u8]>, Error> {
    let Value::String(value) = value else {
        return Err(abi_error("expected String host argument"));
    };
    Ok(string_abi::encode(value))
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
