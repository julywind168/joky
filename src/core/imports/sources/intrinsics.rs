//! Source annotations name operations; only trusted library kinds grant them identity.
use super::*;

fn parameter(f: &FnDef) -> Result<Type, Error> {
    let [parameter] = f.type_params.as_slice() else {
        return Err(error(
            f.span,
            ErrorCode::InvalidCall,
            "Compiler intrinsic requires exactly one type parameter",
        ));
    };
    Ok(Type::Parameter(parameter.clone()))
}

fn buffer_parameters(
    f: &FnDef,
    intrinsic: crate::parser::ast::BufferIntrinsic,
) -> Result<Type, Error> {
    if f.type_params.len() != intrinsic.type_parameter_count() {
        return Err(error(
            f.span,
            ErrorCode::InvalidCall,
            "Invalid compiler buffer type parameter count",
        ));
    }
    if intrinsic.is_bytes() {
        Ok(Type::UInt8)
    } else if intrinsic.is_pair() {
        Ok(Type::tuple(
            f.type_params.iter().cloned().map(Type::Parameter).collect(),
        ))
    } else {
        parameter(f)
    }
}

pub(super) fn bind_functions(module: &mut Module, kind: SourceKind) -> Result<(), Error> {
    use crate::parser::ast::{BufferIntrinsic, CompilerIntrinsic::*, DomainAccessKind, FloatKind};
    for (index, attribute) in std::mem::take(&mut module.intrinsic_attributes) {
        let Some(Item::FnDef(f)) = module.items.get_mut(index) else {
            return Err(error(
                attribute.span,
                ErrorCode::InvalidState,
                "Intrinsic annotation must refer to a function declaration",
            ));
        };
        let id = attribute.name.as_str();
        let mut adapter = false;
        let compiler = match (kind, id) {
            (SourceKind::FixedArrayStorage, "fixed_array.begin") => FixedArrayBegin,
            (SourceKind::FixedArrayStorage, "fixed_array.write_next") => FixedArrayWriteNext,
            (SourceKind::FixedArrayStorage, "fixed_array.finish") => FixedArrayFinish,
            (SourceKind::FixedArrayStorage, "fixed_array.load") => FixedArrayLoad,
            (SourceKind::FixedArrayStorage, "fixed_array.store") => FixedArrayStore,
            (
                SourceKind::FixedArrayStorage,
                "fixed_array.borrow_ref" | "fixed_array.borrow_mut_ref",
            ) => FixedArrayBorrow,
            (SourceKind::FixedArrayStorage, "fixed_array.slice" | "fixed_array.slice_mut") => {
                FixedArraySlice
            }
            (SourceKind::Arena, "arena.new") => ArenaNew,
            (SourceKind::Arena, "arena.alloc") => ArenaAlloc,
            (SourceKind::BufferLibrary, "outbox.emit") => OutboxEmit,
            (SourceKind::Check, "check.assert") => Assert,
            (SourceKind::Check, "check.panic") => Panic,
            (SourceKind::Text, "text.clone") => TextClone,
            (SourceKind::Text, "text.freeze_utf8") => TextFreeze,
            (SourceKind::Text, "text.into_bytes") => TextIntoBytes,
            (SourceKind::Text, "text.bytes") => TextBytes,
            (SourceKind::Integer, "integer.rune_from_valid_u32") => RuneFromValidU32,
            (SourceKind::Json, "json.parse_float64") => Float(FloatKind::FromText),
            (SourceKind::Float, "float.sqrt") => Float(FloatKind::Sqrt),
            (SourceKind::Float, "float.floor") => Float(FloatKind::Floor),
            (SourceKind::Float, "float.ceil") => Float(FloatKind::Ceil),
            (SourceKind::Float, "float.trunc") => Float(FloatKind::Trunc),
            (SourceKind::Float, "float.round_ties_even") => Float(FloatKind::RoundTiesEven),
            (SourceKind::Float, "float.to_bits") => Float(FloatKind::ToBits),
            (SourceKind::Float, "float.from_bits") => Float(FloatKind::FromBits),
            (SourceKind::Float, "float.to_text") => FloatToText,
            (SourceKind::Float, "float.int_from_valid") => IntFromFloat,
            (SourceKind::Mem, "mem.drop") => MemDrop,
            (SourceKind::Mem, "mem.replace") => MemReplace,
            (SourceKind::Mem, "mem.swap") => MemSwap,
            (SourceKind::TypeCheck, "types.plain_copy") => PlainCopy(Box::new(parameter(f)?)),
            (SourceKind::TypeCheck, "types.reference_free") => {
                ReferenceFree(Box::new(parameter(f)?))
            }
            (SourceKind::Vm, "vm.schema") => ScriptSchema(Box::new(parameter(f)?)),
            (SourceKind::Vm, "vm.pack") => ScriptPack,
            (SourceKind::Vm, "vm.unpack") => ScriptUnpack,
            (SourceKind::Domain, "domain.value_pack") => DomainValuePack,
            (SourceKind::Domain, "domain.value_unpack") => DomainValueUnpack,
            (SourceKind::Domain, "domain.lookup_type") => DirectoryType(Box::new(parameter(f)?)),
            (SourceKind::Domain, "domain.module") => DomainModule,
            (SourceKind::Domain, "domain.readonly") => DomainReadOnly,
            (SourceKind::Domain, "domain.pack") => DomainPack,
            (SourceKind::Domain, "domain.unpack") => DomainUnpack,
            (SourceKind::Domain, id) => {
                adapter = true;
                match id {
                    "domain.query" => DomainAccess(DomainAccessKind::Query),
                    "domain.read" => DomainAccess(DomainAccessKind::Read),
                    "domain.write" => DomainAccess(DomainAccessKind::Write),
                    "domain.create" => DomainCreate,
                    "domain.finish_create" => DomainCreateFinish,
                    "domain.after" => TimerStart(false),
                    "domain.every" => TimerStart(true),
                    "domain.finish_timer" => TimerFinish,
                    "domain.finish_timer_value" => TimerValueFinish,
                    "domain.lookup_witness" => DirectoryWitness,
                    "domain.snapshot_from" => DomainSnapshot,
                    "domain.snapshot_pair" => DomainSnapshotPair,
                    "domain.snapshot_value" => DomainSnapshotValue,
                    "domain.finish_snapshot_read" => DomainSetReadFinish,
                    "domain.read_snapshot" => DomainSetRead,
                    "domain.edit_snapshot" => DomainSetEdit(false),
                    "domain.edit_candidate" => DomainSetEdit(true),
                    "domain.finish_edit_snapshot" => DomainSetEditFinish(false),
                    "domain.finish_edit_candidate" => DomainSetEditFinish(true),
                    "domain.candidate_set_from" => DomainSetCandidate(false),
                    "domain.candidate_set_add" => DomainSetCandidate(true),
                    "domain.finish_set_candidate_values" => DomainSetValueFinish(false),
                    "domain.finish_set_candidate" => DomainSetFinish(false),
                    "domain.finish_set_add_values" => DomainSetValueFinish(true),
                    "domain.finish_set_add" => DomainSetFinish(true),
                    "domain.candidate_from" => DomainCandidate,
                    "domain.finish_candidate_values" => DomainCandidateValueFinish,
                    "domain.finish_candidate" => DomainCandidateFinish,
                    "domain.candidate_pair_from" => DomainCandidatePair,
                    "domain.finish_candidate_pair_values" => DomainCandidatePairValueFinish,
                    "domain.finish_candidate_pair" => DomainCandidatePairFinish,
                    "domain.query_from" => DomainAt(DomainAccessKind::Query),
                    "domain.read_from" => DomainAt(DomainAccessKind::Read),
                    "domain.write_to" => DomainAt(DomainAccessKind::Write),
                    "domain.query_pair" => DomainPair(DomainAccessKind::Query),
                    "domain.read_pair" => DomainPair(DomainAccessKind::Read),
                    "domain.update_pair" | "domain.write_pair" => {
                        DomainPair(DomainAccessKind::Write)
                    }
                    "domain.request" | "domain.request_read" => DomainRequest,
                    "domain.finish_request" => DomainFinish,
                    "domain.finish_request_value" => DomainRequestValueFinish,
                    "domain.finish_scalar" => DomainScalarFinish(false),
                    "domain.finish_current_scalar" => DomainScalarFinish(true),
                    "domain.finish_when" => DomainWhenFinish,
                    "domain.finish_paths" => DomainPathsFinish,
                    "domain.remove_named_if" => DirectoryRemoveIf,
                    "domain.finish_remove_named_if" => DirectoryRemoveIfFinish,
                    "domain.finish_value" => DomainValueFinish(false),
                    "domain.finish_current_value" => DomainValueFinish(true),
                    _ => return Err(unknown(&attribute)),
                }
            }
            (SourceKind::BufferStorage, id) => {
                let intrinsic = BufferIntrinsic::ALL
                    .into_iter()
                    .find(|intrinsic| {
                        id.strip_prefix("buffer.")
                            == intrinsic.symbol().strip_prefix("__joky_buffer_")
                    })
                    .ok_or_else(|| unknown(&attribute))?;
                declaration(&attribute, false)?;
                f.buffer_intrinsic = Some(intrinsic);
                f.buffer_element = Some(Box::new(buffer_parameters(f, intrinsic)?));
                continue;
            }
            _ => return Err(unknown(&attribute)),
        };
        declaration(&attribute, adapter)?;
        f.compiler_intrinsic = Some(compiler);
    }
    if kind == SourceKind::BufferLibrary {
        // These bodies still undergo normal provenance and escape analysis.
        // Only compiler-owned adapters receive this private result ABI marker.
        let functions = module
            .items
            .iter_mut()
            .filter_map(|item| match item {
                Item::FnDef(f) => Some(f),
                _ => None,
            })
            .chain(
                module
                    .implementations
                    .iter_mut()
                    .flat_map(|implementation| &mut implementation.methods),
            );
        for function in functions {
            if function.ret_ty.reference_type().is_some() && function.compiler_intrinsic.is_none() {
                function.compiler_intrinsic = Some(DynBorrowAdapter);
            }
        }
    }
    if kind == SourceKind::Arena {
        // Method identities are granted by the embedded definition, never its path.
        for implementation in &mut module.implementations {
            for method in &mut implementation.methods {
                method.compiler_intrinsic = match method.name.as_str() {
                    "new" => Some(ArenaNew),
                    "alloc" => Some(ArenaAlloc),
                    _ => None,
                };
            }
        }
    }
    if kind == SourceKind::BufferStorage {
        for intrinsic in BufferIntrinsic::ALL {
            if module
                .items
                .iter()
                .filter(|item| {
                    matches!(item,
                Item::FnDef(f) if f.buffer_intrinsic == Some(intrinsic))
                })
                .count()
                != 1
            {
                return Err(error(
                    Span::empty(0),
                    ErrorCode::InvalidState,
                    "Each compiler buffer primitive must be declared exactly once",
                ));
            }
        }
    }
    Ok(())
}

