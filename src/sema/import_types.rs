//! Import type definitions from already compiled dependency artifacts.
use super::checker::{intern_tuple, Checker};
use super::symbol_table::{
    ClassFieldInfo, ClassInfo, EnumInfo, EnumVariantInfo, FunctionTypeInfo, StructInfo,
};
use super::{ModuleTypes, Type};
use crate::diagnostic::SemanticError;
use crate::module::StableId;
use crate::syntax::{TypeAnnotation, TypeExpr};
use crate::Span;

impl Checker {
    pub(super) fn import_dependency_effects(&mut self) -> Result<(), SemanticError> {
        let mut imports = self
            .import_aliases
            .iter()
            .map(|(alias, module)| (alias.clone(), *module))
            .collect::<Vec<_>>();
        imports.sort();
        for (alias, module) in imports {
            let Some(table) = self.dependency_types.get(&module).cloned() else {
                continue;
            };
            for effect in table.effects().iter() {
                let name = format!("{alias}.{}", effect.name);
                let id =
                    self.import_effect_definition(module, &table, effect, &name, Span::new(0, 0))?;
                if self.standard_modules.contains(&module) {
                    self.effects.alias(effect.name.clone(), id);
                }
            }
            if self.standard_modules.contains(&module) {
                for name in ["FileMode", "SeekFrom"] {
                    if let Some(ty) = nominal_type(&table, &format!("@{:016x}/{name}", module.0)) {
                        let ty = self.import_abi_type(module, &table, ty, Span::new(0, 0))?;
                        self.type_bindings.entry(name.into()).or_insert(ty);
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn import_abi_effect(
        &mut self,
        module: StableId,
        name: &str,
        span: Span,
    ) -> Result<super::EffectId, SemanticError> {
        if let Some(table) = self.dependency_types.get(&module).cloned() {
            if let Some(effect) = table
                .effects()
                .by_name(name)
                .and_then(|id| table.effects().effect(id))
            {
                let alias = self
                    .import_aliases
                    .iter()
                    .find(|(_, id)| **id == module)
                    .map(|(alias, _)| alias.clone())
                    .unwrap_or_else(|| format!("@{:016x}", module.0));
                return self.import_effect_definition(
                    module,
                    &table,
                    effect,
                    &format!("{alias}.{name}"),
                    span,
                );
            }
        }
        self.effects
            .by_name(name)
            .ok_or_else(|| SemanticError::UnknownType {
                name: name.into(),
                span,
            })
    }

    fn import_effect_definition(
        &mut self,
        module: StableId,
        table: &ModuleTypes,
        effect: &super::effects::EffectInfo,
        name: &str,
        span: Span,
    ) -> Result<super::EffectId, SemanticError> {
        if let Some(id) = self.effects.by_name(name) {
            return Ok(id);
        }
        let existing = self
            .effects
            .iter()
            .find(|existing| existing.identity == effect.identity)
            .map(|existing| existing.id);
        if let Some(id) = existing {
            self.effects.alias(name.into(), id);
            return Ok(id);
        }
        let id = self.effects.define(name.into());
        self.effects.set_identity(id, effect.identity.clone());
        for operation in &effect.operations {
            let parameters = operation
                .parameters
                .iter()
                .map(|ty| self.import_abi_type(module, table, *ty, span))
                .collect::<Result<Vec<_>, _>>()?;
            let result = self.import_abi_type(module, table, operation.return_type, span)?;
            let op = self.effects.operation(
                id,
                operation.name.clone(),
                parameters,
                result,
                operation.mode,
                operation.suspends,
            );
            self.effects
                .set_parameter_borrows(op, operation.parameter_borrows.clone());
        }
        Ok(id)
    }

    pub(in crate::sema) fn resolve_abi_type(
        &mut self,
        module: StableId,
        annotation: &TypeAnnotation,
    ) -> Result<Type, SemanticError> {
        if let TypeExpr::Name(name) = &annotation.kind {
            let qualified = format!("@{:016x}/{name}", module.0);
            if let Some(table) = self.dependency_types.get(&module).cloned() {
                if let Some(ty) =
                    nominal_type(&table, &qualified).or_else(|| nominal_type(&table, name))
                {
                    return self.import_abi_type(module, &table, ty, annotation.span);
                }
            }
        }
        let rewritten = self.qualify_abi_annotation(module, annotation)?;
        self.resolve_type(&rewritten)
    }

    fn qualify_abi_annotation(
        &mut self,
        module: StableId,
        annotation: &TypeAnnotation,
    ) -> Result<TypeAnnotation, SemanticError> {
        let mut result = annotation.clone();
        match &mut result.kind {
            TypeExpr::Const(_) => {}
            TypeExpr::Name(name) => {
                let qualified = format!("@{:016x}/{name}", module.0);
                if let Some(table) = self.dependency_types.get(&module).cloned() {
                    if let Some(ty) =
                        nominal_type(&table, &qualified).or_else(|| nominal_type(&table, name))
                    {
                        self.import_abi_type(module, &table, ty, annotation.span)?;
                        *name = match ty {
                            Type::Class(id) => table.class_name(id),
                            Type::Struct(id) => table.struct_name(id),
                            Type::Enum(id) => table.enum_name(id),
                            _ => unreachable!(),
                        }
                        .to_owned();
                    }
                }
            }
            TypeExpr::Tuple(elements) | TypeExpr::TraitComposition(elements) => {
                for element in elements {
                    *element = self.qualify_abi_annotation(module, element)?;
                }
            }
            TypeExpr::Apply { arguments, .. } => {
                for argument in arguments {
                    argument.value = self.qualify_abi_annotation(module, &argument.value)?;
                }
            }
            TypeExpr::Function { parameters, result } => {
                for parameter in parameters {
                    parameter.value = self.qualify_abi_annotation(module, &parameter.value)?;
                }
                **result = self.qualify_abi_annotation(module, result)?;
            }
            TypeExpr::Member { .. } => {}
        }
        Ok(result)
    }

    pub(super) fn import_abi_type(
        &mut self,
        module: StableId,
        table: &ModuleTypes,
        ty: Type,
        span: Span,
    ) -> Result<Type, SemanticError> {
        if let Some(ty) = self.imported_types.get(&(module, ty)) {
            return Ok(*ty);
        }
        let nominal = match ty {
            Type::Class(id) => Some(table.class_name(id)),
            Type::Struct(id) => Some(table.struct_name(id)),
            Type::Enum(id) => Some(table.enum_name(id)),
            _ => None,
        };
        if let (Some(origin), Some(name)) = (self.definition_module, nominal) {
            if let Some(local) = name.strip_prefix(&format!("@{:016x}/", origin.0)) {
                let existing = match ty {
                    Type::Class(_) => self.classes.get(local).map(|v| Type::Class(v.id)),
                    Type::Struct(_) => self.structs.get(local).map(|v| Type::Struct(v.id)),
                    Type::Enum(_) => self.enums.get(local).map(|v| Type::Enum(v.id)),
                    _ => None,
                };
                if let Some(existing) = existing {
                    return Ok(existing);
                }
            }
        }
        let mapped = match ty {
            Type::Dyn(id) => {
                let mut info = table.dynamic_types[id].clone();
                info.qualify(module);
                info.bindings = info
                    .bindings
                    .iter()
                    .map(|(name, ty)| {
                        Ok((
                            name.clone(),
                            self.import_abi_type(module, table, *ty, span)?,
                        ))
                    })
                    .collect::<Result<_, SemanticError>>()?;
                for method in &mut info.methods {
                    let mut signature = table.function_type(method.signature).clone();
                    signature.parameters = signature
                        .parameters
                        .iter()
                        .map(|ty| self.import_abi_type(module, table, *ty, span))
                        .collect::<Result<_, _>>()?;
                    signature.return_type =
                        self.import_abi_type(module, table, signature.return_type, span)?;
                    let mut effects = super::EffectSet::new();
                    for operation in signature.effects.iter() {
                        let group = table.effects().effect(operation.effect).unwrap();
                        let local = self.import_abi_effect(module, &group.name, span)?;
                        let name = &table.effects().operation_info(operation).unwrap().name;
                        effects.insert(self.effects.operation_by_name(local, name).unwrap());
                    }
                    signature.effects = effects;
                    for name in &mut method.effect_names {
                        let group = self.import_abi_effect(module, name, span)?;
                        *name = self.effects.effect(group).unwrap().name.clone();
                    }
                    method.signature = self.function_types.len();
                    self.function_types.push(signature);
                }
                let id = self
                    .dynamic_types
                    .iter()
                    .position(|entry| {
                        entry.trait_names == info.trait_names && entry.bindings == info.bindings
                    })
                    .unwrap_or_else(|| {
                        let id = self.dynamic_types.len();
                        self.dynamic_types.push(info);
                        id
                    });
                Type::Dyn(id)
            }
            Type::Class(id) => {
                let name = table.class_name(id).to_owned();
                if let Some(info) = self.classes.get(&name) {
                    return Ok(Type::Class(info.id));
                }
                let mapped = Type::Class(self.classes.len());
                self.imported_types.insert((module, ty), mapped);
                self.classes.insert(
                    name.clone(),
                    ClassInfo {
                        id: self.classes.len(),
                        fields: vec![],
                        methods: Default::default(),
                    },
                );
                let fields = table
                    .class_fields(id)
                    .iter()
                    .map(|(name, ty)| {
                        Ok((
                            name.clone(),
                            ClassFieldInfo {
                                ty: self.import_abi_type(module, table, *ty, span)?,
                                mutable: false,
                            },
                            None,
                        ))
                    })
                    .collect::<Result<Vec<_>, SemanticError>>()?;
                self.classes.get_mut(&name).unwrap().fields = fields;
                mapped
            }
            Type::Tuple(id) => {
                let elements = table
                    .tuple_elements(id)
                    .iter()
                    .map(|t| self.import_abi_type(module, table, *t, span))
                    .collect::<Result<Vec<_>, _>>()?;
                Type::Tuple(intern_tuple(&mut self.tuple_types, elements))
            }
            Type::Option(id) => {
                let t = self.import_abi_type(module, table, table.option_type(id), span)?;
                Type::Option(super::checker::intern_option(&mut self.option_types, t))
            }
            Type::Result(id) => {
                let (a, b) = table.result_types(id);
                let a = self.import_abi_type(module, table, a, span)?;
                let b = self.import_abi_type(module, table, b, span)?;
                Type::Result(super::checker::intern_result(&mut self.result_types, a, b))
            }
            Type::List(id) | Type::MutList(id) => {
                let t = self.import_abi_type(module, table, table.list_type(id), span)?;
                let id = super::checker::intern_list(&mut self.list_types, t);
                if matches!(ty, Type::List(_)) {
                    Type::List(id)
                } else {
                    Type::MutList(id)
                }
            }
            Type::Map(id) | Type::MutMap(id) | Type::MutSet(id) => {
                let info = table.map_info(id);
                let a = self.import_abi_type(module, table, info.key, span)?;
                let b = self.import_abi_type(module, table, info.value, span)?;
                let id = super::checker::intern_map(&mut self.maps, a, b);
                match ty {
                    Type::Map(_) => Type::Map(id),
                    Type::MutMap(_) => Type::MutMap(id),
                    _ => Type::MutSet(id),
                }
            }
            Type::Cown(id) => {
                let t = self.import_abi_type(module, table, table.cown_type(id), span)?;
                Type::Cown(super::checker::intern_cown(&mut self.cowns, t))
            }
            Type::CPtr(id) | Type::CMutPtr(id) => {
                let pointee =
                    self.import_abi_type(module, table, table.c_pointer_type(id), span)?;
                let local = self
                    .c_pointers
                    .iter()
                    .position(|ty| *ty == pointee)
                    .unwrap_or_else(|| {
                        let id = self.c_pointers.len();
                        self.c_pointers.push(pointee);
                        id
                    });
                if matches!(ty, Type::CPtr(_)) {
                    Type::CPtr(local)
                } else {
                    Type::CMutPtr(local)
                }
            }
            Type::CArray(id) => {
                let (element, count) = table.c_array_info(id);
                let element = self.import_abi_type(module, table, element, span)?;
                let local = self
                    .c_arrays
                    .iter()
                    .position(|entry| *entry == (element, count))
                    .unwrap_or_else(|| {
                        let id = self.c_arrays.len();
                        self.c_arrays.push((element, count));
                        id
                    });
                Type::CArray(local)
            }
            Type::Struct(id) => {
                if table.range_item(ty).is_some() {
                    return self.intern_range(table.struct_fields(id)[0].1, span);
                }
                let name = table.struct_name(id).to_owned();
                if let Some(info) = self.structs.get(&name) {
                    return Ok(Type::Struct(info.id));
                }
                let mapped = Type::Struct(self.structs.len());
                self.imported_types.insert((module, ty), mapped);
                self.structs.insert(
                    name.clone(),
                    StructInfo {
                        repr_c: table.struct_is_repr_c(id),
                        id: self.structs.len(),
                        fields: vec![],
                        defaults: vec![],
                        methods: Default::default(),
                    },
                );
                let fields = table
                    .struct_fields(id)
                    .iter()
                    .map(|(name, t)| {
                        Ok((name.clone(), self.import_abi_type(module, table, *t, span)?))
                    })
                    .collect::<Result<Vec<_>, SemanticError>>()?;
                let mut defaults = vec![None; fields.len()];
                if let Some(source_defaults) = table.struct_import_defaults(id) {
                    for (target, default) in defaults.iter_mut().zip(source_defaults) {
                        if let Some(default) = default {
                            let mut value = default.clone().map_err(|name| {
                                SemanticError::FunctionNotSupported { name, span }
                            })?;
                            value.map_types(&mut |ty| {
                                self.import_abi_type(module, table, ty, span)
                            })?;
                            let node = crate::syntax::NodeId::new(self.next_import_node);
                            self.next_import_node = self
                                .next_import_node
                                .checked_add(1)
                                .ok_or_else(|| SemanticError::FunctionNotSupported {
                                    name: "AST identity space exhausted".into(),
                                    span,
                                })?;
                            self.types.insert(node, value.ty);
                            self.imported_constants.insert(node, value);
                            *target = Some(crate::syntax::Expr {
                                id: node,
                                span,
                                kind: crate::syntax::ExprKind::Integer(0),
                            });
                        }
                    }
                }
                let info = self.structs.get_mut(&name).unwrap();
                info.defaults = defaults;
                info.fields = fields;
                mapped
            }
            Type::Enum(id) => {
                let name = table.enum_name(id).to_owned();
                if name == crate::sema::ORDERING_NAME {
                    return Ok(Type::Enum(self.enums["Ordering"].id));
                }
                if let Some(info) = self.enums.get(&name) {
                    return Ok(Type::Enum(info.id));
                }
                let mapped = Type::Enum(self.enums.len());
                self.imported_types.insert((module, ty), mapped);
                self.enums.insert(
                    name.clone(),
                    EnumInfo {
                        id: self.enums.len(),
                        variants: vec![],
                    },
                );
                let variants = table
                    .enum_variants(id)
                    .iter()
                    .map(|v| {
                        Ok(EnumVariantInfo {
                            name: v.name.clone(),
                            fields: v
                                .fields
                                .iter()
                                .map(|(n, t)| {
                                    Ok((n.clone(), self.import_abi_type(module, table, *t, span)?))
                                })
                                .collect::<Result<Vec<_>, SemanticError>>()?,
                        })
                    })
                    .collect::<Result<Vec<_>, SemanticError>>()?;
                self.enums.get_mut(&name).unwrap().variants = variants;
                mapped
            }
            Type::Function(id) => {
                let info = table.function_type(id);
                let parameters = info
                    .parameters
                    .iter()
                    .map(|t| self.import_abi_type(module, table, *t, span))
                    .collect::<Result<Vec<_>, _>>()?;
                let return_type = self.import_abi_type(module, table, info.return_type, span)?;
                if !info.effects.is_empty() {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "effectful closure fields require an explicit function ABI".into(),
                        span,
                    });
                }
                let id = self
                    .function_types
                    .iter()
                    .position(|function| {
                        function.parameter_names == info.parameter_names
                            && function.parameters == parameters
                            && function.return_type == return_type
                            && function.effects.is_empty()
                            && function.suspends == info.suspends
                    })
                    .unwrap_or_else(|| {
                        let id = self.function_types.len();
                        self.function_types.push(FunctionTypeInfo {
                            parameter_names: info.parameter_names.clone(),
                            parameters,
                            return_type,
                            effects: Default::default(),
                            suspends: info.suspends,
                        });
                        id
                    });
                Type::Function(id)
            }
            Type::Associated(id) => {
                let info = table.associated_info(id);
                super::checker::intern_associated_type(
                    &mut self.associated_types,
                    info.parameter,
                    &super::import_methods::trait_key(module, &info.trait_name),
                    &info.name,
                )
            }
            // Trait signatures preserve Self until a concrete implementation is selected.
            Type::SelfType => Type::SelfType,
            primitive => primitive,
        };
        self.imported_types.insert((module, ty), mapped);
        if matches!(ty, Type::Class(_) | Type::Struct(_) | Type::Enum(_)) {
            self.import_nominal_methods(module, table, ty, mapped, span)?;
        }
        Ok(mapped)
    }
}

fn nominal_type(table: &ModuleTypes, name: &str) -> Option<Type> {
    table
        .class_type(name)
        .or_else(|| table.struct_type(name))
        .or_else(|| table.enum_type(name))
}
