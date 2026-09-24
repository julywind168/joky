//! Conditional methods on compiler-provided generic containers.
use super::checker::Checker;
use super::Type;
use crate::diagnostic::SemanticError;
use crate::syntax::{
    IntrinsicMethod, IntrinsicType, IntrinsicTypeKind, TypeAnnotation, TypeExpr, TypeParameter,
};
use crate::Span;
use std::collections::HashSet;
use std::sync::OnceLock;

pub(super) fn builtin_intrinsic_declarations() -> &'static [IntrinsicType] {
    static DECLARATIONS: OnceLock<Vec<IntrinsicType>> = OnceLock::new();
    DECLARATIONS.get_or_init(|| {
        [
            include_str!("../../std/joky/list.jk"),
            include_str!("../../std/joky/mut_list.jk"),
            include_str!("../../std/joky/bytes.jk"),
            include_str!("../../std/joky/string.jk"),
            include_str!("../../std/joky/map.jk"),
            include_str!("../../std/joky/mut_bytes.jk"),
            include_str!("../../std/joky/mut_map.jk"),
            include_str!("../../std/joky/mut_set.jk"),
            include_str!("../../std/joky/hasher.jk"),
        ]
        .into_iter()
        .flat_map(|source| {
            crate::syntax::parse_program(source)
                .expect("valid builtin intrinsic declarations")
                .intrinsic_types
        })
        .collect()
    })
}

// Compare declaration types structurally, allowing owner parameter renaming.
fn same_intrinsic_type(
    actual: &TypeAnnotation,
    expected: &TypeAnnotation,
    owner: &[TypeParameter],
    canonical: &[TypeParameter],
) -> bool {
    match (&actual.kind, &expected.kind) {
        (TypeExpr::Name(actual), TypeExpr::Name(expected)) => {
            match (
                owner.iter().position(|p| p.name == *actual),
                canonical.iter().position(|p| p.name == *expected),
            ) {
                (Some(actual), Some(expected)) => actual == expected,
                (None, None) => actual == expected,
                _ => false,
            }
        }
        (
            TypeExpr::Apply {
                callee: actual,
                arguments: actual_args,
            },
            TypeExpr::Apply {
                callee: expected,
                arguments: expected_args,
            },
        ) => {
            same_intrinsic_type(actual, expected, owner, canonical)
                && same_intrinsic_arguments(actual_args, expected_args, owner, canonical)
        }
        (TypeExpr::Tuple(actual), TypeExpr::Tuple(expected)) => {
            actual.len() == expected.len()
                && actual.iter().zip(expected).all(|(actual, expected)| {
                    same_intrinsic_type(actual, expected, owner, canonical)
                })
        }
        (
            TypeExpr::Function {
                parameters: actual,
                result: actual_result,
            },
            TypeExpr::Function {
                parameters: expected,
                result: expected_result,
            },
        ) => {
            same_intrinsic_arguments(actual, expected, owner, canonical)
                && same_intrinsic_type(actual_result, expected_result, owner, canonical)
        }
        _ => false,
    }
}

fn same_intrinsic_arguments(
    actual: &[crate::syntax::TypeArgument],
    expected: &[crate::syntax::TypeArgument],
    owner: &[TypeParameter],
    canonical: &[TypeParameter],
) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            actual.label == expected.label
                && same_intrinsic_type(&actual.value, &expected.value, owner, canonical)
        })
}