fn declaration(attribute: &IntrinsicAttribute, adapter: bool) -> Result<(), Error> {
    if attribute.has_body != adapter {
        return Err(error(
            attribute.span,
            ErrorCode::InvalidCall,
            if adapter {
                "This intrinsic requires a source adapter body"
            } else {
                "Compiler-implemented intrinsic declarations must have no body"
            },
        ));
    }
    Ok(())
}

fn unknown(attribute: &IntrinsicAttribute) -> Error {
    error(
        attribute.span,
        ErrorCode::InvalidImport,
        format!(
            "Unknown or unauthorized compiler intrinsic '{}'",
            attribute.name
        ),
    )
}

/// Check resolved primitive signatures even when no consumer uses the declaration.
pub(super) fn validate_signatures(items: &[Item]) -> Result<(), Error> {
    use crate::parser::ast::{CompilerIntrinsic::*, FloatKind};
    for item in items {
        let Item::FnDef(f) = item else {
            continue;
        };
        let params: Vec<_> = f.params.iter().map(|p| p.ty.clone()).collect();
        let signature = crate::FunctionSignature::new(params.clone(), f.ret_ty.clone());
        let bytes =
            |ty: &Type| matches!(ty.scalar_types(), [Type::Buffer(t)] if **t == Type::UInt8);
        let shared_text = Type::reference(Type::String, false);
        let expected = if let Some(intrinsic) = f.buffer_intrinsic {
            Some(intrinsic.signature(&buffer_parameters(f, intrinsic)?))
        } else {
            match &f.compiler_intrinsic {
                Some(ArenaNew) => Some(crate::FunctionSignature::new(
                    vec![],
                    Type::Resource("arena.Arena".into()),
                )),
                Some(ArenaAlloc) => {
                    let ty = parameter(f)?;
                    Some(crate::FunctionSignature::new(
                        vec![
                            Type::reference(Type::Resource("arena.Arena".into()), false),
                            ty.clone(),
                        ],
                        Type::reference(ty, true),
                    ))
                }
                Some(Assert) => Some(crate::FunctionSignature::new(
                    vec![Type::Bool, shared_text],
                    Type::Unit,
                )),
                Some(Panic) => Some(crate::FunctionSignature::new(vec![shared_text], Type::Unit)),
                Some(TextClone) => Some(crate::FunctionSignature::new(
                    vec![shared_text],
                    Type::String,
                )),
                Some(TextFreeze)
                    if params.len() == 1 && bytes(&params[0]) && f.ret_ty == Type::String =>
                {
                    Some(signature.clone())
                }
                Some(TextIntoBytes) if params == [Type::String] && bytes(&f.ret_ty) => {
                    Some(signature.clone())
                }
                Some(TextBytes) => Some(crate::FunctionSignature::new(
                    vec![shared_text],
                    Type::reference(Type::BufferSlice(Arc::new(Type::UInt8)), false),
                )),
                Some(RuneFromValidU32) => Some(crate::FunctionSignature::new(
                    vec![Type::UInt32],
                    Type::Rune,
                )),
                Some(PlainCopy(_) | ReferenceFree(_)) => {
                    parameter(f)?;
                    Some(crate::FunctionSignature::new(vec![], Type::Unit))
                }
                Some(MemDrop) => Some(crate::FunctionSignature::new(
                    vec![parameter(f)?],
                    Type::Unit,
                )),
                Some(MemReplace) => {
                    let ty = parameter(f)?;
                    Some(crate::FunctionSignature::new(
                        vec![Type::reference(ty.clone(), true), ty.clone()],
                        ty,
                    ))
                }
                Some(MemSwap) => {
                    let ty = Type::reference(parameter(f)?, true);
                    Some(crate::FunctionSignature::new(
                        vec![ty.clone(), ty],
                        Type::Unit,
                    ))
                }
                Some(ScriptSchema(_) | DirectoryType(_)) => {
                    parameter(f)?;
                    Some(crate::FunctionSignature::new(vec![], Type::String))
                }
                Some(ScriptPack | DomainValuePack)
                    if params == [parameter(f)?] && packet(&f.ret_ty) =>
                {
                    Some(signature.clone())
                }
                Some(ScriptUnpack | DomainValueUnpack)
                    if params.len() == 1
                        && params[0]
                            .reference_type()
                            .is_some_and(|r| !r.mutable && packet(&r.target))
                        && f.ret_ty == parameter(f)? =>
                {
                    Some(signature.clone())
                }
                Some(DomainPack) => Some(crate::FunctionSignature::new(
                    vec![parameter(f)?],
                    Type::UInt64,
                )),
                Some(DomainUnpack) => Some(crate::FunctionSignature::new(
                    vec![Type::UInt64],
                    parameter(f)?,
                )),
                Some(DomainModule) => Some(crate::FunctionSignature::new(
                    vec![],
                    crate::runtime::domain::module_type(),
                )),
                Some(Float(kind)) if params.len() == 1 => {
                    let valid = match kind {
                        FloatKind::FromText => {
                            params[0] == shared_text && f.ret_ty == Type::Float64
                        }
                        FloatKind::FromBits => matches!(
                            (&params[0], &f.ret_ty),
                            (Type::UInt32, Type::Float32) | (Type::UInt64, Type::Float64)
                        ),
                        FloatKind::ToBits => matches!(
                            (&params[0], &f.ret_ty),
                            (Type::Float32, Type::UInt32) | (Type::Float64, Type::UInt64)
                        ),
                        _ => params[0].is_float() && params[0] == f.ret_ty,
                    };
                    valid.then(|| signature.clone())
                }
                Some(FloatToText)
                    if params.len() == 1 && params[0].is_float() && f.ret_ty == Type::String =>
                {
                    Some(signature.clone())
                }
                Some(IntFromFloat)
                    if params.len() == 1
                        && params[0].is_float()
                        && f.ret_ty.integer().is_some() =>
                {
                    Some(signature.clone())
                }
                Some(
                    TextFreeze | TextIntoBytes | ScriptPack | ScriptUnpack | DomainValuePack
                    | DomainValueUnpack | Float(_) | FloatToText | IntFromFloat,
                ) => None,
                // Domain adapters also have real source bodies. The shared IR
                // checks their return types and each captured operation at use sites.
                _ => continue,
            }
        };
        let generic = f.buffer_intrinsic.is_some()
            || matches!(
                f.compiler_intrinsic,
                Some(
                    MemDrop
                        | ArenaAlloc
                        | MemReplace
                        | MemSwap
                        | ScriptSchema(_)
                        | DirectoryType(_)
                        | DomainValuePack
                        | DomainValueUnpack
                        | ScriptPack
                        | ScriptUnpack
                        | PlainCopy(_)
                        | ReferenceFree(_)
                        | DomainPack
                        | DomainUnpack
                )
            );
        if f.suspends
            || (!generic && !f.type_params.is_empty())
            || expected.as_ref() != Some(&signature)
        {
            return Err(error(
                f.span,
                ErrorCode::InvalidCall,
                "Invalid compiler intrinsic signature",
            ));
        }
    }
    Ok(())
}

fn packet(ty: &Type) -> bool {
    matches!(ty.scalar_types(), [Type::Buffer(element)] if **element == Type::UInt64)
}
