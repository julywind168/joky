//! Single source of truth for runtime intrinsic signatures, ownership, and
//! link symbols. The MIR verifier and codegen both read this spec instead of
//! keeping parallel match tables.

use joky_runtime_abi::symbols::*;

use crate::diagnostic::Diagnostic;
use crate::sema::{Type, TypeTable};

use super::{MirOwnership, RuntimeIntrinsic};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntrinsicParam {
    Exact(Type),
    CPointer,
    CMutPtr,
    List,
    MutList,
    MutListCursor,
    Map,
    MutMap,
    MutMapCursor,
    MutSet,
    MutSetCursor,
    MapCursorFamily(Type, Type),
    Cown,
    Integer,
}

impl IntrinsicParam {
    pub(crate) fn matches(self, ty: Type, types: &TypeTable) -> bool {
        match self {
            Self::Exact(expected) => ty == expected,
            Self::CPointer => ty.is_c_pointer(),
            Self::CMutPtr => matches!(ty, Type::CMutPtr(_)),
            Self::List => matches!(ty, Type::List(_)),
            Self::MutList => matches!(ty, Type::MutList(_)),
            Self::MutListCursor => matches!(ty, Type::MutListCursor(_)),
            Self::Map => matches!(ty, Type::Map(_)),
            Self::MutMap => matches!(ty, Type::MutMap(_)),
            Self::MutMapCursor => matches!(ty, Type::MutMapCursor(_)),
            Self::MutSet => matches!(ty, Type::MutSet(_)),
            Self::MutSetCursor => matches!(ty, Type::MutSetCursor(_)),
            Self::MapCursorFamily(key, value) => match ty {
                Type::MapCursor(id) | Type::MapKeyCursor(id) | Type::MapValueCursor(id) => {
                    let info = types.map_info(id);
                    info.key == key && info.value == value
                }
                _ => false,
            },
            Self::Cown => matches!(ty, Type::Cown(_)),
            Self::Integer => ty.is_integer(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntrinsicArgOwnership {
    Any,
    Copy,
    Shared,
    Borrowed,
    FromType,
}

impl IntrinsicArgOwnership {
    pub(crate) fn expected(self, ty: Type, types: &TypeTable) -> Option<MirOwnership> {
        match self {
            Self::Any => None,
            Self::Copy => Some(MirOwnership::Copy),
            Self::Shared => Some(MirOwnership::Shared),
            Self::Borrowed => Some(MirOwnership::Borrowed),
            Self::FromType => Some(if types.is_owned(ty) {
                MirOwnership::Borrowed
            } else if types.is_shared(ty) {
                MirOwnership::Shared
            } else {
                MirOwnership::Copy
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeIntrinsicSpec {
    pub parameters: Vec<IntrinsicParam>,
    pub variadic: Option<IntrinsicParam>,
    pub result: Type,
    pub arg_ownership: Vec<IntrinsicArgOwnership>,
    pub symbol: Option<&'static str>,
    pub label: &'static str,
}

fn spec(
    parameters: Vec<IntrinsicParam>,
    result: Type,
    label: &'static str,
    symbol: Option<&'static str>,
) -> RuntimeIntrinsicSpec {
    let arg_ownership = vec![IntrinsicArgOwnership::Any; parameters.len()];
    RuntimeIntrinsicSpec {
        parameters,
        variadic: None,
        result,
        arg_ownership,
        symbol,
        label,
    }
}

fn spec_own(
    parameters: Vec<IntrinsicParam>,
    result: Type,
    ownership: Vec<IntrinsicArgOwnership>,
    label: &'static str,
    symbol: Option<&'static str>,
) -> RuntimeIntrinsicSpec {
    RuntimeIntrinsicSpec {
        parameters,
        variadic: None,
        result,
        arg_ownership: ownership,
        symbol,
        label,
    }
}

fn intern_err(message: &'static str) -> Diagnostic {
    Diagnostic::codegen(message)
}

fn list_ty(types: &TypeTable, element: Type, message: &'static str) -> Result<Type, Diagnostic> {
    types
        .list_id(element)
        .map(Type::List)
        .ok_or_else(|| intern_err(message))
}

fn option_ty(types: &TypeTable, value: Type, message: &'static str) -> Result<Type, Diagnostic> {
    types
        .option_id(value)
        .map(Type::Option)
        .ok_or_else(|| intern_err(message))
}

fn map_ty(
    types: &TypeTable,
    key: Type,
    value: Type,
    message: &'static str,
) -> Result<usize, Diagnostic> {
    types.map_id(key, value).ok_or_else(|| intern_err(message))
}

fn tuple_ty(
    types: &TypeTable,
    elements: &[Type],
    message: &'static str,
) -> Result<Type, Diagnostic> {
    types
        .tuple_id(elements)
        .map(Type::Tuple)
        .ok_or_else(|| intern_err(message))
}

fn cursor_option(
    types: &TypeTable,
    item: Type,
    cursor: Type,
    pair_message: &'static str,
    option_message: &'static str,
) -> Result<Type, Diagnostic> {
    let pair = tuple_ty(types, &[item, cursor], pair_message)?;
    option_ty(types, pair, option_message)
}

impl RuntimeIntrinsic {
    pub(crate) fn spec(
        &self,
        types: &TypeTable,
        arguments: &[Type],
    ) -> Result<RuntimeIntrinsicSpec, Diagnostic> {
        use IntrinsicParam as P;
        use RuntimeIntrinsic::*;
        match *self {
            HasherNew => Ok(spec(
                vec![],
                Type::Hasher,
                "Hasher.new",
                Some(HASHER_NEW_SYMBOL),
            )),
            HasherFinish => Ok(spec(
                vec![P::Exact(Type::Hasher)],
                Type::U64,
                "Hasher.finish",
                Some(HASHER_FINISH_SYMBOL),
            )),
            Hash(ty) if types.has_builtin_hash(ty) && !matches!(ty, Type::Tuple(_)) => {
                Ok(spec_own(
                    vec![P::Exact(ty), P::Exact(Type::Hasher)],
                    Type::Unit,
                    vec![IntrinsicArgOwnership::FromType, IntrinsicArgOwnership::Any],
                    "Hash.hash",
                    None,
                ))
            }
            Hash(_) => Err(intern_err("invalid builtin Hash type")),
            PartialEqual(ty)
                if types.has_builtin_partial_eq(ty) && types.comparison_members(ty).is_none() =>
            {
                let ownership = if types.is_shared(ty) {
                    IntrinsicArgOwnership::Shared
                } else {
                    IntrinsicArgOwnership::Copy
                };
                Ok(spec_own(
                    vec![P::Exact(ty), P::Exact(ty)],
                    Type::Bool,
                    vec![ownership, ownership],
                    "PartialEq.equals",
                    None,
                ))
            }
            PartialCompare(ty)
                if types.has_builtin_ordering(ty, false)
                    && types.comparison_members(ty).is_none() =>
            {
                let ownership = if types.is_shared(ty) {
                    IntrinsicArgOwnership::Shared
                } else {
                    IntrinsicArgOwnership::Copy
                };
                Ok(spec_own(
                    vec![P::Exact(ty), P::Exact(ty)],
                    types.partial_ordering_type(),
                    vec![ownership, ownership],
                    "PartialOrd.partial_compare",
                    None,
                ))
            }
            Compare(ty)
                if types.has_builtin_ordering(ty, true)
                    && types.comparison_members(ty).is_none() =>
            {
                let ownership = if types.is_shared(ty) {
                    IntrinsicArgOwnership::Shared
                } else {
                    IntrinsicArgOwnership::Copy
                };
                Ok(spec_own(
                    vec![P::Exact(ty), P::Exact(ty)],
                    types.ordering_type(),
                    vec![ownership, ownership],
                    "Ord.compare",
                    None,
                ))
            }
            PartialEqual(_) | PartialCompare(_) | Compare(_) => {
                Err(intern_err("invalid builtin comparison operands"))
            }
            DebugNativeId(ty) if ty.is_native_resource() => Ok(spec_own(
                vec![P::Exact(ty)],
                Type::U64,
                vec![IntrinsicArgOwnership::Borrowed],
                "Debug.native_id",
                Some(DEBUG_NATIVE_ID_SYMBOL),
            )),
            DebugPath(ty) if matches!(ty, Type::Struct(_) | Type::Class(_) | Type::Enum(_)) => {
                Ok(spec_own(
                    vec![P::Exact(Type::Bytes), P::Exact(ty)],
                    Type::Bytes,
                    vec![IntrinsicArgOwnership::Any, IntrinsicArgOwnership::Borrowed],
                    "Debug.path",
                    Some(DEBUG_PATH_SYMBOL),
                ))
            }
            DebugPathStatus => Ok(spec(
                vec![P::Exact(Type::Bytes)],
                Type::I32,
                "Debug.path_status",
                Some(DEBUG_PATH_STATUS_SYMBOL),
            )),
            DebugBytes => Ok(spec(
                vec![P::Exact(Type::Bytes)],
                Type::String,
                "Debug.bytes",
                Some(DEBUG_BYTES_SYMBOL),
            )),
            DebugDuration => Ok(spec(
                vec![P::Exact(Type::Duration)],
                Type::String,
                "Debug.duration",
                Some(DEBUG_DURATION_SYMBOL),
            )),
            DebugNativeId(_) | DebugPath(_) => Err(intern_err("invalid Debug intrinsic type")),
            DebugString => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::String,
                "Debug.debug",
                Some(DEBUG_STRING_SYMBOL),
            )),
            CCallbackNew(signature) => {
                let Type::Function(id) = signature else {
                    return Err(intern_err("CCallback requires a function type"));
                };
                let info = types.function_type(id);
                if info.parameters.iter().any(|ty| !ty.is_c_abi_compatible())
                    || !(info.return_type.is_c_abi_compatible() || info.return_type == Type::Unit)
                {
                    return Err(intern_err("invalid CCallback signature"));
                }
                Ok(spec(
                    vec![P::Exact(Type::Function(id)), P::Exact(info.return_type)],
                    Type::CCallback,
                    "CCallback.new",
                    Some(CALLBACK_NEW_SYMBOL),
                ))
            }
            CCallbackFunction | CCallbackContext => {
                let id = types
                    .c_pointer_id(Type::Unit)
                    .ok_or_else(|| intern_err("callback pointer type missing"))?;
                Ok(spec(
                    vec![P::Exact(Type::CCallback)],
                    if matches!(*self, CCallbackFunction) {
                        Type::CPtr(id)
                    } else {
                        Type::CMutPtr(id)
                    },
                    "CCallback accessor",
                    Some(if matches!(*self, CCallbackFunction) {
                        CALLBACK_FUNCTION_SYMBOL
                    } else {
                        CALLBACK_CONTEXT_SYMBOL
                    }),
                ))
            }
            CCallbackClose => Ok(spec(
                vec![P::Exact(Type::CCallback)],
                Type::Unit,
                "CCallback.close",
                None,
            )),
            CCallbackFailed => Ok(spec(
                vec![P::Exact(Type::CCallback)],
                Type::Bool,
                "CCallback.failed",
                Some(CALLBACK_FAILED_SYMBOL),
            )),
            CPointerNull(ty) if ty.is_c_pointer() => Ok(spec(vec![], ty, "C pointer null", None)),
            CPointerNull(_) => Err(intern_err("invalid C pointer null constructor")),
            CPointerIsNull => Ok(spec(
                vec![P::CPointer],
                Type::Bool,
                "C pointer is_null",
                None,
            )),
            CStrAsString => Ok(spec(
                vec![P::Exact(Type::CStr)],
                option_ty(
                    types,
                    Type::String,
                    "MIR CStr to_string Option was not interned",
                )?,
                "CStr.to_string",
                Some(STRING_FROM_CSTR_SYMBOL),
            )),
            CCellAlloc(content) => {
                let pointer = types
                    .c_pointer_id(content)
                    .map(Type::CMutPtr)
                    .ok_or_else(|| intern_err("MIR C cell pointer type was not interned"))?;
                Ok(spec(
                    vec![P::Exact(content)],
                    pointer,
                    "C cell alloc",
                    Some(C_ALLOC_SYMBOL),
                ))
            }
            CCellRead(content) => Ok(spec(
                vec![P::Exact(
                    types
                        .c_pointer_id(content)
                        .map(Type::CMutPtr)
                        .ok_or_else(|| intern_err("MIR C cell pointer type was not interned"))?,
                )],
                content,
                "C cell read",
                None,
            )),
            CCellWrite(content) => {
                let pointer = types
                    .c_pointer_id(content)
                    .map(Type::CMutPtr)
                    .ok_or_else(|| intern_err("MIR C cell pointer type was not interned"))?;
                Ok(spec(
                    vec![P::Exact(pointer), P::Exact(content)],
                    Type::Unit,
                    "C cell write",
                    None,
                ))
            }
            CCellFree => Ok(spec(
                vec![P::CMutPtr],
                Type::Unit,
                "C cell free",
                Some(C_FREE_SYMBOL),
            )),
            BatchNew(element) => Ok(spec(
                vec![
                    P::Exact(list_ty(types, element, "batch input List not interned")?),
                    P::Exact(Type::U64),
                ],
                Type::Batch,
                "Batch.new",
                Some(BATCH_NEW_SYMBOL),
            )),
            BatchWorker => Ok(spec(
                vec![P::Exact(Type::Batch)],
                Type::Bool,
                "Batch.worker",
                Some(BATCH_WORKER_SYMBOL),
            )),
            BatchNext(element) => {
                let input = list_ty(types, element, "batch input List not interned")?;
                Ok(spec(
                    vec![P::Exact(Type::Batch)],
                    tuple_ty(types, &[Type::U64, input], "batch item tuple not interned")?,
                    "Batch.next",
                    Some(BATCH_NEXT_SYMBOL),
                ))
            }
            BatchPush(element) => Ok(spec(
                vec![
                    P::Exact(Type::Batch),
                    P::Exact(Type::U64),
                    P::Exact(list_ty(types, element, "batch output List not interned")?),
                ],
                Type::Unit,
                "Batch.push",
                Some(BATCH_PUSH_SYMBOL),
            )),
            BatchFinish(element) => Ok(spec(
                vec![P::Exact(Type::Batch)],
                list_ty(types, element, "batch output List not interned")?,
                "Batch.finish",
                Some(BATCH_FINISH_SYMBOL),
            )),
            SeqNew => Ok(spec(
                vec![],
                Type::SeqBuilder,
                "Seq.new",
                Some(SEQ_NEW_SYMBOL),
            )),
            SeqPush(element) => Ok(spec(
                vec![
                    P::Exact(Type::SeqBuilder),
                    P::Exact(list_ty(types, element, "seq output List not interned")?),
                ],
                Type::Unit,
                "Seq.push",
                Some(SEQ_PUSH_SYMBOL),
            )),
            SeqFinish(element) => Ok(spec(
                vec![P::Exact(Type::SeqBuilder)],
                list_ty(types, element, "seq output List not interned")?,
                "Seq.finish",
                Some(SEQ_FINISH_SYMBOL),
            )),
            BytesCursorNew => Ok(spec(
                vec![P::Exact(Type::Bytes)],
                Type::BytesCursor,
                "Bytes.iter",
                Some(BYTES_CURSOR_NEW_SYMBOL),
            )),
            BytesCursorAdvance => Ok(spec(
                vec![P::Exact(Type::BytesCursor)],
                cursor_option(
                    types,
                    Type::U8,
                    Type::BytesCursor,
                    "bytes cursor pair not interned",
                    "bytes cursor Option not interned",
                )?,
                "BytesCursor.advance",
                Some(BYTES_CURSOR_STEP_SYMBOL),
            )),
            MapEntriesCursorNew(key, value) => Ok(spec(
                vec![P::Exact(Type::Map(map_ty(
                    types,
                    key,
                    value,
                    "map cursor input Map not interned",
                )?))],
                Type::MapCursor(map_ty(
                    types,
                    key,
                    value,
                    "map cursor input Map not interned",
                )?),
                "Map.entries",
                Some(MAP_CURSOR_NEW_SYMBOL),
            )),
            MapKeysCursorNew(key, value) => Ok(spec(
                vec![P::Exact(Type::Map(map_ty(
                    types,
                    key,
                    value,
                    "map cursor input Map not interned",
                )?))],
                Type::MapKeyCursor(map_ty(
                    types,
                    key,
                    value,
                    "map cursor input Map not interned",
                )?),
                "Map.keys",
                Some(MAP_CURSOR_NEW_SYMBOL),
            )),
            MapValuesCursorNew(key, value) => Ok(spec(
                vec![P::Exact(Type::Map(map_ty(
                    types,
                    key,
                    value,
                    "map cursor input Map not interned",
                )?))],
                Type::MapValueCursor(map_ty(
                    types,
                    key,
                    value,
                    "map cursor input Map not interned",
                )?),
                "Map.values",
                Some(MAP_CURSOR_NEW_SYMBOL),
            )),
            MapCursorAdvance(key, value) => {
                let receiver = arguments
                    .first()
                    .copied()
                    .ok_or_else(|| intern_err("map cursor advance requires a receiver"))?;
                if !P::MapCursorFamily(key, value).matches(receiver, types) {
                    return Err(intern_err("invalid map cursor advance receiver"));
                }
                let item = match receiver {
                    Type::MapCursor(_) => {
                        tuple_ty(types, &[key, value], "map cursor pair not interned")?
                    }
                    Type::MapKeyCursor(_) => key,
                    _ => value,
                };
                Ok(spec(
                    vec![P::MapCursorFamily(key, value)],
                    cursor_option(
                        types,
                        item,
                        receiver,
                        "map cursor result not interned",
                        "map cursor Option not interned",
                    )?,
                    "MapCursor.advance",
                    Some(MAP_CURSOR_STEP_SYMBOL),
                ))
            }
            Panic => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::Unit,
                "panic",
                Some(PANIC_SYMBOL),
            )),
            CownNew(payload) => Ok(spec(
                vec![P::Exact(payload)],
                Type::Cown(
                    types
                        .cown_id(payload)
                        .ok_or_else(|| intern_err("MIR Cown payload type was not interned"))?,
                ),
                "Cown.new",
                Some(COWN_NEW_SYMBOL),
            )),
            CownAcquire(payload) => Ok(spec(
                vec![P::Exact(Type::Cown(types.cown_id(payload).ok_or_else(
                    || intern_err("MIR Cown payload type was not interned"),
                )?))],
                payload,
                "Cown.acquire",
                Some(COWN_ACQUIRE_SYMBOL),
            )),
            CownAcquireMany => Ok(RuntimeIntrinsicSpec {
                parameters: Vec::new(),
                variadic: Some(P::Cown),
                result: Type::Unit,
                arg_ownership: Vec::new(),
                symbol: Some(COWN_ACQUIRE_MANY_SYMBOL),
                label: "Cown.acquire_many",
            }),
            CownPayload(payload) => Ok(spec(
                vec![P::Exact(Type::Cown(types.cown_id(payload).ok_or_else(
                    || intern_err("MIR Cown payload type was not interned"),
                )?))],
                payload,
                "Cown.payload",
                Some(COWN_PAYLOAD_SYMBOL),
            )),
            CownRelease => Ok(spec(
                vec![P::Cown],
                Type::Unit,
                "Cown.release",
                Some(COWN_RELEASE_SYMBOL),
            )),
            BytesNew => Ok(spec(
                vec![],
                Type::Bytes,
                "Bytes.new",
                Some(BYTES_NEW_SYMBOL),
            )),
            BytesFromString => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::Bytes,
                "Bytes.from_string",
                Some(BYTES_FROM_STRING_SYMBOL),
            )),
            BytesIsEmpty => Ok(spec(
                vec![P::Exact(Type::Bytes)],
                Type::Bool,
                "Bytes.is_empty",
                Some(BYTES_IS_EMPTY_SYMBOL),
            )),
            BytesLength => Ok(spec(
                vec![P::Exact(Type::Bytes)],
                Type::U64,
                "Bytes.length",
                Some(BYTES_LENGTH_SYMBOL),
            )),
            BytesGet => Ok(spec(
                vec![P::Exact(Type::Bytes), P::Exact(Type::U64)],
                option_ty(types, Type::U8, "MIR Bytes get Option was not interned")?,
                "Bytes.get",
                Some(BYTES_GET_SYMBOL),
            )),
            BytesSlice => Ok(spec(
                vec![
                    P::Exact(Type::Bytes),
                    P::Exact(Type::U64),
                    P::Exact(Type::U64),
                ],
                option_ty(
                    types,
                    Type::Bytes,
                    "MIR Bytes slice Option was not interned",
                )?,
                "Bytes.slice",
                Some(BYTES_SLICE_SYMBOL),
            )),
            BytesConcat => Ok(spec(
                vec![P::Exact(Type::Bytes), P::Exact(Type::Bytes)],
                Type::Bytes,
                "Bytes.concat",
                Some(BYTES_CONCAT_SYMBOL),
            )),
            BytesToString => Ok(spec(
                vec![P::Exact(Type::Bytes)],
                option_ty(
                    types,
                    Type::String,
                    "MIR Bytes String Option was not interned",
                )?,
                "Bytes.to_string",
                Some(BYTES_TO_STRING_SYMBOL),
            )),
            MutBytesNew => Ok(spec(
                vec![],
                Type::MutBytes,
                "MutBytes.new",
                Some(MUT_BYTES_NEW_SYMBOL),
            )),
            MutBytesWithCapacity => Ok(spec(
                vec![P::Exact(Type::U64)],
                Type::MutBytes,
                "MutBytes.with_capacity",
                Some(MUT_BYTES_WITH_CAPACITY_SYMBOL),
            )),
            MutBytesFromBytes => Ok(spec(
                vec![P::Exact(Type::Bytes)],
                Type::MutBytes,
                "MutBytes.from_bytes",
                Some(MUT_BYTES_FROM_BYTES_SYMBOL),
            )),
            MutBytesLength => Ok(spec(
                vec![P::Exact(Type::MutBytes)],
                Type::U64,
                "MutBytes.length",
                Some(MUT_BYTES_LENGTH_SYMBOL),
            )),
            MutBytesCapacity => Ok(spec(
                vec![P::Exact(Type::MutBytes)],
                Type::U64,
                "MutBytes.capacity",
                Some(MUT_BYTES_CAPACITY_SYMBOL),
            )),
            MutBytesPush => Ok(spec(
                vec![P::Exact(Type::MutBytes), P::Exact(Type::U8)],
                Type::Unit,
                "MutBytes.push",
                Some(MUT_BYTES_PUSH_SYMBOL),
            )),
            MutBytesGet => Ok(spec(
                vec![P::Exact(Type::MutBytes), P::Exact(Type::U64)],
                option_ty(types, Type::U8, "MIR MutBytes get Option was not interned")?,
                "MutBytes.get",
                Some(MUT_BYTES_GET_SYMBOL),
            )),
            MutBytesSet => Ok(spec(
                vec![
                    P::Exact(Type::MutBytes),
                    P::Exact(Type::U64),
                    P::Exact(Type::U8),
                ],
                Type::Unit,
                "MutBytes.set",
                Some(MUT_BYTES_SET_SYMBOL),
            )),
            MutBytesPop => Ok(spec(
                vec![P::Exact(Type::MutBytes)],
                option_ty(types, Type::U8, "MIR MutBytes pop Option was not interned")?,
                "MutBytes.pop",
                Some(MUT_BYTES_POP_SYMBOL),
            )),
            MutBytesClear => Ok(spec(
                vec![P::Exact(Type::MutBytes)],
                Type::Unit,
                "MutBytes.clear",
                Some(MUT_BYTES_CLEAR_SYMBOL),
            )),
            MutBytesReserve => Ok(spec(
                vec![P::Exact(Type::MutBytes), P::Exact(Type::U64)],
                Type::Unit,
                "MutBytes.reserve",
                Some(MUT_BYTES_RESERVE_SYMBOL),
            )),
            MutBytesToBytes => Ok(spec(
                vec![P::Exact(Type::MutBytes)],
                Type::Bytes,
                "MutBytes.to_bytes",
                Some(MUT_BYTES_TO_BYTES_SYMBOL),
            )),
            MutBytesExtend => Ok(spec(
                vec![P::Exact(Type::MutBytes), P::Exact(Type::Bytes)],
                Type::Unit,
                "MutBytes.extend",
                Some(MUT_BYTES_EXTEND_SYMBOL),
            )),
            ListEmpty(element) => Ok(spec(
                vec![],
                list_ty(types, element, "MIR List element type was not interned")?,
                "List.empty",
                None,
            )),
            ListCons(element) => {
                let list = list_ty(types, element, "MIR List element type was not interned")?;
                Ok(spec(
                    vec![P::Exact(element), P::Exact(list)],
                    list,
                    "List.cons",
                    Some(LIST_CONS_SYMBOL),
                ))
            }
            ListIsEmpty => Ok(spec(vec![P::List], Type::Bool, "List.is_empty", None)),
            ListHead(element) => Ok(spec(
                vec![P::List],
                option_ty(types, element, "MIR List head Option type was not interned")?,
                "List.head",
                Some(LIST_HEAD_SYMBOL),
            )),
            ListTail(element) => {
                let list = list_ty(types, element, "MIR List element type was not interned")?;
                Ok(spec(
                    vec![P::List],
                    option_ty(types, list, "MIR List tail Option type was not interned")?,
                    "List.tail",
                    Some(LIST_TAIL_SYMBOL),
                ))
            }
            ListLength => Ok(spec(
                vec![P::List],
                Type::U64,
                "List.length",
                Some(LIST_LENGTH_SYMBOL),
            )),
            ListReverse(element) => Ok(spec(
                vec![P::List],
                list_ty(types, element, "MIR List element type was not interned")?,
                "List.reverse",
                Some(LIST_REVERSE_SYMBOL),
            )),
            MutListNew(element) => Ok(spec(
                vec![],
                Type::MutList(
                    types
                        .list_id(element)
                        .ok_or_else(|| intern_err("MIR MutList element type was not interned"))?,
                ),
                "MutList.new",
                Some(MUT_LIST_NEW_SYMBOL),
            )),
            MutListPush(element) => Ok(spec(
                vec![P::MutList, P::Exact(element)],
                Type::Unit,
                "MutList.push",
                Some(MUT_LIST_PUSH_SYMBOL),
            )),
            MutListGet(element) => Ok(spec(
                vec![P::MutList, P::Integer],
                option_ty(types, element, "MIR MutList get Option was not interned")?,
                "MutList.get",
                Some(MUT_LIST_GET_SYMBOL),
            )),
            MutListSet(element) => Ok(spec(
                vec![P::MutList, P::Integer, P::Exact(element)],
                Type::Unit,
                "MutList.set",
                Some(MUT_LIST_SET_SYMBOL),
            )),
            MutListPop(element) => Ok(spec(
                vec![P::MutList],
                option_ty(types, element, "MIR MutList pop Option was not interned")?,
                "MutList.pop",
                Some(MUT_LIST_POP_SYMBOL),
            )),
            MutListLength => Ok(spec(
                vec![P::MutList],
                Type::U64,
                "MutList.length",
                Some(MUT_LIST_LENGTH_SYMBOL),
            )),
            MutListCapacity => Ok(spec(
                vec![P::MutList],
                Type::U64,
                "MutList.capacity",
                Some(MUT_LIST_CAPACITY_SYMBOL),
            )),
            MutListIntoIter(element) => Ok(spec(
                vec![P::MutList],
                Type::MutListCursor(
                    types
                        .list_id(element)
                        .ok_or_else(|| intern_err("MIR MutListCursor type was not interned"))?,
                ),
                "MutList.into_iter",
                Some(MUT_LIST_CURSOR_NEW_SYMBOL),
            )),
            MutListCursorAdvance(element) => {
                let cursor = Type::MutListCursor(
                    types
                        .list_id(element)
                        .ok_or_else(|| intern_err("MIR MutListCursor type was not interned"))?,
                );
                Ok(spec(
                    vec![P::MutListCursor],
                    cursor_option(
                        types,
                        element,
                        cursor,
                        "MIR MutListCursor pair was not interned",
                        "MIR MutListCursor Option was not interned",
                    )?,
                    "MutListCursor.advance",
                    Some(MUT_LIST_CURSOR_STEP_SYMBOL),
                ))
            }
            MutListToList(element) => Ok(spec(
                vec![P::MutList],
                list_ty(types, element, "MIR MutList to_list List was not interned")?,
                "MutList.to_list",
                Some(MUT_LIST_TO_LIST_SYMBOL),
            )),
            MutMapNew(key, value) => Ok(spec(
                vec![],
                Type::MutMap(map_ty(
                    types,
                    key,
                    value,
                    "MIR MutMap type was not interned",
                )?),
                "MutMap.new",
                Some(MUT_MAP_NEW_SYMBOL),
            )),
            MutMapInsert(key, value) => Ok(spec(
                vec![P::MutMap, P::Exact(key), P::Exact(value)],
                option_ty(types, value, "MIR MutMap insert Option was not interned")?,
                "MutMap.insert",
                Some(MUT_MAP_INSERT_SYMBOL),
            )),
            MutMapGet(key, value) => Ok(spec(
                vec![P::MutMap, P::Exact(key)],
                option_ty(types, value, "MIR MutMap Option was not interned")?,
                "MutMap.get",
                Some(MUT_MAP_GET_SYMBOL),
            )),
            MutMapRemove(key, value) => Ok(spec(
                vec![P::MutMap, P::Exact(key)],
                option_ty(types, value, "MIR MutMap Option was not interned")?,
                "MutMap.remove",
                Some(MUT_MAP_REMOVE_SYMBOL),
            )),
            MutMapContainsKey(key) => Ok(spec(
                vec![P::MutMap, P::Exact(key)],
                Type::Bool,
                "MutMap.contains_key",
                Some(MUT_MAP_CONTAINS_KEY_SYMBOL),
            )),
            MutMapLength => Ok(spec(
                vec![P::MutMap],
                Type::U64,
                "MutMap.length",
                Some(MUT_MAP_LENGTH_SYMBOL),
            )),
            MutMapCapacity => Ok(spec(
                vec![P::MutMap],
                Type::U64,
                "MutMap.capacity",
                Some(MUT_MAP_CAPACITY_SYMBOL),
            )),
            MutMapIsEmpty => Ok(spec(
                vec![P::MutMap],
                Type::Bool,
                "MutMap.is_empty",
                Some(MUT_MAP_IS_EMPTY_SYMBOL),
            )),
            MutMapIntoIter(key, value) => Ok(spec(
                vec![P::MutMap],
                Type::MutMapCursor(map_ty(
                    types,
                    key,
                    value,
                    "MIR MutMapCursor type was not interned",
                )?),
                "MutMap.into_iter",
                Some(MUT_MAP_CURSOR_NEW_SYMBOL),
            )),
            MutMapCursorAdvance(key, value) => {
                let cursor = Type::MutMapCursor(map_ty(
                    types,
                    key,
                    value,
                    "MIR MutMapCursor type was not interned",
                )?);
                let item = tuple_ty(
                    types,
                    &[key, value],
                    "MIR MutMapCursor entry was not interned",
                )?;
                Ok(spec(
                    vec![P::MutMapCursor],
                    cursor_option(
                        types,
                        item,
                        cursor,
                        "MIR MutMapCursor pair was not interned",
                        "MIR MutMapCursor Option was not interned",
                    )?,
                    "MutMapCursor.advance",
                    Some(MUT_MAP_CURSOR_STEP_SYMBOL),
                ))
            }
            MutMapToList(key, value) => {
                let entry = tuple_ty(
                    types,
                    &[key, value],
                    "MIR MutMap to_list entry was not interned",
                )?;
                Ok(spec(
                    vec![P::MutMap],
                    list_ty(types, entry, "MIR MutMap to_list List was not interned")?,
                    "MutMap.to_list",
                    Some(MUT_MAP_TO_LIST_SYMBOL),
                ))
            }
            MutSetNew(element) => Ok(spec(
                vec![],
                Type::MutSet(map_ty(
                    types,
                    element,
                    Type::Bool,
                    "MIR MutSet type was not interned",
                )?),
                "MutSet.new",
                Some(MUT_MAP_NEW_SYMBOL),
            )),
            MutSetAdd(element) => Ok(spec(
                vec![P::MutSet, P::Exact(element)],
                Type::Bool,
                "MutSet.add",
                Some(MUT_MAP_INSERT_SYMBOL),
            )),
            MutSetRemove(element) => Ok(spec(
                vec![P::MutSet, P::Exact(element)],
                Type::Bool,
                "MutSet.remove",
                Some(MUT_MAP_REMOVE_SYMBOL),
            )),
            MutSetContains(element) => Ok(spec(
                vec![P::MutSet, P::Exact(element)],
                Type::Bool,
                "MutSet.contains",
                Some(MUT_MAP_CONTAINS_KEY_SYMBOL),
            )),
            MutSetLength => Ok(spec(
                vec![P::MutSet],
                Type::U64,
                "MutSet.length",
                Some(MUT_MAP_LENGTH_SYMBOL),
            )),
            MutSetCapacity => Ok(spec(
                vec![P::MutSet],
                Type::U64,
                "MutSet.capacity",
                Some(MUT_MAP_CAPACITY_SYMBOL),
            )),
            MutSetIsEmpty => Ok(spec(
                vec![P::MutSet],
                Type::Bool,
                "MutSet.is_empty",
                Some(MUT_MAP_IS_EMPTY_SYMBOL),
            )),
            MutSetIntoIter(element) => Ok(spec(
                vec![P::MutSet],
                Type::MutSetCursor(map_ty(
                    types,
                    element,
                    Type::Bool,
                    "MIR MutSetCursor type was not interned",
                )?),
                "MutSet.into_iter",
                Some(MUT_MAP_CURSOR_NEW_SYMBOL),
            )),
            MutSetCursorAdvance(element) => {
                let cursor = Type::MutSetCursor(map_ty(
                    types,
                    element,
                    Type::Bool,
                    "MIR MutSetCursor type was not interned",
                )?);
                Ok(spec(
                    vec![P::MutSetCursor],
                    cursor_option(
                        types,
                        element,
                        cursor,
                        "MIR MutSetCursor pair was not interned",
                        "MIR MutSetCursor Option was not interned",
                    )?,
                    "MutSetCursor.advance",
                    Some(MUT_MAP_CURSOR_STEP_SYMBOL),
                ))
            }
            MutSetToList(element) => Ok(spec(
                vec![P::MutSet],
                list_ty(types, element, "MIR MutSet to_list List was not interned")?,
                "MutSet.to_list",
                Some(MUT_MAP_TO_LIST_SYMBOL),
            )),
            MapEmpty(key, value) => Ok(spec(
                vec![],
                Type::Map(map_ty(types, key, value, "MIR Map type was not interned")?),
                "Map.empty",
                None,
            )),
            MapInsert(key, value) => Ok(spec(
                vec![P::Map, P::Exact(key), P::Exact(value)],
                Type::Map(map_ty(types, key, value, "MIR Map type was not interned")?),
                "Map.insert",
                Some(MAP_INSERT_SYMBOL),
            )),
            MapRemove(key, value) => Ok(spec(
                vec![P::Map, P::Exact(key)],
                Type::Map(map_ty(types, key, value, "MIR Map type was not interned")?),
                "Map.remove",
                Some(MAP_REMOVE_SYMBOL),
            )),
            MapGet(key, value) => Ok(spec(
                vec![P::Map, P::Exact(key)],
                option_ty(types, value, "MIR Map get Option was not interned")?,
                "Map.get",
                Some(MAP_GET_SYMBOL),
            )),
            MapContainsKey(key) => Ok(spec(
                vec![P::Map, P::Exact(key)],
                Type::Bool,
                "Map.contains_key",
                Some(MAP_CONTAINS_KEY_SYMBOL),
            )),
            MapLength => Ok(spec(
                vec![P::Map],
                Type::U64,
                "Map.length",
                Some(MAP_LENGTH_SYMBOL),
            )),
            MapIsEmpty => Ok(spec(vec![P::Map], Type::Bool, "Map.is_empty", None)),
            Show(ty) if matches!(ty, Type::String | Type::Bool) || ty.is_numeric() => {
                Ok(spec(vec![P::Exact(ty)], Type::String, "Show.show", None))
            }
            Show(_) => Err(intern_err("invalid Show operand")),
            Echo => Ok(spec(
                vec![P::Exact(Type::String), P::Exact(Type::String)],
                Type::Unit,
                "echo",
                Some(ECHO_SYMBOL),
            )),
            Print => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::Unit,
                "print",
                Some(PRINT_SYMBOL),
            )),
            Println => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::Unit,
                "print",
                Some(PRINTLN_SYMBOL),
            )),
            StringConcat => Ok(spec(
                vec![P::Exact(Type::String), P::Exact(Type::String)],
                Type::String,
                "String concat",
                Some(STRING_CONCAT_SYMBOL),
            )),
            StringEqual | StringNotEqual => Ok(spec(
                vec![P::Exact(Type::String), P::Exact(Type::String)],
                Type::Bool,
                "String equality",
                Some(STRING_EQ_SYMBOL),
            )),
            StringIsEmpty => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::Bool,
                "String is_empty",
                None,
            )),
            StringByteCount => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::U64,
                "String byte_count",
                Some(STRING_LEN_SYMBOL),
            )),
            StringStartsWith => Ok(spec(
                vec![P::Exact(Type::String), P::Exact(Type::String)],
                Type::Bool,
                "String starts_with",
                Some(STRING_STARTS_WITH_SYMBOL),
            )),
            StringEndsWith => Ok(spec(
                vec![P::Exact(Type::String), P::Exact(Type::String)],
                Type::Bool,
                "String ends_with",
                Some(STRING_ENDS_WITH_SYMBOL),
            )),
            StringContains => Ok(spec(
                vec![P::Exact(Type::String), P::Exact(Type::String)],
                Type::Bool,
                "String contains",
                Some(STRING_CONTAINS_SYMBOL),
            )),
            StringScalarCount => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::U64,
                "String scalar_count",
                Some(STRING_SCALAR_COUNT_SYMBOL),
            )),
            StringGraphemeCount => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::U64,
                "String grapheme_count",
                Some(STRING_GRAPHEME_COUNT_SYMBOL),
            )),
            StringIsAscii => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::Bool,
                "String is_ascii",
                Some(STRING_IS_ASCII_SYMBOL),
            )),
            StringTrim => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::String,
                "String trim",
                Some(STRING_TRIM_SYMBOL),
            )),
            StringToUpper => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::String,
                "String to_upper",
                Some(STRING_TO_UPPER_SYMBOL),
            )),
            StringToLower => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::String,
                "String to_lower",
                Some(STRING_TO_LOWER_SYMBOL),
            )),
            Path(op) => {
                let (parameters, result) = crate::sema::path_intrinsics::signature(op, types)
                    .ok_or_else(|| Diagnostic::codegen("invalid path intrinsic signature"))?;
                Ok(spec(
                    parameters.into_iter().map(P::Exact).collect(),
                    result,
                    "path",
                    Some(PATH_CALL_SYMBOL),
                ))
            }
            StringSplit => Ok(spec(
                vec![P::Exact(Type::String), P::Exact(Type::String)],
                list_ty(
                    types,
                    Type::String,
                    "MIR String split List was not interned",
                )?,
                "String split",
                Some(STRING_SPLIT_SYMBOL),
            )),
            StringReplace => Ok(spec(
                vec![
                    P::Exact(Type::String),
                    P::Exact(Type::String),
                    P::Exact(Type::String),
                ],
                Type::String,
                "String replace",
                Some(STRING_REPLACE_SYMBOL),
            )),
            StringGetByte => Ok(spec(
                vec![P::Exact(Type::String), P::Exact(Type::U64)],
                option_ty(
                    types,
                    Type::U8,
                    "MIR String get_byte Option was not interned",
                )?,
                "String get_byte",
                Some(STRING_GET_BYTE_SYMBOL),
            )),
            StringSlice => Ok(spec(
                vec![
                    P::Exact(Type::String),
                    P::Exact(Type::U64),
                    P::Exact(Type::U64),
                ],
                option_ty(
                    types,
                    Type::String,
                    "MIR String slice Option was not interned",
                )?,
                "String slice",
                Some(STRING_SLICE_SYMBOL),
            )),
            StringParse(target) => {
                Ok(spec(
                    vec![P::Exact(Type::String)],
                    Type::Result(types.result_id(target, Type::String).ok_or_else(|| {
                        intern_err("MIR String parse result type was not interned")
                    })?),
                    "String parse",
                    None,
                ))
            }
            StringAsCString => Ok(spec(
                vec![P::Exact(Type::String)],
                Type::CStr,
                "String as_cstr",
                Some(STRING_C_STRING_CHECK_SYMBOL),
            )),
        }
    }

    pub(crate) fn runtime_symbol(self) -> Option<&'static str> {
        match self {
            Self::HasherNew => Some(HASHER_NEW_SYMBOL),
            Self::HasherFinish => Some(HASHER_FINISH_SYMBOL),
            Self::Panic => Some(PANIC_SYMBOL),
            Self::Print => Some(PRINT_SYMBOL),
            Self::Println => Some(PRINTLN_SYMBOL),
            Self::Echo => Some(ECHO_SYMBOL),
            Self::CownNew(_) => Some(COWN_NEW_SYMBOL),
            Self::CownAcquire(_) => Some(COWN_ACQUIRE_SYMBOL),
            Self::CownAcquireMany => Some(COWN_ACQUIRE_MANY_SYMBOL),
            Self::CownPayload(_) => Some(COWN_PAYLOAD_SYMBOL),
            Self::CownRelease => Some(COWN_RELEASE_SYMBOL),
            Self::BytesNew => Some(BYTES_NEW_SYMBOL),
            Self::BytesFromString => Some(BYTES_FROM_STRING_SYMBOL),
            Self::BytesIsEmpty => Some(BYTES_IS_EMPTY_SYMBOL),
            Self::BytesLength => Some(BYTES_LENGTH_SYMBOL),
            Self::BytesGet => Some(BYTES_GET_SYMBOL),
            Self::BytesSlice => Some(BYTES_SLICE_SYMBOL),
            Self::BytesConcat => Some(BYTES_CONCAT_SYMBOL),
            Self::BytesToString => Some(BYTES_TO_STRING_SYMBOL),
            Self::BytesCursorNew => Some(BYTES_CURSOR_NEW_SYMBOL),
            Self::BytesCursorAdvance => Some(BYTES_CURSOR_STEP_SYMBOL),
            Self::MutBytesNew => Some(MUT_BYTES_NEW_SYMBOL),
            Self::MutBytesWithCapacity => Some(MUT_BYTES_WITH_CAPACITY_SYMBOL),
            Self::MutBytesFromBytes => Some(MUT_BYTES_FROM_BYTES_SYMBOL),
            Self::MutBytesLength => Some(MUT_BYTES_LENGTH_SYMBOL),
            Self::MutBytesCapacity => Some(MUT_BYTES_CAPACITY_SYMBOL),
            Self::MutBytesPush => Some(MUT_BYTES_PUSH_SYMBOL),
            Self::MutBytesGet => Some(MUT_BYTES_GET_SYMBOL),
            Self::MutBytesSet => Some(MUT_BYTES_SET_SYMBOL),
            Self::MutBytesPop => Some(MUT_BYTES_POP_SYMBOL),
            Self::MutBytesClear => Some(MUT_BYTES_CLEAR_SYMBOL),
            Self::MutBytesReserve => Some(MUT_BYTES_RESERVE_SYMBOL),
            Self::MutBytesToBytes => Some(MUT_BYTES_TO_BYTES_SYMBOL),
            Self::MutBytesExtend => Some(MUT_BYTES_EXTEND_SYMBOL),
            Self::ListCons(_) => Some(LIST_CONS_SYMBOL),
            Self::ListHead(_) => Some(LIST_HEAD_SYMBOL),
            Self::ListTail(_) => Some(LIST_TAIL_SYMBOL),
            Self::ListLength => Some(LIST_LENGTH_SYMBOL),
            Self::ListReverse(_) => Some(LIST_REVERSE_SYMBOL),
            Self::MutListNew(_) => Some(MUT_LIST_NEW_SYMBOL),
            Self::MutListPush(_) => Some(MUT_LIST_PUSH_SYMBOL),
            Self::MutListGet(_) => Some(MUT_LIST_GET_SYMBOL),
            Self::MutListSet(_) => Some(MUT_LIST_SET_SYMBOL),
            Self::MutListPop(_) => Some(MUT_LIST_POP_SYMBOL),
            Self::MutListLength => Some(MUT_LIST_LENGTH_SYMBOL),
            Self::MutListCapacity => Some(MUT_LIST_CAPACITY_SYMBOL),
            Self::MutListIntoIter(_) => Some(MUT_LIST_CURSOR_NEW_SYMBOL),
            Self::MutListCursorAdvance(_) => Some(MUT_LIST_CURSOR_STEP_SYMBOL),
            Self::MutListToList(_) => Some(MUT_LIST_TO_LIST_SYMBOL),
            Self::MapInsert(_, _) => Some(MAP_INSERT_SYMBOL),
            Self::MapGet(_, _) => Some(MAP_GET_SYMBOL),
            Self::MapRemove(_, _) => Some(MAP_REMOVE_SYMBOL),
            Self::MapContainsKey(_) => Some(MAP_CONTAINS_KEY_SYMBOL),
            Self::MapLength => Some(MAP_LENGTH_SYMBOL),
            Self::MapEntriesCursorNew(_, _)
            | Self::MapKeysCursorNew(_, _)
            | Self::MapValuesCursorNew(_, _) => Some(MAP_CURSOR_NEW_SYMBOL),
            Self::MapCursorAdvance(_, _) => Some(MAP_CURSOR_STEP_SYMBOL),
            Self::MutMapNew(_, _) | Self::MutSetNew(_) => Some(MUT_MAP_NEW_SYMBOL),
            Self::MutMapInsert(_, _) | Self::MutSetAdd(_) => Some(MUT_MAP_INSERT_SYMBOL),
            Self::MutMapGet(_, _) => Some(MUT_MAP_GET_SYMBOL),
            Self::MutMapRemove(_, _) | Self::MutSetRemove(_) => Some(MUT_MAP_REMOVE_SYMBOL),
            Self::MutMapContainsKey(_) | Self::MutSetContains(_) => {
                Some(MUT_MAP_CONTAINS_KEY_SYMBOL)
            }
            Self::MutMapLength | Self::MutSetLength => Some(MUT_MAP_LENGTH_SYMBOL),
            Self::MutMapCapacity | Self::MutSetCapacity => Some(MUT_MAP_CAPACITY_SYMBOL),
            Self::MutMapIsEmpty | Self::MutSetIsEmpty => Some(MUT_MAP_IS_EMPTY_SYMBOL),
            Self::MutMapIntoIter(_, _) | Self::MutSetIntoIter(_) => Some(MUT_MAP_CURSOR_NEW_SYMBOL),
            Self::MutMapCursorAdvance(_, _) | Self::MutSetCursorAdvance(_) => {
                Some(MUT_MAP_CURSOR_STEP_SYMBOL)
            }
            Self::MutMapToList(_, _) | Self::MutSetToList(_) => Some(MUT_MAP_TO_LIST_SYMBOL),
            Self::BatchNew(_) => Some(BATCH_NEW_SYMBOL),
            Self::BatchWorker => Some(BATCH_WORKER_SYMBOL),
            Self::BatchNext(_) => Some(BATCH_NEXT_SYMBOL),
            Self::BatchPush(_) => Some(BATCH_PUSH_SYMBOL),
            Self::BatchFinish(_) => Some(BATCH_FINISH_SYMBOL),
            Self::SeqNew => Some(SEQ_NEW_SYMBOL),
            Self::SeqPush(_) => Some(SEQ_PUSH_SYMBOL),
            Self::SeqFinish(_) => Some(SEQ_FINISH_SYMBOL),
            Self::CCallbackNew(_) => Some(CALLBACK_NEW_SYMBOL),
            Self::CCallbackFunction => Some(CALLBACK_FUNCTION_SYMBOL),
            Self::CCallbackContext => Some(CALLBACK_CONTEXT_SYMBOL),
            Self::CCallbackFailed => Some(CALLBACK_FAILED_SYMBOL),
            Self::CCellAlloc(_) => Some(C_ALLOC_SYMBOL),
            Self::CCellFree => Some(C_FREE_SYMBOL),
            Self::CStrAsString => Some(STRING_FROM_CSTR_SYMBOL),
            Self::StringConcat => Some(STRING_CONCAT_SYMBOL),
            Self::StringEqual | Self::StringNotEqual => Some(STRING_EQ_SYMBOL),
            Self::StringByteCount => Some(STRING_LEN_SYMBOL),
            Self::StringStartsWith => Some(STRING_STARTS_WITH_SYMBOL),
            Self::StringEndsWith => Some(STRING_ENDS_WITH_SYMBOL),
            Self::StringContains => Some(STRING_CONTAINS_SYMBOL),
            Self::StringScalarCount => Some(STRING_SCALAR_COUNT_SYMBOL),
            Self::StringGraphemeCount => Some(STRING_GRAPHEME_COUNT_SYMBOL),
            Self::StringIsAscii => Some(STRING_IS_ASCII_SYMBOL),
            Self::StringTrim => Some(STRING_TRIM_SYMBOL),
            Self::StringToUpper => Some(STRING_TO_UPPER_SYMBOL),
            Self::StringToLower => Some(STRING_TO_LOWER_SYMBOL),
            Self::StringSplit => Some(STRING_SPLIT_SYMBOL),
            Self::Path(_) => Some(PATH_CALL_SYMBOL),
            Self::StringReplace => Some(STRING_REPLACE_SYMBOL),
            Self::StringGetByte => Some(STRING_GET_BYTE_SYMBOL),
            Self::StringSlice => Some(STRING_SLICE_SYMBOL),
            Self::StringAsCString => Some(STRING_C_STRING_CHECK_SYMBOL),
            Self::DebugString => Some(DEBUG_STRING_SYMBOL),
            Self::DebugBytes => Some(DEBUG_BYTES_SYMBOL),
            Self::DebugDuration => Some(DEBUG_DURATION_SYMBOL),
            Self::DebugPath(_) => Some(DEBUG_PATH_SYMBOL),
            Self::DebugPathStatus => Some(DEBUG_PATH_STATUS_SYMBOL),
            Self::DebugNativeId(_) => Some(DEBUG_NATIVE_ID_SYMBOL),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_symbols_are_declared_in_the_abi() {
        let samples = [
            RuntimeIntrinsic::Panic,
            RuntimeIntrinsic::Println,
            RuntimeIntrinsic::BytesLength,
            RuntimeIntrinsic::ListLength,
            RuntimeIntrinsic::MapLength,
            RuntimeIntrinsic::MutListLength,
            RuntimeIntrinsic::MutMapLength,
            RuntimeIntrinsic::BatchWorker,
            RuntimeIntrinsic::CownRelease,
        ];
        for intrinsic in samples {
            let symbol = intrinsic.runtime_symbol().expect("mapped symbol");
            assert!(
                joky_runtime_abi::symbols::ALL.contains(&symbol),
                "{symbol} missing from ABI symbol table"
            );
        }
    }
}
