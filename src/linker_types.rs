//! Relocate type-bearing MIR metadata at the module boundary.
use crate::mir::*;
use crate::sema::{Type, TypeRemap};

pub(crate) fn remap(function: &mut MirFunction, map: &TypeRemap) {
    let mut groups = crate::sema::EffectGroupSet::new();
    for group in function.declared_effects.iter() {
        groups.insert(map.groups[&group]);
    }
    function.declared_effects = groups;
    function.receiver = function.receiver.map(|t| map.ty(t));
    function.return_type = map.ty(function.return_type);
    for parameter in &mut function.parameters {
        parameter.ty = map.ty(parameter.ty);
    }
    for local in &mut function.locals {
        local.ty = map.ty(local.ty);
    }
    for ty in &mut function.value_types {
        *ty = map.ty(*ty);
    }
    for continuation in &mut function.continuations {
        continuation.operation = continuation.operation.map(|op| map.operation(op));
        for slot in &mut continuation.frame_slots {
            slot.ty = map.ty(slot.ty);
        }
    }
    for statement in function
        .blocks
        .iter_mut()
        .flat_map(|block| &mut block.statements)
    {
        match statement {
            MirStatement::Const {
                value: MirConstant::EffectOperation(operation),
                ..
            } => {
                *operation = map.operation(*operation);
            }
            MirStatement::Construct { type_id, .. }
            | MirStatement::EnumConstruct {
                enum_id: type_id, ..
            } => {
                let ty = match *type_id {
                    MirTypeId::Struct(i) => Type::Struct(i),
                    MirTypeId::Class(i) => Type::Class(i),
                    MirTypeId::Enum(i) => Type::Enum(i),
                    MirTypeId::Option(i) => Type::Option(i),
                    MirTypeId::Result(i) => Type::Result(i),
                };
                *type_id = MirTypeId::from_type(map.ty(ty)).unwrap();
            }
            MirStatement::MethodCall { receiver_type, .. } => {
                *receiver_type = map.ty(*receiver_type)
            }
            MirStatement::RuntimeCall { intrinsic, .. } => intrinsic_types(intrinsic, map),
            MirStatement::ResumableRequest { operation, .. }
            | MirStatement::HandlerRequest { operation, .. }
            | MirStatement::Suspend { operation, .. }
            | MirStatement::TaskAbort { operation, .. }
            | MirStatement::TaskFailurePayload { operation, .. } => {
                *operation = map.operation(*operation)
            }
            MirStatement::HandlerEnter { handlers } => {
                for arm in handlers {
                    arm.operation = map.operation(arm.operation);
                    arm.result_type = map.ty(arm.result_type);
                }
            }
            MirStatement::Unit { .. }
            | MirStatement::Const { .. }
            | MirStatement::Read { .. }
            | MirStatement::BorrowLocal { .. }
            | MirStatement::TakeLocal { .. }
            | MirStatement::Unary { .. }
            | MirStatement::Binary { .. }
            | MirStatement::Numeric { .. }
            | MirStatement::Call { .. }
            | MirStatement::FunctionValue { .. }
            | MirStatement::DynamicValue { .. }
            | MirStatement::DynamicUpcast { .. }
            | MirStatement::CallIndirect { .. }
            | MirStatement::Project { .. }
            | MirStatement::EnumTag { .. }
            | MirStatement::EnumProject { .. }
            | MirStatement::Tuple { .. }
            | MirStatement::Store { .. }
            | MirStatement::Resume { .. }
            | MirStatement::TaskPoll { .. }
            | MirStatement::TaskCancelled { .. }
            | MirStatement::TaskFailureOperation { .. }
            | MirStatement::TaskFailureClaim { .. }
            | MirStatement::TaskFailureRethrow { .. }
            | MirStatement::HandlerExit
            | MirStatement::ScopeEnter { .. }
            | MirStatement::ScopeExit { .. }
            | MirStatement::TaskCreate { .. }
            | MirStatement::TaskWait { .. }
            | MirStatement::CownAcquire { .. }
            | MirStatement::TaskJoin { .. }
            | MirStatement::TaskClaimResult { .. }
            | MirStatement::TaskCancel { .. }
            | MirStatement::RaceStart { .. }
            | MirStatement::RaceSelect { .. }
            | MirStatement::Dup { .. }
            | MirStatement::Move { .. }
            | MirStatement::Drop { .. }
            | MirStatement::Deinit { .. }
            | MirStatement::DropLocal { .. }
            | MirStatement::Bind { .. }
            | MirStatement::Phi { .. } => {}
        }
    }
}

