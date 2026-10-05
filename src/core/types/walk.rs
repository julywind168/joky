//! Shared, iterative traversal of type arguments and validated storage.
//! Nominal bodies are not expanded: recursive references retain shallow identity.
use crate::parser::ast::{EnumType, FunctionType, RecordType, TupleType, Type};
use std::{collections::HashSet, sync::Arc};

#[derive(Clone, Copy)]
pub(crate) enum TypeEdges {
    /// Signature types and nominal arguments; no fields, scalars or buffer elements.
    Arguments,
    /// Also visit nominal scalar layouts and internal buffer elements.
    Storage,
    /// Visit nominal fields, variant payloads and buffer elements, including zero-sized nested types.
    Fields,
}

#[derive(Hash, PartialEq, Eq)]
enum Node {
    Function(*const FunctionType),
    Tuple(*const TupleType),
    Enum(*const EnumType),
    Record(*const RecordType),
}

/// Visits shared aggregate/signature nodes once, in the existing stack order.
/// Pointer identity bounds shared DAGs without invoking structural type equality.
pub(crate) struct TypeWalk<'a> {
    pending: Vec<&'a Type>,
    seen: HashSet<Node>,
    edges: TypeEdges,
}

impl<'a> TypeWalk<'a> {
    pub(crate) fn new(root: &'a Type, edges: TypeEdges) -> Self {
        Self {
            pending: vec![root],
            seen: HashSet::new(),
            edges,
        }
    }
}

impl<'a> Iterator for TypeWalk<'a> {
    type Item = &'a Type;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(ty) = self.pending.pop() {
            let node = match ty {
                Type::Function(f) => Some(Node::Function(Arc::as_ptr(f))),
                Type::Tuple(t) => Some(Node::Tuple(Arc::as_ptr(t))),
                Type::Enum(e) => Some(Node::Enum(Arc::as_ptr(e))),
                Type::Record(r) => Some(Node::Record(Arc::as_ptr(r))),
                _ => None,
            };
            if node.is_some_and(|node| !self.seen.insert(node)) {
                continue;
            }
            match ty {
                Type::Function(f) => {
                    self.pending.extend(&f.signature.params);
                    self.pending.push(&f.signature.ret);
                }
                Type::Reference(r) => self.pending.push(&r.target),
                Type::Tuple(t) => self.pending.extend(&t.elements),
                Type::Enum(e) => {
                    self.pending.extend(&e.arguments);
                    if matches!(self.edges, TypeEdges::Storage) {
                        self.pending.extend(&e.scalars);
                    } else if matches!(self.edges, TypeEdges::Fields) {
                        self.pending
                            .extend(e.variants.iter().filter_map(|v| v.payload.as_ref()));
                    }
                }
                Type::Record(r) => {
                    self.pending.extend(&r.arguments);
                    if matches!(self.edges, TypeEdges::Storage) {
                        self.pending.extend(&r.scalars);
                    } else if matches!(self.edges, TypeEdges::Fields) {
                        self.pending.extend(r.fields.iter().map(|f| &f.ty));
                    }
                }
                Type::Buffer(element)
                | Type::BufferElement(element)
                | Type::BufferSlice(element) => {
                    if !matches!(self.edges, TypeEdges::Arguments) {
                        self.pending.push(element);
                    }
                }
                Type::Float32
                | Type::Float64
                | Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Int64
                | Type::UInt8
                | Type::UInt16
                | Type::UInt32
                | Type::Rune
                | Type::UInt64
                | Type::String
                | Type::Ptr
                | Type::Bool
                | Type::Unit
                | Type::Named(_)
                | Type::Applied(_)
                | Type::Parameter(_) => {}
            }
            return Some(ty);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parser::ast::Item, TypeRegistry};

    #[test]
    fn shared_type_dags_are_visited_once_and_keep_error_order() {
        let mut ty = Type::tuple(vec![
            Type::Named("first".into()),
            Type::Named("last".into()),
        ]);
        for _ in 0..40 {
            ty = Type::tuple(vec![ty.clone(), ty]);
        }
        let nodes: Vec<_> = TypeWalk::new(&ty, TypeEdges::Arguments).collect();
        assert_eq!(nodes.len(), 43);
        assert!(matches!(nodes[41], Type::Named(name) if name == "last"));
        assert!(matches!(nodes[42], Type::Named(name) if name == "first"));
        let signature = Type::function(
            vec![Type::Named("first".into())],
            Type::Named("last".into()),
        );
        let error = TypeRegistry::default().validate(&signature).unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::UnknownType);
        assert!(error.message().contains("`last`"));
    }

    #[test]
    fn nominal_arguments_and_storage_have_distinct_edges() {
        let ast = TypeRegistry::default()
            .resolve_aliases(
                &crate::parser::parse(
                    "struct Box<T> { callback: fn(Int32) -> Bool } fn value(x: Box<UInt64>) {}",
                )
                .unwrap(),
            )
            .unwrap();
        let function = ast
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(function) if function.name == "value" => Some(function),
                _ => None,
            })
            .expect("expected source function");
        let ty = &function.params[0].ty;
        let arguments: Vec<_> = TypeWalk::new(ty, TypeEdges::Arguments).collect();
        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[1], &Type::UInt64);
        let storage: Vec<_> = TypeWalk::new(ty, TypeEdges::Storage).collect();
        assert!(storage.iter().any(|ty| matches!(ty, Type::Function(_))));
        assert!(storage.contains(&&Type::Int32));
        assert!(storage.contains(&&Type::Bool));
        let buffer = Type::Buffer(Arc::new(ty.clone()));
        assert_eq!(TypeWalk::new(&buffer, TypeEdges::Arguments).count(), 1);
        assert_eq!(
            TypeWalk::new(&buffer, TypeEdges::Storage).count(),
            storage.len() + 1
        );
    }
}
