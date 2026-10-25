//! Semantic packet kinds are independent of flattened ABI lane counts.
use crate::{Error, Type};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Kind {
    Scalar = 0,
    String = 1,
    Tuple = 2,
    Record = 3,
    Enum = 4,
    Local = 5,
}

impl Kind {
    pub(crate) fn of(ty: &Type) -> Self {
        match ty {
            Type::String => Self::String,
            Type::Tuple(_) => Self::Tuple,
            Type::Record(r) if r.closure.is_none() => Self::Record,
            Type::Enum(_) => Self::Enum,
            _ if crate::core::instance::scalar(ty) => Self::Scalar,
            _ => Self::Local,
        }
    }

    pub(crate) fn decode(value: usize) -> Option<Self> {
        Some(match value {
            0 => Self::Scalar,
            1 => Self::String,
            2 => Self::Tuple,
            3 => Self::Record,
            4 => Self::Enum,
            5 => Self::Local,
            _ => return None,
        })
    }

    /// Aggregate transport remains closed until all backends implement it.
    pub(crate) fn implemented(self) -> bool {
        matches!(self, Self::Scalar | Self::String)
    }
}

/// Classify the whole nominal tree, not just currently active physical lanes.
/// This describes TP6 eligibility; it is not an executable transport certificate.
pub(super) fn classify(
    root: &Type,
    resolve: &impl Fn(&Type) -> Result<Type, Error>,
    has_drop: &impl Fn(&Type) -> bool,
) -> Option<Kind> {
    let root = resolve(root).ok()?;
    let kind = Kind::of(&root);
    let mut pending = vec![(root, 0)];
    let mut visited = 0;
    while let Some((ty, depth)) = pending.pop() {
        visited += 1;
        if depth > 32 || visited > 65_536 {
            return None;
        }
        let ty = resolve(&ty).ok()?;
        if has_drop(&ty) || ty.storage_layout().is_err() {
            return None;
        }
        let children: Vec<Type> = match &ty {
            Type::Tuple(t) => t.elements.clone(),
            Type::Record(r) if r.closure.is_none() && !r.indirect => r
                .arguments
                .iter()
                .filter_map(|a| a.as_type())
                .cloned()
                .chain(r.fields.iter().map(|f| f.ty.clone()))
                .collect(),
            Type::Enum(e) if !e.indirect => e
                .arguments
                .iter()
                .filter_map(|a| a.as_type())
                .cloned()
                .chain(e.variants.iter().filter_map(|v| v.payload.clone()))
                .collect(),
            Type::String => Vec::new(),
            _ if crate::core::instance::scalar(&ty) => Vec::new(),
            _ => return None,
        };
        pending.extend(children.into_iter().map(|ty| (ty, depth + 1)));
    }
    Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::task_scope::layout::{encode, CheckedLayout};

    #[test]
    fn task_packet_kinds_check_nominal_children_and_preserve_transport_gate() {
        let source = r#"
            import joky/drop.{type Drop}
            import joky/option.{type Option}
            import joky/result.{type Result}
            import joky/array.{type Array}
            struct One { value: Int32 }
            struct Empty { }
            struct Box<T> { value: T }
            struct Marker<T> { value: Int32 }
            struct Tracked { text: String }
            impl Drop for Tracked { fn drop(&mut self) { } }
            type Alias = Box<(Int8, String)>
            fn scalar(value: Int32) { }
            fn text(value: String) { }
            fn unit(value: Unit) { }
            fn one(value: One) { }
            fn empty(value: Empty) { }
            fn tuple(value: (Int8, UInt16)) { }
            fn record(value: Alias) { }
            fn optional(value: Option<Alias>) { }
            fn outcome(value: Result<Alias, String>) { }
            fn borrowed(value: Box<&Int32>) { }
            fn pointer(value: Option<Ptr>) { }
            fn phantom(value: Marker<Ptr>) { }
            fn tracked(value: Box<Tracked>) { }
            fn inactive_drop(value: Result<Int32, Tracked>) { }
            fn container(value: Box<Array<Int32>>) { }
            fn callable(value: fn(Int32) -> Int32) { }
            fn child() @suspends -> One { One { value: 42 } }
            fn join_value() @suspends -> Int32 {
                let (a, b) = join { | child() | child() }
                a.value + b.value
            }
            fn race_value() @suspends -> Int32 {
                let a = race { | child() | child() }
                a.value
            }
            fn branch_value(value: One) @suspends { scope { branch { one(value) } } }
        "#;
        let types = crate::TypeRegistry::default().with_prelude(false);
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
            .unwrap();
        let prepared =
            crate::core::prepared::PreparedModule::new(&ast, &types, &Default::default()).unwrap();
        let plan = prepared.plan(&Default::default()).unwrap();
        let effects = super::super::analyse(&plan, &prepared, &Default::default());
        for name in ["join_value", "race_value", "branch_value"] {
            assert!(effects.require(name).is_err(), "{name}");
        }
        for (name, expected) in [
            ("scalar", Some(Kind::Scalar)),
            ("text", Some(Kind::String)),
            ("unit", Some(Kind::Scalar)),
            ("one", Some(Kind::Record)),
            ("empty", Some(Kind::Record)),
            ("tuple", Some(Kind::Tuple)),
            ("record", Some(Kind::Record)),
            ("optional", Some(Kind::Enum)),
            ("outcome", Some(Kind::Enum)),
            ("borrowed", None),
            ("pointer", None),
            ("phantom", None),
            ("tracked", None),
            ("inactive_drop", None),
            ("container", None),
            ("callable", None),
        ] {
            let signature = &plan.functions[name].signature;
            let ty = &signature.params[0];
            let kind = classify(ty, &|ty| Ok(ty.clone()), &|ty| {
                plan.bodies[name].has_custom_drop(ty)
            });
            assert_eq!(kind, expected, "{name}");
            let words = encode(signature).unwrap();
            let layout = CheckedLayout::decode(&words).unwrap();
            assert_eq!(layout.slot_counts().0, ty.parameter_slots(), "{name}");
            assert_eq!(
                layout.parallel_shape().is_ok(),
                kind.is_some_and(Kind::implemented),
                "{name} must not acquire transport from its lane shape"
            );
        }
        // Equal payload layouts must still retain distinct semantic kinds.
        // Unit alone retains its legacy parameter placeholder.
        for (scalar, aggregate, slots) in [("scalar", "one", (1, 1)), ("unit", "empty", (1, 0))] {
            let scalar = encode(&plan.functions[scalar].signature).unwrap();
            let aggregate = encode(&plan.functions[aggregate].signature).unwrap();
            assert_eq!((scalar[3], aggregate[3]), slots);
            assert_eq!(&scalar[4..], &aggregate[4..]);
            assert_ne!(scalar[2], aggregate[2]);
        }
    }
}
