//! Native key adapters keep runtime storage independent of Joky value layouts.
use super::{
    abi::{abi_types, value_arguments, value_from_params},
    cranelift::{CraneliftBackend, ModuleLifecycle},
    environment::CompiledValue,
    helpers::codegen_error,
};
use crate::{
    diagnostic::CodegenError,
    mir::{MirFunction, MirFunctionId},
    sema::{Type, TypeTable},
};
use cranelift_codegen::ir::{
    types, AbiParam, FuncRef, InstBuilder, MemFlagsData, Signature, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Linkage};
use std::collections::{HashMap, HashSet};

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    pub(super) fn declare_hasher_runtime(&mut self) -> Result<[FuncId; 5], CodegenError> {
        use joky_runtime_abi::symbols::*;
        let pointer = self.module.target_config().pointer_type();
        let call_conv = self.module.isa().default_call_conv();
        let mut ids = Vec::new();
        for (name, parameters, returns) in [
            (HASHER_NEW_SYMBOL, vec![], vec![pointer]),
            (HASHER_WORD_SYMBOL, vec![pointer, types::I64], vec![]),
            (
                HASHER_STRING_SYMBOL,
                vec![pointer, pointer, pointer],
                vec![],
            ),
            (HASHER_FINISH_SYMBOL, vec![pointer], vec![types::I64]),
            (HASHER_BYTES_SYMBOL, vec![pointer, pointer], vec![]),
        ] {
            ids.push(
                self.module
                    .declare_function(
                        name,
                        Linkage::Import,
                        &Signature {
                            params: parameters.into_iter().map(AbiParam::new).collect(),
                            returns: returns.into_iter().map(AbiParam::new).collect(),
                            call_conv,
                        },
                    )
                    .map_err(codegen_error)?,
            );
        }
        Ok(ids.try_into().unwrap())
    }

    pub(super) fn define_map_key_adapters(
        &mut self,
        functions: &[MirFunction],
        types: &TypeTable,
        functions_ids: &HashMap<MirFunctionId, FuncId>,
    ) -> Result<(), CodegenError> {
        self.map_key_adapters.clear();
        let mut keys = types
            .map_key_types()
            .filter(|ty| !types.contains_type_parameter(*ty))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.next_function += 1;
        keys.sort_by_key(|ty| format!("{ty:?}"));
        let runtime = self.declare_hasher_runtime()?;
        let pointer = self.module.target_config().pointer_type();
        let call_conv = self.module.isa().default_call_conv();
        let string_eq = self
            .module
            .declare_function(
                joky_runtime_abi::symbols::STRING_EQ_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer); 4],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let drop = self
            .module
            .declare_function(
                joky_runtime_abi::symbols::DROP_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer)],
                    returns: vec![],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_eq = self
            .module
            .declare_function(
                joky_runtime_abi::symbols::BYTES_EQ_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer); 2],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let dup = self
            .module
            .declare_function(
                joky_runtime_abi::symbols::DUP_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer)],
                    returns: vec![AbiParam::new(pointer)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        for (index, key) in keys.into_iter().enumerate() {
            let mut adapters = Vec::new();
            for hashing in [true, false] {
                let signature = Signature {
                    params: vec![AbiParam::new(pointer); if hashing { 1 } else { 2 }],
                    returns: vec![AbiParam::new(if hashing { types::I64 } else { types::I8 })],
                    call_conv,
                };
                let id = self
                    .module
                    .declare_function(
                        &format!(
                            "joky_key_{}_{}_{}",
                            self.next_function,
                            index,
                            if hashing { "hash" } else { "equal" }
                        ),
                        Linkage::Local,
                        &signature,
                    )
                    .map_err(codegen_error)?;
                let mut context = self.module.make_context();
                context.func.signature = signature;
                let refs =
                    runtime.map(|id| self.module.declare_func_in_func(id, &mut context.func));
                let eq = self
                    .module
                    .declare_func_in_func(string_eq, &mut context.func);
                let drop_ref = self.module.declare_func_in_func(drop, &mut context.func);
                let dup_ref = self.module.declare_func_in_func(dup, &mut context.func);
                let methods = functions
                    .iter()
                    .filter_map(|function| {
                        let receiver = function.receiver?;
                        let method = match function.name.as_str() {
                            crate::sema::HASH_METHOD => crate::sema::HASH_METHOD,
                            crate::sema::PARTIAL_EQ_METHOD => crate::sema::PARTIAL_EQ_METHOD,
                            _ => return None,
                        };
                        Some((
                            (receiver, method),
                            self.module.declare_func_in_func(
                                functions_ids[&function.id],
                                &mut context.func,
                            ),
                        ))
                    })
                    .collect();
                let adapter = KeyAdapter {
                    types,
                    methods,
                    dup: dup_ref,
                    hash: HashRefs {
                        word: refs[1],
                        string: refs[2],
                        bytes: refs[4],
                    },
                    string_eq: eq,
                    bytes_eq: self
                        .module
                        .declare_func_in_func(bytes_eq, &mut context.func),
                };
                {
                    let mut builder =
                        FunctionBuilder::new(&mut context.func, &mut self.function_context);
                    let block = builder.create_block();
                    builder.append_block_params_for_function_params(block);
                    builder.switch_to_block(block);
                    let args = builder.block_params(block).to_vec();
                    let mut load = |address| {
                        let words = abi_types(key, pointer, types)
                            .into_iter()
                            .enumerate()
                            .map(|(index, ty)| {
                                builder.ins().load(
                                    ty,
                                    MemFlagsData::new(),
                                    address,
                                    (index * 8) as i32,
                                )
                            })
                            .collect::<Vec<_>>();
                        value_from_params(&words, key, types).map_err(codegen_error)
                    };
                    let left = load(args[0])?;
                    let right = if hashing { None } else { Some(load(args[1])?) };
                    let result = if hashing {
                        let call = builder.ins().call(refs[0], &[]);
                        let state = builder.inst_results(call)[0];
                        adapter.hash(&mut builder, key, left, state)?;
                        let call = builder.ins().call(refs[3], &[state]);
                        let result = builder.inst_results(call)[0];
                        builder.ins().call(drop_ref, &[state]);
                        result
                    } else {
                        adapter.equal(&mut builder, key, left, right.unwrap())?
                    };
                    builder.ins().return_(&[result]);
                    builder.seal_all_blocks();
                    builder.finalize(self.module.target_config());
                }
                self.module
                    .define_function(id, &mut context)
                    .map_err(codegen_error)?;
                adapters.push(id);
            }
            self.map_key_adapters
                .insert(key, (adapters[0], adapters[1]));
        }
        Ok(())
    }

    pub(super) fn map_key_refs(
        &mut self,
        function: &mut cranelift_codegen::ir::Function,
    ) -> HashMap<Type, (FuncRef, FuncRef)> {
        self.map_key_adapters
            .iter()
            .map(|(ty, (hash, equal))| {
                (
                    *ty,
                    (
                        self.module.declare_func_in_func(*hash, function),
                        self.module.declare_func_in_func(*equal, function),
                    ),
                )
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
pub(super) struct HashRefs {
    pub word: FuncRef,
    pub string: FuncRef,
    pub bytes: FuncRef,
}

struct KeyAdapter<'a> {
    types: &'a TypeTable,
    methods: HashMap<(Type, &'static str), FuncRef>,
    dup: FuncRef,
    hash: HashRefs,
    string_eq: FuncRef,
    bytes_eq: FuncRef,
}

impl KeyAdapter<'_> {
    fn method(&self, ty: Type, method: &'static str) -> Result<FuncRef, CodegenError> {
        self.methods
            .get(&(ty, method))
            .copied()
            .ok_or_else(|| CodegenError::RuntimeError {
                message: format!("missing {method} implementation for map key {ty:?}"),
            })
    }

    fn hash(
        &self,
        builder: &mut FunctionBuilder<'_>,
        ty: Type,
        value: CompiledValue,
        state: Value,
    ) -> Result<(), CodegenError> {
        if !self.types.has_builtin_hash(ty) {
            // A shared &self is a retained copy consumed by the method.
            let value = super::functions::duplicate_shared_value(builder, value, self.dup)?;
            let mut args = value_arguments(value);
            args.push(state);
            builder
                .ins()
                .call(self.method(ty, crate::sema::HASH_METHOD)?, &args);
        } else if let CompiledValue::Tuple {
            elements,
            ty: Type::Tuple(id),
        } = value
        {
            for (element, ty) in elements.into_iter().zip(self.types.tuple_elements(id)) {
                self.hash(builder, *ty, element, state)?;
            }
        } else {
            emit_hash(builder, value, state, self.hash)?;
        }
        Ok(())
    }

    fn equal(
        &self,
        builder: &mut FunctionBuilder<'_>,
        ty: Type,
        left: CompiledValue,
        right: CompiledValue,
    ) -> Result<Value, CodegenError> {
        if !self.types.has_builtin_partial_eq(ty) {
            let left = super::functions::duplicate_shared_value(builder, left, self.dup)?;
            let right = super::functions::duplicate_shared_value(builder, right, self.dup)?;
            let mut args = value_arguments(left);
            args.extend(value_arguments(right));
            let call = builder
                .ins()
                .call(self.method(ty, crate::sema::PARTIAL_EQ_METHOD)?, &args);
            return Ok(builder.inst_results(call)[0]);
        }
        if let (
            CompiledValue::Tuple {
                elements: left,
                ty: Type::Tuple(id),
            },
            CompiledValue::Tuple {
                elements: right, ..
            },
        ) = (&left, &right)
        {
            let done = builder.create_block();
            builder.append_block_param(done, types::I8);
            for ((a, b), ty) in left.iter().zip(right).zip(self.types.tuple_elements(*id)) {
                let equal = self.equal(builder, *ty, a.clone(), b.clone())?;
                let next = builder.create_block();
                builder.ins().brif(equal, next, &[], done, &[equal.into()]);
                builder.switch_to_block(next);
            }
            let equal = builder.ins().iconst(types::I8, 1);
            builder.ins().jump(done, &[equal.into()]);
            builder.switch_to_block(done);
            return Ok(builder.block_params(done)[0]);
        }
        emit_equal(builder, left, right, self.string_eq, self.bytes_eq)
    }
}

pub(super) fn emit_hash(
    builder: &mut FunctionBuilder<'_>,
    value: CompiledValue,
    state: Value,
    refs: HashRefs,
) -> Result<(), CodegenError> {
    match value {
        CompiledValue::Struct { fields, .. }
        | CompiledValue::Tuple {
            elements: fields, ..
        } => {
            for field in fields {
                emit_hash(builder, field, state, refs)?;
            }
        }
        CompiledValue::String { pointer, length } => {
            builder.ins().call(refs.string, &[state, pointer, length]);
        }
        CompiledValue::Bytes { pointer } => {
            builder.ins().call(refs.bytes, &[state, pointer]);
        }
        CompiledValue::Unit => {
            let zero = builder.ins().iconst(types::I64, 0);
            builder.ins().call(refs.word, &[state, zero]);
        }
        CompiledValue::Enum { tag, fields, ty } => {
            // Hash the discriminant first
            let tag_i64 = builder.ins().uextend(types::I64, tag);
            builder.ins().call(refs.word, &[state, tag_i64]);
            // Then recursively hash the current variant's fields (codegen has
            // already filled in the correct variant fields)
            for field in fields {
                emit_hash(builder, field, state, refs)?;
            }
            let _ = ty;
        }
        CompiledValue::Numeric { value, ty } if ty.is_integer() || ty == Type::Duration => {
            let value = if builder.func.dfg.value_type(value) == types::I64 {
                value
            } else if ty.is_signed_integer() {
                builder.ins().sextend(types::I64, value)
            } else {
                builder.ins().uextend(types::I64, value)
            };
            builder.ins().call(refs.word, &[state, value]);
        }
        CompiledValue::Boolean { value } => {
            let value = builder.ins().uextend(types::I64, value);
            builder.ins().call(refs.word, &[state, value]);
        }
        _ => {
            return Err(CodegenError::RuntimeError {
                message: "unsupported builtin Hash value".into(),
            })
        }
    }
    Ok(())
}

fn emit_equal(
    builder: &mut FunctionBuilder<'_>,
    left: CompiledValue,
    right: CompiledValue,
    string: FuncRef,
    bytes: FuncRef,
) -> Result<Value, CodegenError> {
    use cranelift_codegen::ir::condcodes::IntCC;
    Ok(match (left, right) {
        (
            CompiledValue::Struct { fields: left, .. },
            CompiledValue::Struct { fields: right, .. },
        )
        | (
            CompiledValue::Tuple { elements: left, .. },
            CompiledValue::Tuple {
                elements: right, ..
            },
        ) => {
            let mut equal = builder.ins().iconst(types::I8, 1);
            for (a, b) in left.into_iter().zip(right) {
                let next = emit_equal(builder, a, b, string, bytes)?;
                equal = builder.ins().band(equal, next);
            }
            equal
        }
        (
            CompiledValue::String {
                pointer: a,
                length: al,
            },
            CompiledValue::String {
                pointer: b,
                length: bl,
            },
        ) => {
            let call = builder.ins().call(string, &[a, al, b, bl]);
            builder.inst_results(call)[0]
        }
        (CompiledValue::Bytes { pointer: a }, CompiledValue::Bytes { pointer: b }) => {
            let call = builder.ins().call(bytes, &[a, b]);
            builder.inst_results(call)[0]
        }
        (CompiledValue::Unit, CompiledValue::Unit) => builder.ins().iconst(types::I8, 1),
        (CompiledValue::Enum { tag: a, .. }, CompiledValue::Enum { tag: b, .. })
        | (CompiledValue::Numeric { value: a, .. }, CompiledValue::Numeric { value: b, .. })
        | (CompiledValue::Boolean { value: a }, CompiledValue::Boolean { value: b }) => {
            builder.ins().icmp(IntCC::Equal, a, b)
        }
        _ => {
            return Err(CodegenError::RuntimeError {
                message: "unsupported builtin map equality".into(),
            })
        }
    })
}
