//! Versioned language ABI. Type identity is independent of layout and JIT addresses.

use super::{stable_id, StableId};
use crate::syntax::{self, Program, TypeAnnotation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StableTypeId(pub StableId);

impl StableTypeId {
    pub fn new(module: StableId, kind: &str, name: &str) -> Self {
        Self(stable_id(
            "type",
            format!("{:016x}/{kind}/{name}", module.0).as_bytes(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AbiParameter {
    pub name: String,
    pub ty: TypeAnnotation,
    pub borrowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AbiFunction {
    pub parameters: Vec<AbiParameter>,
    pub result: TypeAnnotation,
    pub effects: Vec<String>,
}

impl AbiFunction {
    /// Parse the versioned signature with the language parser, including nested
    /// tuples/functions and borrowed parameters. Comma splitting is not valid.
    pub fn parse(signature: &str) -> Result<Self, String> {
        let rest = signature
            .strip_prefix("fn(")
            .ok_or("expected function ABI")?;
        let (types, effects) = rest.rsplit_once("!{").ok_or("missing effect ABI")?;
        let effects = effects.strip_suffix('}').ok_or("invalid effect ABI")?;
        let source = format!("fn __abi({types} effects {{{effects}}} {{}}");
        let program =
            syntax::parse_program(&source).map_err(|e| format!("invalid function ABI: {e}"))?;
        if program.functions.len() != 1 {
            return Err("invalid function ABI".into());
        }
        let function = &program.functions[0];
        Ok(Self {
            parameters: function
                .parameters
                .iter()
                .map(|p| AbiParameter {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    borrowed: p.borrowed,
                })
                .collect(),
            result: function.return_type.clone().ok_or("missing ABI result")?,
            effects: function
                .effect_names
                .iter()
                .map(ToString::to_string)
                .collect(),
        })
    }
}

/// Reserved metadata entries cover nominal identities and definitions. Field
/// order is ABI-significant; declaration order and method order are not.
pub(crate) fn declaration_signatures(program: &Program, module: StableId) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for function in &program.functions {
        if let Some(foreign) = &function.foreign {
            entries.push((
                format!("extern:C:{}", function.name),
                format!(
                    "{:?}:{:?}:{:?}",
                    foreign.library, foreign.symbol, foreign.target_os
                ),
            ));
        }
    }
    for intrinsic in &program.intrinsic_types {
        let mut methods = intrinsic
            .methods
            .iter()
            .map(|method| {
                let mut predicates = method
                    .where_predicates
                    .iter()
                    .map(|predicate| {
                        let mut bounds = predicate
                            .bounds
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>();
                        bounds.sort();
                        bounds.dedup();
                        format!("{}:{}", predicate.parameter, bounds.join("+"))
                    })
                    .collect::<Vec<_>>();
                predicates.sort();
                let constraints = if predicates.is_empty() {
                    String::new()
                } else {
                    format!(" where {}", predicates.join(","))
                };
                format!(
                    "{}:{:?}<{}>({})->{}{}",
                    method.name,
                    method.receiver_mode,
                    method
                        .type_parameters
                        .iter()
                        .map(|parameter| {
                            let mut bounds = parameter
                                .bounds
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>();
                            bounds.sort();
                            bounds.dedup();
                            format!("{}:{}", parameter.name, bounds.join("+"))
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                    method
                        .parameters
                        .iter()
                        .map(|parameter| format!(
                            "{}:{}{}",
                            parameter.name,
                            if parameter.borrowed { "&" } else { "" },
                            parameter.ty
                        ))
                        .collect::<Vec<_>>()
                        .join(","),
                    method.return_type,
                    constraints
                )
            })
            .collect::<Vec<_>>();
        methods.sort();
        entries.push((
            format!("type:intrinsic:{}", intrinsic.name),
            format!(
                "kind={};effect={};arity={};{}",
                intrinsic.kind.as_str(),
                intrinsic.effect.as_deref().unwrap_or(""),
                intrinsic.type_parameters.len(),
                methods.join(";")
            ),
        ));
    }
    for class in &program.classes {
        let fields = class
            .fields
            .iter()
            .map(|f| format!("{}:{}:{}", f.name, f.mutable, f.ty))
            .collect::<Vec<_>>()
            .join(",");
        entries.push((
            format!("type:class:{}", class.name),
            format!(
                "{:016x}({fields})",
                StableTypeId::new(module, "class", &class.name).0 .0
            ),
        ));
    }
    for structure in &program.structs {
        let fields = structure
            .fields
            .iter()
            .map(|f| format!("{}:{}", f.name, f.ty))
            .collect::<Vec<_>>()
            .join(",");
        entries.push((
            format!("type:struct:{}", structure.name),
            format!(
                "{:016x};repr_c={};({fields})",
                StableTypeId::new(module, "struct", &structure.name).0 .0,
                structure.repr_c
            ),
        ));
    }
    for enumeration in &program.enums {
        let variants = enumeration
            .variants
            .iter()
            .map(|v| {
                format!(
                    "{}({})",
                    v.name,
                    v.fields
                        .iter()
                        .map(|f| format!("{}:{}", f.name, f.ty))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        entries.push((
            format!("type:enum:{}", enumeration.name),
            format!(
                "{:016x}({variants})",
                StableTypeId::new(module, "enum", &enumeration.name).0 .0
            ),
        ));
    }
    for definition in &program.traits {
        let mut methods = definition
            .methods
            .iter()
            .map(|m| {
                format!(
                    "{}:{:?}({})->{} effects {:?}",
                    m.name,
                    m.receiver_mode,
                    m.parameters
                        .iter()
                        .map(|p| format!(
                            "{}:{}{}",
                            p.name,
                            if p.borrowed { "&" } else { "" },
                            p.ty
                        ))
                        .collect::<Vec<_>>()
                        .join(","),
                    m.return_type,
                    m.effect_names
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                )
            })
            .collect::<Vec<_>>();
        methods.sort();
        let mut associated = definition
            .associated_types
            .iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>();
        associated.sort();
        entries.push((
            format!("type:trait:{}", definition.name),
            format!(
                "{:016x}({};{})",
                StableTypeId::new(module, "trait", &definition.name).0 .0,
                associated.join(","),
                methods.join(";")
            ),
        ));
    }
    for effect in &program.effects {
        let mut operations = effect
            .operations
            .iter()
            .map(|op| {
                format!(
                    "{}:{:?}:{}({})->{}",
                    op.name,
                    op.mode,
                    op.suspends,
                    op.parameters
                        .iter()
                        .map(|p| format!(
                            "{}:{}{}",
                            p.name,
                            if p.borrowed { "&" } else { "" },
                            p.ty
                        ))
                        .collect::<Vec<_>>()
                        .join(","),
                    op.return_type
                )
            })
            .collect::<Vec<_>>();
        operations.sort();
        entries.push((
            format!("effect:{}", effect.name),
            format!(
                "{:016x}({})",
                StableTypeId::new(module, "effect", &effect.name).0 .0,
                operations.join(";")
            ),
        ));
    }
    entries.sort();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intrinsic_declaration_kind_is_abi_significant() {
        let signature = |kind: &str| {
            let source = format!(
                "@intrinsic {kind} List(T: type) {{ fn sorted(&self) -> List(T) where T: Ord }}"
            );
            declaration_signatures(&syntax::parse_program(&source).unwrap(), StableId(1))
        };
        assert_ne!(signature("struct"), signature("class"));
    }

    #[test]
    fn intrinsic_where_constraints_are_abi_significant_and_order_independent() {
        let signature = |clause: &str| {
            let source =
                format!("@intrinsic class C(K: type, V: type) {{ fn f(&self) -> Unit {clause} }}");
            declaration_signatures(&syntax::parse_program(&source).unwrap(), StableId(1))
        };
        assert_ne!(signature(""), signature("where K: Ord"));
        assert_ne!(signature("where K: Ord"), signature("where V: Ord"));
        assert_ne!(signature("where K: Ord"), signature("where K: PartialOrd"));
        assert_eq!(
            signature("where K: Ord + Debug, V: Eq"),
            signature("where V: Eq, K: Debug + Ord")
        );
    }

    #[test]
    fn intrinsic_method_generics_and_static_receivers_are_abi_significant() {
        let signature = |source: &str| {
            declaration_signatures(&syntax::parse_program(source).unwrap(), StableId(1))
        };
        let generic = |parameters: &str| {
            format!(
                "@intrinsic struct String {{ fn parse(&self, {parameters}) -> Result(T, String) }}"
            )
        };
        assert_ne!(
            signature(&generic("T: type")),
            signature(&generic("T: type + FromString"))
        );
        assert_ne!(
            signature(&generic("T: type + FromString")),
            signature(&generic("T: type + Debug"))
        );
        assert_eq!(
            signature(&generic("T: type + FromString + Debug")),
            signature(&generic("T: type + Debug + FromString"))
        );
        assert_ne!(
            signature("trait Factory { fn create() -> Self }"),
            signature("trait Factory { fn create(&self) -> Self }")
        );
    }

    #[test]
    fn extern_target_os_is_abi_significant() {
        let mac = syntax::parse_program(
            "@extern(c, \"libSystem.B.dylib\", \"f\", os = \"macos\") fn f() -> Unit;",
        )
        .unwrap();
        let linux = syntax::parse_program(
            "@extern(c, \"libc.so.6\", \"f\", os = \"linux\") fn f() -> Unit;",
        )
        .unwrap();
        assert_ne!(
            declaration_signatures(&mac, StableId(1)),
            declaration_signatures(&linux, StableId(1))
        );
    }

    #[test]
    fn intrinsic_effect_binding_and_method_subset_are_abi_significant() {
        let source = "@intrinsic(effect = io) class File { fn peek(&self) -> Int32; fn close(self) -> Unit }";
        let signature = |source: &str| {
            declaration_signatures(&crate::syntax::parse_program(source).unwrap(), StableId(1))
        };
        let original = signature(source);
        for changed in [
            source.replace("effect = io", "effect = other"),
            source.replace("; fn close(self) -> Unit", ""),
            source.replace("&self", "self"),
            source.replace("Int32", "Int64"),
        ] {
            assert_ne!(original, signature(&changed));
        }
        assert_eq!(original, signature("@intrinsic(effect = io) class File { fn close(self) -> Unit; fn peek(&self) -> Int32 }"));
    }

    #[test]
    fn nested_signature_preserves_borrows_labels_and_effects() {
        let abi =
            AbiFunction::parse("fn(pair:(Int32,(String,Bool)),file:&File)->(Int32,String)!{io}")
                .unwrap();
        assert_eq!(abi.parameters.len(), 2);
        assert_eq!(abi.parameters[0].ty.to_string(), "(Int32,(String,Bool))");
        assert!(abi.parameters[1].borrowed);
        assert_eq!(abi.effects, ["io"]);
        assert_eq!(abi.result.to_string(), "(Int32,String)");
    }

    #[test]
    fn nominal_identity_survives_layout_changes_but_fingerprint_does_not() {
        let a = syntax::parse_program("class Box { let value: Int32 }").unwrap();
        let b = syntax::parse_program("class Box { let value: String }").unwrap();
        assert_ne!(
            declaration_signatures(&a, StableId(1)),
            declaration_signatures(&b, StableId(1))
        );
        assert_ne!(
            StableTypeId::new(StableId(1), "class", "Box"),
            StableTypeId::new(StableId(2), "class", "Box")
        );
        assert_ne!(
            StableTypeId::new(StableId(1), "class", "Box"),
            StableTypeId::new(StableId(1), "trait", "Box")
        );
    }
}