fn intrinsic_types(intrinsic: &mut RuntimeIntrinsic, map: &TypeRemap) {
    use RuntimeIntrinsic::*;
    match intrinsic {
        Hash(t)
        | PartialCompare(t)
        | Compare(t)
        | PartialEqual(t)
        | CPointerNull(t)
        | BatchNew(t)
        | BatchNext(t)
        | BatchPush(t)
        | BatchFinish(t)
        | SeqPush(t)
        | SeqFinish(t)
        | CownNew(t)
        | CownAcquire(t)
        | CownPayload(t)
        | ListEmpty(t)
        | ListCons(t)
        | ListHead(t)
        | ListTail(t)
        | ListReverse(t)
        | MutListNew(t)
        | MutListPush(t)
        | MutListGet(t)
        | MutListSet(t)
        | MutListPop(t)
        | MutListIntoIter(t)
        | MutListCursorAdvance(t)
        | MutListToList(t)
        | MutMapContainsKey(t)
        | MutSetNew(t)
        | MutSetAdd(t)
        | MutSetRemove(t)
        | MutSetContains(t)
        | MutSetIntoIter(t)
        | MutSetCursorAdvance(t)
        | MutSetToList(t)
        | MapContainsKey(t)
        | Show(t)
        | DebugNativeId(t)
        | DebugPath(t)
        | StringParse(t)
        | CCellAlloc(t)
        | CCellRead(t)
        | CCellWrite(t)
        | CCallbackNew(t) => *t = map.ty(*t),
        MapEntriesCursorNew(a, b)
        | MapKeysCursorNew(a, b)
        | MapValuesCursorNew(a, b)
        | MapCursorAdvance(a, b)
        | MutMapNew(a, b)
        | MutMapInsert(a, b)
        | MutMapGet(a, b)
        | MutMapRemove(a, b)
        | MutMapIntoIter(a, b)
        | MutMapCursorAdvance(a, b)
        | MutMapToList(a, b)
        | MapEmpty(a, b)
        | MapInsert(a, b)
        | MapGet(a, b)
        | MapRemove(a, b) => {
            *a = map.ty(*a);
            *b = map.ty(*b);
        }
        HasherNew | HasherFinish | CPointerIsNull | BatchWorker | SeqNew | Panic
        | CownAcquireMany | CownRelease | BytesNew | BytesFromString | BytesIsEmpty
        | BytesLength | BytesGet | BytesSlice | BytesConcat | BytesToString | BytesCursorNew
        | BytesCursorAdvance | MutBytesNew | MutBytesWithCapacity | MutBytesFromBytes
        | MutBytesLength | MutBytesCapacity | MutBytesPush | MutBytesGet | MutBytesSet
        | MutBytesPop | MutBytesClear | MutBytesReserve | MutBytesToBytes | MutBytesExtend
        | ListIsEmpty | ListLength | MutListLength | MutListCapacity | MutMapLength
        | MutMapCapacity | MutMapIsEmpty | MutSetLength | MutSetCapacity | MutSetIsEmpty
        | MapLength | MapIsEmpty | Print | Println | DebugString | DebugBytes | DebugDuration
        | Echo | StringConcat | StringEqual | StringNotEqual | StringIsEmpty | StringByteCount
        | StringStartsWith | StringEndsWith | StringContains | StringScalarCount
        | StringGraphemeCount | StringIsAscii | StringTrim | StringToUpper | StringToLower
        | StringSplit | StringReplace | StringGetByte | StringSlice | StringAsCString
        | CStrAsString | CCellFree | CCallbackFunction | CCallbackContext | CCallbackClose
        | CCallbackFailed | DebugPathStatus | Path(_) => {}
    }
}