impl Checker {
    pub(super) fn validate_intrinsic_kind(
        &self,
        declaration: &IntrinsicType,
    ) -> Result<(), SemanticError> {
        let expected = match declaration.name.as_str() {
            "List" | "Map" | "Set" => Some(IntrinsicTypeKind::Struct),
            "MutList" | "MutMap" | "MutSet" => Some(IntrinsicTypeKind::Class),
            name => super::types::primitive_type(name).map(|ty| {
                if ty.is_native_resource() || matches!(ty, Type::MutBytes | Type::Hasher) {
                    IntrinsicTypeKind::Class
                } else {
                    IntrinsicTypeKind::Struct
                }
            }),
        };
        if let Some(expected) = expected {
            if declaration.kind != expected {
                return Err(SemanticError::FunctionNotSupported {
                    name: format!(
                        "intrinsic type '{}' must be declared as {}",
                        declaration.name,
                        expected.as_str()
                    ),
                    span: declaration.span,
                });
            }
        }
        if declaration.effect.is_none()
            && declaration
                .methods
                .iter()
                .any(|method| method.operation.is_some())
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "intrinsic operation aliases require an effect binding".into(),
                span: declaration.span,
            });
        }
        if declaration.effect.is_some() && declaration.kind != IntrinsicTypeKind::Class {
            return Err(SemanticError::FunctionNotSupported {
                name: "intrinsic effect bindings require a class declaration".into(),
                span: declaration.span,
            });
        }
        Ok(())
    }

    pub(super) fn register_intrinsic_type(&mut self, declaration: &IntrinsicType) {
        // Concrete names such as Bytes are types already; Bytes() constructs a value.
        if !declaration.type_parameters.is_empty() {
            self.intrinsic_type_arities
                .insert(declaration.name.clone(), declaration.type_parameters.len());
        }
        self.intrinsic_parameter_names.insert(
            declaration.name.clone(),
            declaration
                .type_parameters
                .iter()
                .map(|p| p.name.clone())
                .collect(),
        );
        self.intrinsic_methods.insert(
            declaration.name.clone(),
            declaration
                .methods
                .iter()
                .cloned()
                .map(|m| (m.name.clone(), m))
                .collect(),
        );
    }

    pub(super) fn validate_intrinsic_constraints(
        &mut self,
        declaration: &IntrinsicType,
    ) -> Result<(), SemanticError> {
        let invalid = |message: String, span| SemanticError::FunctionNotSupported {
            name: message,
            span,
        };
        let parameters = declaration
            .type_parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<HashSet<_>>();
        if parameters.len() != declaration.type_parameters.len() {
            return Err(invalid(
                "duplicate intrinsic type parameter".into(),
                declaration.span,
            ));
        }
        let canonical = builtin_intrinsic_declarations()
            .iter()
            .find(|d| d.name == declaration.name);
        if canonical.is_some_and(|d| d.type_parameters.len() != declaration.type_parameters.len()) {
            return Err(invalid(
                "intrinsic type parameter count does not match builtin declaration".into(),
                declaration.span,
            ));
        }
        let mut names = HashSet::new();
        for method in &declaration.methods {
            let mut all_parameters = parameters.clone();
            for parameter in &method.type_parameters {
                if !all_parameters.insert(&parameter.name)
                    || method.parameters.iter().any(|p| p.name == parameter.name)
                {
                    return Err(invalid(
                        format!("duplicate intrinsic type parameter '{}'", parameter.name),
                        parameter.span,
                    ));
                }
            }
            if !names.insert(&method.name) {
                return Err(invalid(
                    format!(
                        "duplicate intrinsic method '{}.{}'",
                        declaration.name, method.name
                    ),
                    method.span,
                ));
            }
            if let Some(owner) = canonical {
                let required = owner
                    .methods
                    .iter()
                    .find(|m| m.name == method.name)
                    .ok_or_else(|| {
                        invalid(
                            format!(
                                "unknown intrinsic method '{}.{}'",
                                declaration.name, method.name
                            ),
                            method.span,
                        )
                    })?;
                let actual_parameters = declaration
                    .type_parameters
                    .iter()
                    .chain(&method.type_parameters)
                    .cloned()
                    .collect::<Vec<_>>();
                let expected_parameters = owner
                    .type_parameters
                    .iter()
                    .chain(&required.type_parameters)
                    .cloned()
                    .collect::<Vec<_>>();
                let matches = method.receiver_mode == required.receiver_mode
                    && method.type_parameters.len() == required.type_parameters.len()
                    && method.parameters.len() == required.parameters.len()
                    && method.parameters.iter().zip(&required.parameters).all(
                        |(actual, expected)| {
                            actual.borrowed == expected.borrowed
                                && same_intrinsic_type(
                                    &actual.ty,
                                    &expected.ty,
                                    &actual_parameters,
                                    &expected_parameters,
                                )
                        },
                    )
                    && same_intrinsic_type(
                        &method.return_type,
                        &required.return_type,
                        &actual_parameters,
                        &expected_parameters,
                    );
                if !matches {
                    return Err(invalid(format!("intrinsic method '{}.{}' must match its builtin signature (receiver, parameters and return type)", declaration.name, method.name), method.span));
                }
            }
            let mut resolved = method.clone();
            for parameter in &mut resolved.type_parameters {
                for bound in &mut parameter.bounds {
                    let name = self.import_dynamic_trait(&bound.to_string(), bound.span)?;
                    if !self.traits.contains_key(&name) {
                        return Err(SemanticError::UnknownTrait {
                            name,
                            span: bound.span,
                        });
                    }
                    let identity = self
                        .definition_module
                        .map_or_else(|| name.clone(), |module| super::trait_key(module, &name));
                    *bound = TypeAnnotation::named(&identity, bound.span);
                }
            }
            let mut subjects = HashSet::new();
            for predicate in &mut resolved.where_predicates {
                if !parameters.contains(predicate.parameter.as_str())
                    && !method
                        .type_parameters
                        .iter()
                        .any(|p| p.name == predicate.parameter)
                {
                    return Err(invalid(
                        format!(
                            "where clause refers to unknown type parameter '{}'",
                            predicate.parameter
                        ),
                        predicate.span,
                    ));
                }
                let subject = match &predicate.associated {
                    Some(associated) => format!("{}.{associated}", predicate.parameter),
                    None => predicate.parameter.clone(),
                };
                if !subjects.insert(subject.clone()) {
                    return Err(invalid(
                        format!("duplicate where predicate for '{subject}'"),
                        predicate.span,
                    ));
                }
                for bound in &mut predicate.bounds {
                    let name = self.import_dynamic_trait(&bound.to_string(), bound.span)?;
                    if !self.traits.contains_key(&name) {
                        return Err(SemanticError::UnknownTrait {
                            name,
                            span: bound.span,
                        });
                    }
                    let identity = self
                        .definition_module
                        .map_or_else(|| name.clone(), |module| super::trait_key(module, &name));
                    *bound = TypeAnnotation::named(&identity, bound.span);
                }
            }
            // A redeclaration may add constraints but cannot remove requirements
            // of an operation supplied by the compiler.
            if let Some((owner, required)) = canonical.and_then(|owner| {
                owner
                    .methods
                    .iter()
                    .find(|m| m.name == method.name)
                    .map(|m| (owner, m))
            }) {
                for (parameter, required) in resolved
                    .type_parameters
                    .iter()
                    .zip(&required.type_parameters)
                {
                    for bound in &required.bounds {
                        let name = bound.to_string();
                        if !parameter
                            .bounds
                            .iter()
                            .chain(
                                resolved
                                    .where_predicates
                                    .iter()
                                    .filter(|p| p.parameter == parameter.name)
                                    .flat_map(|p| &p.bounds),
                            )
                            .any(|actual| {
                                super::methods::builtin_bounds(
                                    actual.as_name().expect("resolved trait name"),
                                )
                                .contains(&name.as_str())
                            })
                        {
                            return Err(invalid(
                                format!(
                                    "intrinsic method '{}.{}' requires {}: {name}",
                                    declaration.name, method.name, parameter.name
                                ),
                                method.span,
                            ));
                        }
                    }
                }
                for predicate in &required.where_predicates {
                    let index = owner
                        .type_parameters
                        .iter()
                        .position(|p| p.name == predicate.parameter)
                        .expect("builtin predicate parameter");
                    let parameter = &declaration.type_parameters[index].name;
                    for bound in &predicate.bounds {
                        let name = bound.to_string();
                        let satisfied = resolved
                            .where_predicates
                            .iter()
                            .filter(|p| &p.parameter == parameter)
                            .flat_map(|p| &p.bounds)
                            .any(|actual| {
                                super::methods::builtin_bounds(
                                    actual.as_name().expect("resolved trait name"),
                                )
                                .contains(&name.as_str())
                            });
                        if !satisfied {
                            return Err(invalid(
                                format!(
                                    "intrinsic method '{}.{}' requires where {parameter}: {name}",
                                    declaration.name, method.name
                                ),
                                method.span,
                            ));
                        }
                    }
                }
            }
            self.intrinsic_methods
                .get_mut(&declaration.name)
                .expect("registered intrinsic")
                .insert(method.name.clone(), resolved);
        }
        Ok(())
    }

    pub(super) fn check_intrinsic_constraints(
        &self,
        receiver: Type,
        method: &IntrinsicMethod,
        type_arguments: &[Type],
        span: Span,
    ) -> Result<(), SemanticError> {
        for (parameter, actual) in method.type_parameters.iter().zip(type_arguments) {
            for bound in &parameter.bounds {
                let name = bound.to_string();
                if !self.implements_trait(*actual, &name) {
                    return Err(SemanticError::TraitBoundNotSatisfied {
                        trait_name: name,
                        actual: super::type_name(*actual),
                        span,
                    });
                }
            }
        }
        for predicate in &method.where_predicates {
            let actual = method
                .type_parameters
                .iter()
                .position(|p| p.name == predicate.parameter)
                .and_then(|index| type_arguments.get(index).copied())
                .map(Ok)
                .unwrap_or_else(|| {
                    self.resolve_intrinsic_type(receiver, &predicate.parameter, span)
                })?;
            for bound in &predicate.bounds {
                let name = bound.to_string();
                if !self.implements_trait(actual, &name) {
                    return Err(SemanticError::TraitBoundNotSatisfied {
                        trait_name: name,
                        actual: super::type_name(actual),
                        span,
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::module::{ModuleCompileContext, StableId};
    use crate::sema::{check_module_with_context, ModuleTypes};
    use crate::syntax::parse_program;

    #[test]
    fn intrinsic_method_bounds_keep_identity_after_serialization() {
        let provider = StableId(101);
        let context = ModuleCompileContext {
            definition_module: Some(provider),
            ..Default::default()
        };
        let source = "trait Ready { fn ready(&self) -> Bool } @intrinsic struct String { fn parse(&self, T: type + FromString + Ready) -> Result(T, String) }";
        let table = check_module_with_context(&parse_program(source).unwrap(), &context).unwrap();
        let encoded = bincode::serialize(&table.module).unwrap();
        let table: ModuleTypes = bincode::deserialize(&encoded).unwrap();
        assert_eq!(
            table.interface.intrinsic_types[0].methods[0].type_parameters[0].bounds[1].to_string(),
            super::super::trait_key(provider, "Ready")
        );
        let context = ModuleCompileContext {
            definition_module: Some(StableId(102)),
            standard_modules: [provider].into(),
            imports: [("api".into(), provider)].into(),
            dependency_types: std::collections::HashMap::from([(provider, table.into())]).into(),
            ..Default::default()
        };
        let source = r#"
            struct Value {}
            impl FromString for Value { fn from_string(value: &String) -> Result(Self, String) { Ok(Value()) } }
            impl api.Ready for Value { fn ready(&self) -> Bool { true } }
            fn main() { "value".parse(Value) }
        "#;
        check_module_with_context(&parse_program(source).unwrap(), &context).unwrap();
        let source = source.replace("api.Ready", "Ready");
        let source = format!("trait Ready {{ fn ready(&self) -> Bool }} {source}");
        let error =
            check_module_with_context(&parse_program(&source).unwrap(), &context).unwrap_err();
        assert!(error.to_string().contains("Ready"), "{error}");
    }

    #[test]
    fn intrinsic_declaration_kind_matches_builtin_semantics() {
        for (name, parameters, expected) in [
            ("List", "(T: type)", "struct"),
            ("Map", "(K: type, V: type)", "struct"),
            ("Set", "(T: type)", "struct"),
            ("Bytes", "", "struct"),
            ("MutList", "(T: type)", "class"),
            ("MutMap", "(K: type, V: type)", "class"),
            ("MutSet", "(T: type)", "class"),
            ("MutBytes", "", "class"),
            ("File", "", "class"),
            ("TcpStream", "", "class"),
            ("SqliteConnection", "", "class"),
        ] {
            let source = format!("@intrinsic {expected} {name}{parameters} {{}}");
            crate::sema::check_module(&parse_program(&source).unwrap()).unwrap();
            let wrong = if expected == "struct" {
                "class"
            } else {
                "struct"
            };
            let source = format!("@intrinsic {wrong} {name}{parameters} {{}}");
            let error = crate::sema::check_module(&parse_program(&source).unwrap()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("must be declared as {expected}")),
                "{source}: {error}"
            );
        }
        let program = parse_program("@intrinsic(effect = io) struct Handle {}").unwrap();
        let error = crate::sema::check_module(&program).unwrap_err();
        assert!(
            error.to_string().contains("require a class declaration"),
            "{error}"
        );
    }

    #[test]
    fn imported_where_predicates_preserve_trait_identity_through_serialization() {
        let provider = StableId(101);
        let context = ModuleCompileContext {
            definition_module: Some(provider),
            ..Default::default()
        };
        let program = parse_program("trait Ready { fn ready(&self) -> Bool } @intrinsic pub class MutList(T: type) { fn push(&self, item: T) -> Unit where T: Ready }").unwrap();
        let table = check_module_with_context(&program, &context).unwrap();
        let bytes = bincode::serialize(&table.module).unwrap();
        let table: ModuleTypes = bincode::deserialize(&bytes).unwrap();
        assert_eq!(
            table.interface.intrinsic_types[0].methods[0].where_predicates[0].bounds[0].to_string(),
            super::super::trait_key(provider, "Ready")
        );
        let context = ModuleCompileContext {
            definition_module: Some(StableId(102)),
            standard_modules: [provider].into(),
            imports: [("api".into(), provider)].into(),
            dependency_types: std::collections::HashMap::from([(provider, table.into())]).into(),
            ..Default::default()
        };
        let body = "struct Item { let key: Int32 } fn f() { let xs = MutList#{Item(key: 1)}; xs.push(Item(key: 2)) }";
        let source =
            format!("impl api.Ready for Item {{ fn ready(&self) -> Bool {{ true }} }} {body}");
        check_module_with_context(&parse_program(&source).unwrap(), &context).unwrap();
        let source = format!("trait Ready {{ fn ready(&self) -> Bool }} impl Ready for Item {{ fn ready(&self) -> Bool {{ true }} }} {body}");
        let error =
            check_module_with_context(&parse_program(&source).unwrap(), &context).unwrap_err();
        assert!(error.to_string().contains("Ready"), "{error}");
        let source = format!("@intrinsic class MutList(Item: type) {{ fn push(&self, item: Item) -> Unit where Item: api.Ready }} impl api.Ready for Item {{ fn ready(&self) -> Bool {{ true }} }} {body}");
        check_module_with_context(&parse_program(&source).unwrap(), &context).unwrap();
    }
}
