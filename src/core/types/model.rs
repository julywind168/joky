//! Shared type representation for syntax, semantic checking, and compilation backends.
//!
//! Parsed annotations and resolved types share identities and storage metadata;
//! AST declarations and expressions remain in `parser::ast`.
use super::walk::{TypeEdges, TypeWalk};
use crate::Span;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// Parsed type names; aliases are normalized before semantic and ABI checks.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// Compiler-owned inline shape, normalized independently of executable storage.
    FixedArray(Arc<FixedArrayType>),
    /// Compiler-private, non-Copy initialization capability.
    #[doc(hidden)]
    FixedArrayInit(Arc<FixedArrayType>),
    /// Borrowed trait target, bound before shared semantic planning.
    DynTrait(Arc<DynTraitType>),
    /// A non-capturing language function value with an explicit suspension effect.
    Function(Arc<FunctionType>),
    Float32,
    Float64,
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    Rune,
    UInt64,
    String,
    /// Registered non-Copy external resource; its control block is private.
    Resource(Arc<str>),
    /// Opaque, nullable C object pointer; copying does not transfer ownership.
    Ptr,
    Bool,
    Unit,
    /// An unresolved source type name or a registered opaque host type.
    Named(String),
    /// An unresolved application of a generic record, enum or alias.
    Applied(Arc<TypeApplication>),
    /// A rigid, declaration-local type parameter; never an executable value tag.
    Parameter(Arc<TypeParameter>),
    Projection(Arc<TypeProjection>),
    /// A resolved module-local nominal record with a finite scalar layout.
    Record(Arc<RecordType>),
    /// A resolved nominal tagged union with Copy or owning payloads.
    Enum(Arc<EnumType>),
    /// A structural tuple, normalized before planning.
    Tuple(Arc<TupleType>),
    /// Task-local reference; crosses only an explicitly checked C borrowing boundary.
    Reference(Arc<ReferenceType>),
    /// Internal, task-bound buffer ownership. No source spelling or host ABI yet.
    #[doc(hidden)]
    Buffer(Arc<Type>),
    /// Borrow-only targets with no source-visible representation or owning value.
    #[doc(hidden)]
    BufferElement(Arc<Type>),
    #[doc(hidden)]
    BufferSlice(Arc<Type>),
    /// Borrowed cursor; its private position never owns the element storage.
    #[doc(hidden)]
    BufferIterator(Arc<Type>),
    /// Borrowed paired-column cursor and stable entry snapshot.
    #[doc(hidden)]
    BufferPairIterator(Arc<Type>),
    #[doc(hidden)]
    BufferEntry(Arc<Type>),
}

/// Source projections become declaration identities before rigid type checking.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct TypeProjection {
    pub base: Type,
    pub trait_name: Option<String>,
    pub name: String,
    pub identity: Option<(u64, usize)>,
    pub span: Span,
}
impl PartialEq for TypeProjection {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
            && match (self.identity, other.identity) {
                (Some(a), Some(b)) => a == b,
                (None, None) => self.trait_name == other.trait_name && self.name == other.name,
                _ => false,
            }
    }
}
impl Eq for TypeProjection {}

/// Structural callable type; host signatures keep their separate calling contract.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub signature: crate::FunctionSignature,
    pub suspends: bool,
    /// No argument loan may be retained or returned.
    pub noescape: bool,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct ReferenceType {
    pub target: Type,
    pub mutable: bool,
    /// Internal dyn lanes. The first retains the semantic borrow identity.
    pub(crate) lanes: Vec<Type>,
}

// Cached physical lanes never change semantic reference identity.
impl PartialEq for ReferenceType {
    fn eq(&self, other: &Self) -> bool {
        self.mutable == other.mutable && self.target == other.target
    }
}
impl Eq for ReferenceType {}

/// Named equalities retain their source locations for qualification diagnostics.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct DynTraitType {
    pub identity: Option<u64>,
    pub interface: Option<Arc<DynInterface>>,
    pub name: String,
    pub equalities: Vec<AssociatedEquality>,
    pub span: Span,
}

/// Shared semantic descriptor; this is not a native layout or host ABI.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct DynInterface {
    pub trait_id: u64,
    pub bindings: Vec<(String, Type)>,
    pub methods: Vec<DynMethodSlot>,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct DynMethodSlot {
    pub suspends: bool,
    pub name: String,
    pub mutable: bool,
    pub signature: crate::FunctionSignature,
    pub noescape_params: Vec<usize>,
    pub span: Span,
}

impl PartialEq for DynInterface {
    fn eq(&self, other: &Self) -> bool {
        self.trait_id == other.trait_id && self.bindings == other.bindings
    }
}
impl Eq for DynInterface {}

impl PartialEq for DynTraitType {
    fn eq(&self, other: &Self) -> bool {
        if let (Some(a), Some(b)) = (&self.interface, &other.interface) {
            return a == b;
        }
        self.identity == other.identity
            && self.name == other.name
            && self.equalities.len() == other.equalities.len()
            && self
                .equalities
                .iter()
                .zip(&other.equalities)
                .all(|(a, b)| a.name == b.name && a.ty == b.ty)
    }
}
impl Eq for DynTraitType {}

impl Type {
    pub(crate) fn contains_compact_enum(&self) -> bool {
        TypeWalk::new(self, TypeEdges::Fields).any(|t| matches!(t, Self::Enum(e) if e.compact))
    }
    pub(crate) fn place_storage_name(&self) -> &'static str {
        if self.contains_compact_enum() {
            "Wide enum storage"
        } else {
            "FixedArray"
        }
    }
    pub(crate) fn contains_place_storage(&self) -> bool {
        TypeWalk::new(self, TypeEdges::Fields).any(|t| {
            matches!(t, Self::FixedArray(_) | Self::FixedArrayInit(_))
                || matches!(t, Self::Enum(e) if e.compact)
        })
    }
    pub(crate) fn is_explicit_arena(&self) -> bool {
        matches!(self, Self::Resource(name) if name.as_ref() == "arena.Arena")
    }

    pub(crate) fn contains_explicit_arena(&self) -> bool {
        TypeWalk::new(self, TypeEdges::Fields).any(Type::is_explicit_arena)
    }

    pub(crate) fn is_domain_capability(&self) -> bool {
        matches!(self,Self::Named(name) if name=="domain.Handle" || name=="domain.Module" || name=="admission.Token")
    }
    pub(crate) fn contains_dyn(&self) -> bool {
        match self {
            Self::DynTrait(_) => true,
            Self::FixedArray(_)
            | Self::FixedArrayInit(_)
            | Self::Record(_)
            | Self::Enum(_)
            | Self::Tuple(_)
            | Self::Function(_)
            | Self::Reference(_)
            | Self::Projection(_)
            | Self::Buffer(_)
            | Self::BufferElement(_)
            | Self::BufferSlice(_)
            | Self::BufferIterator(_)
            | Self::BufferPairIterator(_)
            | Self::BufferEntry(_) => {
                TypeWalk::new(self, TypeEdges::Fields).any(|ty| matches!(ty, Self::DynTrait(_)))
            }
            _ => false,
        }
    }
    pub(crate) fn contains_dyn_view(&self) -> bool {
        TypeWalk::new(self, TypeEdges::Fields)
            .any(|ty| matches!(ty, Self::Reference(r) if matches!(r.target, Self::DynTrait(_))))
    }
    pub(crate) fn is_dyn_wrapper_view(&self) -> bool {
        self.reference_type().is_some_and(|r| {
            matches!(r.target, Self::Record(_))
                && matches!(r.target.scalar_types(), [Self::DynTrait(_)])
        })
    }
    pub(crate) fn dyn_interface(&self) -> Option<&Arc<DynInterface>> {
        match self.reference_type().map_or(self, |r| &r.target) {
            Self::DynTrait(d) => d.interface.as_ref(),
            _ => None,
        }
    }
    pub(crate) fn is_callable_parameter(&self) -> bool {
        matches!(self, Self::Parameter(p) if p.callable.is_some())
    }
    pub(crate) fn is_closure_value(&self) -> bool {
        self.closure_info().is_some() || self.is_callable_parameter()
    }
    pub(crate) fn contains_closures(&self) -> bool {
        TypeWalk::new(self, TypeEdges::Storage).any(Type::is_closure_value)
    }
    /// Borrowed environments remain ineligible for persistent storage even when
    /// read-only copies can travel through local aggregates and synchronous calls.
    pub(crate) fn contains_unstorable_closures(&self) -> bool {
        TypeWalk::new(self, TypeEdges::Fields)
            .any(|ty| ty.closure_info().is_some_and(|c| !c.storable))
    }
    pub(crate) fn closure_info(&self) -> Option<&ClosureInfo> {
        match self {
            Self::Record(record) => record.closure.as_deref(),
            _ => None,
        }
    }
    /// A borrowed FnOnce environment may travel through a scoped CPS call,
    /// while its capture loans remain visible to ordinary lifetime checking.
    pub(crate) fn is_scoped_suspending_closure(&self) -> bool {
        self.closure_info()
            .is_some_and(|c| c.suspends && !c.storable && c.mode == CallableKind::FnOnce)
            && !self.is_owner()
    }
    pub fn function(params: Vec<Type>, ret: Type) -> Self {
        Self::function_with_effect(params, ret, false)
    }
    pub fn function_with_effect(params: Vec<Type>, ret: Type, suspends: bool) -> Self {
        Self::function_with_contract(params, ret, suspends, false)
    }
    pub fn function_with_contract(
        params: Vec<Type>,
        ret: Type,
        suspends: bool,
        noescape: bool,
    ) -> Self {
        Self::Function(Arc::new(FunctionType {
            signature: crate::FunctionSignature::new(params, ret),
            suspends,
            noescape,
        }))
    }
    pub(crate) fn contains_functions(&self) -> bool {
        self.scalar_types().iter().any(|ty| {
            matches!(ty, Self::Function(_))
                || ty
                    .reference_type()
                    .is_some_and(|r| r.target.contains_functions())
        })
    }
    /// One scalar storage lane carrying ownership, including text and host resources.
    /// Rigid unknowns model movable owners until concrete substitution before lowering.
    pub(crate) fn is_owner_lane(&self) -> bool {
        matches!(
            self,
            Self::Buffer(_)
                | Self::String
                | Self::Resource(_)
                | Self::DynTrait(_)
                | Self::Projection(_)
        ) || matches!(self, Self::Parameter(p) if p.callable.is_none() && !p.copy)
    }
    /// A movable owning value, including aggregates with owning contents.
    pub(crate) fn is_owner(&self) -> bool {
        if self.is_place() {
            return TypeWalk::new(self, TypeEdges::Inline).any(Type::is_owner_lane);
        }
        match self {
            Self::Record(r) => {
                !r.owners.is_empty()
                    || r.scalars.iter().any(Type::is_owner_lane)
                    || r.closure.is_none()
                        && r.scalars
                            .iter()
                            .any(|ty| ty.is_callable_parameter() && !ty.is_copy_value())
            }
            Self::Enum(e) => !e.owners.is_empty() || e.scalars.iter().any(Type::is_owner_lane),
            Self::Tuple(t) => !t.owners.is_empty() || t.scalars.iter().any(Type::is_owner_lane),
            _ => self.is_owner_lane(),
        }
    }
    /// Mixed records, enums and tuples borrow their inline storage. One-field wrapper chains
    /// retain the buffer handle representation used by Array.
    pub(crate) fn is_inline_owner(&self) -> bool {
        if !self.is_owner() {
            return false;
        }
        let mut ty = self;
        while let Self::Record(record) = ty {
            if record.fields.len() != 1 {
                return true;
            }
            ty = &record.fields[0].ty;
        }
        matches!(
            ty,
            Self::Enum(_) | Self::Tuple(_) | Self::FixedArray(_) | Self::FixedArrayInit(_)
        )
    }
    pub(crate) fn is_buffer_target(&self) -> bool {
        self.is_owner()
            || matches!(
                self,
                Self::BufferElement(_)
                    | Self::BufferSlice(_)
                    | Self::BufferIterator(_)
                    | Self::BufferPairIterator(_)
                    | Self::BufferEntry(_)
            )
    }
    pub(crate) fn is_resource_owner(&self) -> bool {
        let mut ty = self;
        while let Self::Record(record) = ty {
            if record.fields.len() != 1 {
                return false;
            }
            ty = &record.fields[0].ty;
        }
        matches!(ty, Self::Resource(_))
    }
    pub(crate) fn is_resource_view(&self) -> bool {
        self.reference_type()
            .is_some_and(|r| r.target.is_resource_owner())
    }
    /// Storage that can stay borrowed while its owning activation is suspended.
    /// Provenance and loan checking keep the owner alive and prevent relocation.
    pub(crate) fn is_suspend_safe_view(&self) -> bool {
        self.reference_type().is_some_and(|r| {
            let target = match &r.target {
                Self::BufferElement(element)
                | Self::BufferSlice(element)
                | Self::BufferIterator(element)
                | Self::BufferPairIterator(element)
                | Self::BufferEntry(element) => element,
                target => target,
            };
            target.is_suspend_safe_owner()
                || target.reference_type().is_some_and(|r| !r.mutable)
                    && target.is_suspend_safe_view()
                || target.is_copy_storage_value()
                    && !target.contains_references()
                    && !target.contains_closures()
                    && !target.contains_dyn()
        })
    }
    pub(crate) fn is_buffer_view(&self) -> bool {
        self.reference_type()
            .is_some_and(|r| !matches!(r.target, Self::DynTrait(_)) && r.target.is_buffer_target())
    }
    /// Owning values whose complete storage tree is reference-free can be
    /// moved between ordinary CPS activations. Borrowed views and aggregates
    /// containing references need separate scoped eligibility and loan checking.
    pub(crate) fn is_suspend_safe_owner(&self) -> bool {
        if self.contains_unstorable_closures() {
            return false;
        }
        fn safe_storage(ty: &Type) -> bool {
            if ty.contains_references() {
                return false;
            }
            match ty {
                Type::String
                | Type::Resource(_)
                | Type::DynTrait(_)
                | Type::Parameter(_)
                | Type::Projection(_) => true,
                Type::FixedArray(array) => safe_storage(&array.element),
                Type::Buffer(element) => {
                    if element.is_owner() {
                        safe_storage(element)
                    } else {
                        element.is_copy_storage_value() && !element.contains_buffers()
                    }
                }
                Type::Record(_) | Type::Enum(_) | Type::Tuple(_) => ty
                    .buffer_elements()
                    .iter()
                    .all(|element| safe_storage(element)),
                _ => ty.is_copy_storage_value() && !ty.contains_buffers(),
            }
        }

        self.is_owner() && safe_storage(self)
    }
    /// Owners may stay in a local CPS activation with suspend-safe shared loans.
    /// This includes reference-free owners, but does not grant storage or transfer
    /// eligibility; provenance still checks each loan.
    pub(crate) fn is_suspend_safe_local_owner(&self) -> bool {
        if self.is_suspend_safe_owner() {
            return true;
        }
        self.is_owner()
            && self.contains_references()
            && !self.contains_unstorable_closures()
            && !TypeWalk::new(self, TypeEdges::Inline).any(Self::is_scoped_owning_tuple)
            && self.scalar_types().iter().all(|ty| {
                if let Some(reference) = ty.reference_type() {
                    !reference.mutable && ty.is_suspend_safe_view()
                } else if ty.is_owner() {
                    ty.is_suspend_safe_owner()
                } else {
                    ty.is_copy_storage_value()
                }
            })
    }
    pub(crate) fn with_buffer_element(&self, element: Type) -> Self {
        match self {
            Self::Buffer(_) => Self::Buffer(Arc::new(element)),
            Self::BufferElement(_) => Self::BufferElement(Arc::new(element)),
            Self::BufferSlice(_) => Self::BufferSlice(Arc::new(element)),
            Self::BufferIterator(_) => Self::BufferIterator(Arc::new(element)),
            Self::BufferPairIterator(_) => Self::BufferPairIterator(Arc::new(element)),
            Self::BufferEntry(_) => Self::BufferEntry(Arc::new(element)),
            _ => unreachable!("internal buffer target"),
        }
    }
    pub(crate) fn contains_buffers(&self) -> bool {
        self.is_buffer_target()
            || self
                .scalar_types()
                .iter()
                .any(|ty| ty.is_buffer_target() || ty.is_buffer_view())
    }
    /// Buffer views can be copied locally, but cannot be stored or generalized.
    pub(crate) fn is_copy_storage_value(&self) -> bool {
        self.is_copy_value() && !self.contains_buffers() && !self.contains_unstorable_closures()
    }
    /// Copy bounds grant duplication, not permission to retain a loan in storage.
    pub(crate) fn is_copy_argument(&self) -> bool {
        self.is_copy_storage_value() || self.is_scoped_shared_value()
    }
    /// Shared owner references and read-only borrowed closures can travel locally.
    /// They remain ineligible for container and arena storage.
    pub(crate) fn is_scoped_shared_value(&self) -> bool {
        self.is_copy_value()
            && (self.contains_buffers() || self.contains_unstorable_closures())
            && self.scalar_types().iter().all(Self::is_scoped_shared_lane)
    }
    fn is_scoped_shared_lane(&self) -> bool {
        if let Some(reference) = self.reference_type() {
            !reference.mutable
                && (!self.is_buffer_view() || reference.target.is_owner())
                && !reference.target.contains_dyn()
                && !reference.target.contains_explicit_arena()
        } else {
            !self.is_buffer_target() && !self.is_callable_parameter()
        }
    }
    pub(super) fn is_cursor_packet_metadata(&self) -> bool {
        self.is_copy_storage_value() && !self.contains_references()
            || self.contains_unstorable_closures() && self.is_scoped_shared_value()
    }
    /// Local cursor packets transfer exclusive cursor loans alongside Copy state,
    /// including shared loans carried by read-only callbacks.
    /// They cannot be copied, stored, borrowed as a whole, or used as enum payloads.
    pub(crate) fn is_scoped_cursor_value(&self) -> bool {
        let mut borrowed_callbacks = None;
        !self.is_owner()
            && !matches!(self, Self::Enum(_))
            && self.scalar_types().iter().any(|ty| {
                ty.reference_type()
                    .is_some_and(|r| r.mutable && matches!(r.target, Self::BufferIterator(_)))
            })
            && self.scalar_types().iter().all(|ty| {
                if let Some(r) = ty.reference_type() {
                    if r.mutable {
                        matches!(r.target, Self::BufferIterator(_))
                    } else {
                        ty.is_scoped_shared_lane()
                            && *borrowed_callbacks.get_or_insert_with(|| self.contains_unstorable_closures())
                    }
                } else {
                    ty.is_copy_storage_value() && !ty.contains_references()
                        // Rigid callbacks are checked again with their actual capture lanes.
                        || matches!(ty, Self::Parameter(p) if p.copy && p.callable.as_ref().is_some_and(|f| f.mode == CallableKind::Fn && !f.suspends))
                }
            })
    }
    pub(crate) fn is_scoped_borrowed_value(&self) -> bool {
        self.is_scoped_shared_value()
            || self.is_scoped_cursor_value()
            || self.is_scoped_owning_tuple()
    }

    /// A synchronous transition can return Copy borrow state alongside an independent owner.
    pub(super) fn is_scoped_owning_pair(elements: &[Self]) -> bool {
        let [state, item] = elements else {
            return false;
        };
        state.is_aggregate()
            && state.contains_references()
            && state.is_copy_argument()
            && state.scalar_types().iter().all(Self::is_scoped_shared_lane)
            && item.is_suspend_safe_owner()
    }

    fn is_scoped_owning_tuple(&self) -> bool {
        matches!(self, Self::Tuple(tuple) if Self::is_scoped_owning_pair(&tuple.elements))
    }

    /// All underlying element types, including shallow nominal owner views.
    pub(crate) fn buffer_elements(&self) -> &[Arc<Type>] {
        if self
            .reference_type()
            .is_some_and(|r| matches!(r.target, Self::DynTrait(_)))
        {
            return &[];
        }
        match self.reference_type().map_or(self, |r| &r.target) {
            Self::Buffer(element)
            | Self::BufferElement(element)
            | Self::BufferSlice(element)
            | Self::BufferIterator(element)
            | Self::BufferPairIterator(element)
            | Self::BufferEntry(element) => std::slice::from_ref(element),
            Self::String => {
                static BYTE: std::sync::OnceLock<Arc<Type>> = std::sync::OnceLock::new();
                std::slice::from_ref(BYTE.get_or_init(|| Arc::new(Type::UInt8)))
            }
            Self::Resource(_) | Self::DynTrait(_) => {
                static LEAF: std::sync::OnceLock<Arc<Type>> = std::sync::OnceLock::new();
                std::slice::from_ref(LEAF.get_or_init(|| Arc::new(Type::Unit)))
            }
            Self::Record(record) => &record.owners,
            Self::Enum(enumeration) => &enumeration.owners,
            Self::Tuple(tuple) => &tuple.owners,
            _ => &[],
        }
    }
    /// Copy elements may retain ordinary shared references. Owning aggregate
    /// elements move as a whole and recursively require reference-free contents.
    /// Rigid parameters are checked again after specialization.
    pub(crate) fn is_buffer_element(&self) -> bool {
        if self.contains_closures() {
            return false;
        }
        if self.contains_place_storage() {
            return self.is_fixed_array_data();
        }
        // Indirect identities carry no value layout. Their declaration bodies
        // are checked separately before any storage operation is planned.
        if matches!(self, Self::Record(r) if r.indirect)
            || matches!(self, Self::Enum(e) if e.indirect)
        {
            return true;
        }
        if self.is_owner() {
            if self.contains_references() {
                return false;
            }
            // Nested arrays can share their element DAG many times. Validate
            // each aggregate identity once rather than recursively expanding it.
            let mut pending = vec![self];
            let mut seen = std::collections::HashSet::new();
            while let Some(ty) = pending.pop() {
                let identity = match ty {
                    Self::Record(record) => Some(Arc::as_ptr(record).cast::<()>()),
                    Self::Enum(enumeration) => Some(Arc::as_ptr(enumeration).cast::<()>()),
                    Self::Tuple(tuple) => Some(Arc::as_ptr(tuple).cast::<()>()),
                    _ => None,
                };
                if identity.is_some_and(|identity| !seen.insert(identity)) {
                    continue;
                }
                if ty.is_owner() {
                    if !matches!(
                        ty,
                        Self::String
                            | Self::DynTrait(_)
                            | Self::Resource(_)
                            | Self::Parameter(_)
                            | Self::Projection(_)
                            | Self::Record(_)
                            | Self::Enum(_)
                            | Self::Tuple(_)
                    ) || (!ty
                        .scalar_types()
                        .iter()
                        .any(|ty| matches!(ty, Self::Parameter(_) | Self::Projection(_)))
                        && ty.storage_layout().is_err())
                    {
                        return false;
                    }
                    pending.extend(ty.buffer_elements().iter().map(Arc::as_ref));
                } else if !ty.is_buffer_element() || ty.contains_references() {
                    return false;
                }
            }
            return true;
        }
        self.is_copy_storage_value()
            && self.scalar_types().iter().all(|ty| {
                matches!(
                    ty,
                    Self::Float32
                        | Self::Float64
                        | Self::Int8
                        | Self::Int16
                        | Self::Int32
                        | Self::Int64
                        | Self::UInt8
                        | Self::UInt16
                        | Self::UInt32
                        | Self::Rune
                        | Self::UInt64
                        | Self::Bool
                        | Self::Ptr
                        | Self::Function(_)
                        | Self::Parameter(_)
                        | Self::Projection(_)
                ) || ty.is_domain_capability()
                    || ty
                        .reference_type()
                        .is_some_and(|r| !r.mutable && !r.target.contains_buffers())
            })
            && (self
                .scalar_types()
                .iter()
                .any(|ty| matches!(ty, Self::Parameter(_) | Self::Projection(_)))
                || self.memory_layout().is_ok())
    }
    pub fn reference(target: Type, mutable: bool) -> Self {
        let lanes = if matches!(target, Self::DynTrait(_)) {
            vec![
                Self::Reference(Arc::new(ReferenceType {
                    target: target.clone(),
                    mutable,
                    lanes: Vec::new(),
                })),
                Self::Ptr,
            ]
        } else {
            Vec::new()
        };
        Self::Reference(Arc::new(ReferenceType {
            target,
            mutable,
            lanes,
        }))
    }
    /// Shared references and owner-free Fn environments are locally Copy.
    /// Moving environments retain their owner identity; exclusive capabilities never copy.
    pub(crate) fn is_copy_value(&self) -> bool {
        self.is_copy_scalar()
            || self.is_aggregate()
                && !self.is_owner()
                && !TypeWalk::new(self, TypeEdges::Inline).any(|ty| {
                    ty.closure_info()
                        .is_some_and(|c| c.storable || c.mode != CallableKind::Fn)
                })
                && !self
                    .scalar_types()
                    .iter()
                    .any(|ty| ty.reference_type().is_some_and(|r| r.mutable))
            || self.reference_type().is_some_and(|r| !r.mutable)
    }
    pub(crate) fn contains_references(&self) -> bool {
        self.is_callable_parameter()
            || self.scalar_types().iter().any(|ty| {
                ty.reference_type().is_some()
                    || ty.is_callable_parameter()
                    // Callable fields are admitted only through a record
                    // specialization that rejects borrowing environments.
                    && !matches!(self, Self::Record(r) if r.closure.is_none())
            })
    }
    pub(crate) fn reference_type(&self) -> Option<&ReferenceType> {
        match self {
            Self::Reference(r) => Some(r),
            _ => None,
        }
    }
}

/// Unresolved generic type application; boxed to keep typed IR slots compact.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub struct TypeApplication {
    pub name: String,
    pub arguments: Vec<GenericArgument>,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Alias expansion can produce shared, zero-sized tuple DAGs whose
        // fully expanded spelling is exponentially larger than the source.
        fn display(
            ty: &Type,
            f: &mut std::fmt::Formatter<'_>,
            remaining: &mut usize,
        ) -> std::fmt::Result {
            if *remaining == 0 {
                return f.write_str("...");
            }
            *remaining -= 1;
            match ty {
                Type::FixedArray(a) | Type::FixedArrayInit(a) => {
                    f.write_str(if matches!(ty, Type::FixedArrayInit(_)) {
                        "FixedArrayInit<"
                    } else {
                        "FixedArray<"
                    })?;
                    display(&a.element, f, remaining)?;
                    write!(f, ", {}>", a.length)
                }
                Type::Buffer(element)
                | Type::BufferElement(element)
                | Type::BufferSlice(element)
                | Type::BufferIterator(element)
                | Type::BufferPairIterator(element)
                | Type::BufferEntry(element) => {
                    f.write_str(match ty {
                        Type::Buffer(_) => "Buffer<",
                        Type::BufferElement(_) => "Element<",
                        Type::BufferSlice(_) => "Slice<",
                        Type::BufferPairIterator(_) => "Entries<",
                        Type::BufferEntry(_) => "Entry<",
                        _ => "Iter<",
                    })?;
                    display(element, f, remaining)?;
                    f.write_str(">")
                }
                Type::Reference(r) => {
                    f.write_str(if r.mutable { "&mut " } else { "&" })?;
                    display(&r.target, f, remaining)
                }
                Type::Function(signature) => {
                    f.write_str("fn(")?;
                    for (i, param) in signature.signature.params.iter().enumerate() {
                        if i != 0 {
                            f.write_str(", ")?;
                        }
                        display(param, f, remaining)?;
                    }
                    f.write_str(")")?;
                    if signature.suspends {
                        f.write_str(" @suspends")?;
                    }
                    if signature.noescape {
                        f.write_str(" @noescape")?;
                    }
                    f.write_str(" -> ")?;
                    display(&signature.signature.ret, f, remaining)
                }
                Type::Float32 => f.write_str("Float32"),
                Type::Float64 => f.write_str("Float64"),
                Type::Int8 => f.write_str("Int8"),
                Type::Int16 => f.write_str("Int16"),
                Type::Int32 => f.write_str("Int32"),
                Type::Int64 => f.write_str("Int64"),
                Type::UInt8 => f.write_str("UInt8"),
                Type::UInt16 => f.write_str("UInt16"),
                Type::UInt32 => f.write_str("UInt32"),
                Type::Rune => f.write_str("Rune"),
                Type::UInt64 => f.write_str("UInt64"),
                Type::String => f.write_str("String"),
                Type::Ptr => f.write_str("Ptr"),
                Type::Bool => f.write_str("Bool"),
                Type::Unit => f.write_str("Unit"),
                Type::Named(name) => f.write_str(name),
                Type::Resource(name) => f.write_str(name),
                Type::DynTrait(d) => {
                    write!(f, "dyn {}", d.name)?;
                    if !d.equalities.is_empty() {
                        f.write_str("<")?;
                        for (index, equality) in d.equalities.iter().enumerate() {
                            if index != 0 {
                                f.write_str(", ")?;
                            }
                            write!(f, "{} = ", equality.name)?;
                            display(&equality.ty, f, remaining)?;
                        }
                        f.write_str(">")?;
                    }
                    Ok(())
                }
                Type::Projection(p) => {
                    if let Some(name) = &p.trait_name {
                        write!(f, "<{} as {}>::{}", p.base, name, p.name)
                    } else {
                        write!(f, "{}.{}", p.base, p.name)
                    }
                }
                Type::Parameter(parameter) => f.write_str(&parameter.name),
                Type::Record(record) => {
                    f.write_str(&record.name)?;
                    arguments(&record.arguments, f, remaining)
                }
                Type::Enum(enumeration) => {
                    f.write_str(&enumeration.name)?;
                    arguments(&enumeration.arguments, f, remaining)
                }
                Type::Applied(application) => {
                    f.write_str(&application.name)?;
                    arguments(&application.arguments, f, remaining)
                }
                Type::Tuple(tuple) => {
                    f.write_str("(")?;
                    for (index, ty) in tuple.elements.iter().enumerate() {
                        if index != 0 {
                            f.write_str(", ")?;
                        }
                        if *remaining == 0 {
                            f.write_str("...")?;
                            break;
                        }
                        display(ty, f, remaining)?;
                    }
                    if tuple.elements.len() == 1 {
                        f.write_str(",")?;
                    }
                    f.write_str(")")
                }
            }
        }
        fn arguments(
            args: &[GenericArgument],
            f: &mut std::fmt::Formatter<'_>,
            remaining: &mut usize,
        ) -> std::fmt::Result {
            if args.is_empty() {
                return Ok(());
            }
            f.write_str("<")?;
            for (index, ty) in args.iter().enumerate() {
                if index != 0 {
                    f.write_str(", ")?;
                }
                if *remaining == 0 {
                    f.write_str("...")?;
                    break;
                }
                match ty {
                    GenericArgument::Type(ty) => display(ty, f, remaining)?,
                    GenericArgument::Const(c) => write!(f, "{c}")?,
                };
            }
            f.write_str(">")
        }
        display(self, f, &mut 256)
    }
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub(crate) struct CallableBound {
    pub suspends: bool,
    pub mode: CallableKind,
    pub noescape: bool,
    pub signature: crate::FunctionSignature,
    pub additional_signatures: Vec<crate::FunctionSignature>,
}

/// Compiler-owned callable metadata; source declarations cannot manufacture it.
#[derive(joky_image_derive::Image, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CallableKind {
    Fn,
    FnMut,
    FnOnce,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub(crate) struct ClosureInfo {
    pub suspends: bool,
    pub storable: bool,
    pub mode: CallableKind,
    pub signature: crate::FunctionSignature,
    pub entry: String,
    pub pointer_entry: Option<String>,
}

/// A source trait capability and its named associated-type equalities.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct TraitBound {
    pub name: String,
    pub equalities: Vec<AssociatedEquality>,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct AssociatedEquality {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

/// Compact homogeneous inline shape; no element lanes are expanded here.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub struct FixedArrayType {
    pub element: Type,
    pub length: ConstArgument,
}

/// Generic parameter kinds are explicit, independent of declaration identity.
#[derive(joky_image_derive::Image, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterKind {
    Type,
    Const,
}

/// A canonical UInt64 value or a declaration-scoped rigid constant.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub enum ConstArgument {
    Value(u64),
    Parameter(Arc<TypeParameter>),
}
impl std::fmt::Display for ConstArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(n) => write!(f, "{n}"),
            Self::Parameter(p) => f.write_str(&p.name),
        }
    }
}

/// Ordered generic arguments retain their kind in syntax and instance keys.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub enum GenericArgument {
    Type(Type),
    Const(ConstArgument),
}
impl GenericArgument {
    /// Borrow a type argument without interpreting constants as types.
    pub fn as_type(&self) -> Option<&Type> {
        if let Self::Type(t) = self {
            Some(t)
        } else {
            None
        }
    }
    /// Mutably borrow the type portion during annotation normalization.
    pub fn as_type_mut(&mut self) -> Option<&mut Type> {
        if let Self::Type(t) = self {
            Some(t)
        } else {
            None
        }
    }
    pub(crate) fn try_map_type<E>(
        &self,
        mut f: impl FnMut(&Type) -> Result<Type, E>,
    ) -> Result<Self, E> {
        Ok(match self {
            Self::Type(t) => Self::Type(f(t)?),
            Self::Const(c) => Self::Const(c.clone()),
        })
    }
}
impl From<Type> for GenericArgument {
    fn from(t: Type) -> Self {
        Self::Type(t)
    }
}
impl std::fmt::Display for GenericArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Type(t) => t.fmt(f),
            Self::Const(c) => c.fmt(f),
        }
    }
}

/// A distinct rigid parameter owned by a generic declaration or trait Self.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct TypeParameter {
    pub kind: ParameterKind,
    pub(crate) callable: Option<Arc<CallableBound>>,
    pub copy: bool,
    pub name: String,
    pub bounds: Vec<TraitBound>,
    pub span: Span,
    pub(crate) id: u64,
}
impl TypeParameter {
    pub(crate) fn argument(self: &Arc<Self>) -> GenericArgument {
        match self.kind {
            ParameterKind::Type => GenericArgument::Type(Type::Parameter(self.clone())),
            ParameterKind::Const => GenericArgument::Const(ConstArgument::Parameter(self.clone())),
        }
    }
    pub fn new(name: String, span: Span) -> Self {
        Self {
            name,
            span,
            id: fresh_type_id(),
            kind: ParameterKind::Type,
            bounds: Vec::new(),
            callable: None,
            copy: false,
        }
    }
}
impl PartialEq for TypeParameter {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for TypeParameter {}

/// A resolved nominal type. Only repr_c records promise a C memory layout.
#[derive(joky_image_derive::Image, Clone)]
pub struct RecordType {
    /// Identity installed only on the embedded Array declaration.
    pub(crate) array: bool,
    /// Identity installed only on the embedded List declaration.
    pub(crate) list: bool,
    pub(crate) map: bool,
    pub(crate) set: bool,
    pub(crate) closure: Option<Arc<ClosureInfo>>,
    /// Underlying buffer elements in declaration order, retained in shallow views.
    pub(crate) owners: Vec<Arc<Type>>,
    pub(crate) repr_c: bool,
    pub(crate) indirect: bool,
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) parameters: Vec<Arc<TypeParameter>>,
    pub(crate) arguments: Vec<GenericArgument>,
    pub(crate) template: Option<Arc<RecordType>>,
    pub(crate) fields: Vec<RecordField>,
    pub(crate) scalars: Vec<Type>,
    pub(crate) depth: usize,
}

// Avoid recursively formatting shared layouts (which may expand exponentially).
impl std::fmt::Debug for RecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordType")
            .field("name", &self.name)
            .field("id", &self.id)
            .finish()
    }
}

impl PartialEq for RecordType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.arguments.len() == other.arguments.len()
            && equal_arguments(&self.arguments, &other.arguments)
    }
}
impl Eq for RecordType {}

#[derive(joky_image_derive::Image, Clone, Debug)]
pub(crate) struct RecordField {
    pub private: bool,
    pub name: String,
    pub ty: Type,
    /// Offset in the flattened scalar slots, never a byte offset.
    pub offset: usize,
}

impl Type {
    /// Scalar expansion deliberately excludes compiler-managed places.
    pub(crate) fn scalar_types(&self) -> &[Type] {
        if self.is_place() {
            return &[];
        }
        match self {
            Self::FixedArray(_) | Self::FixedArrayInit(_) => &[],
            Self::Record(record) => &record.scalars,
            Self::Enum(enumeration) => &enumeration.scalars,
            Self::Tuple(tuple) => &tuple.scalars,
            Self::Reference(r) if !r.lanes.is_empty() => &r.lanes,
            Self::Unit => &[],
            _ => std::slice::from_ref(self),
        }
    }

    /// Physical private-call lanes: a place has one address, not scalar elements.
    pub(crate) fn value_lanes(&self) -> &[Type] {
        if self.is_place() {
            std::slice::from_ref(self)
        } else {
            self.scalar_types()
        }
    }

    /// Private calls return aggregate storage and fat borrowed views through an output region.
    pub(crate) fn is_call_aggregate(&self) -> bool {
        self.is_aggregate() || self.reference_type().is_some_and(|r| r.lanes.len() > 1)
    }
    pub(crate) fn is_aggregate(&self) -> bool {
        matches!(
            self,
            Self::FixedArray(_) | Self::Record(_) | Self::Enum(_) | Self::Tuple(_)
        )
    }

    pub(crate) fn nominal(&self) -> Option<(&str, u64)> {
        match self {
            Self::Record(record) => Some((&record.name, record.id)),
            Self::Enum(enumeration) => Some((&enumeration.name, enumeration.id)),
            _ => None,
        }
    }

    pub(crate) fn layout_depth(&self) -> usize {
        match self {
            Self::FixedArray(a) | Self::FixedArrayInit(a) => a.element.layout_depth() + 1,
            Self::Function(signature) => signature.signature.type_depth(),
            Self::Record(record) => record.depth,
            Self::Enum(enumeration) => enumeration.depth,
            Self::Tuple(tuple) => tuple.depth,
            _ => 0,
        }
    }

    /// Keep legacy Unit parameter placeholders, but flatten aggregate parameters.
    pub(crate) fn parameter_slots(&self) -> usize {
        if *self == Self::Unit || self.is_place() {
            1
        } else {
            self.scalar_types().len()
        }
    }
}

pub(crate) fn fresh_type_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(crate::core::image::FIRST_DYNAMIC_ID);
    NEXT_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("nominal type identity space exhausted")
}

/// A resolved nominal enum. Its layout is private to the compiler.
#[derive(joky_image_derive::Image, Clone)]
pub struct EnumType {
    /// Compiler-private place with an overlaid, reference-free Copy payload.
    pub(crate) compact: bool,
    pub(crate) owners: Vec<Arc<Type>>,
    pub(crate) indirect: bool,
    pub(crate) id: u64,
    pub(crate) parameters: Vec<Arc<TypeParameter>>,
    pub(crate) arguments: Vec<GenericArgument>,
    pub(crate) template: Option<Arc<EnumType>>,
    pub(crate) name: String,
    pub(crate) variants: Vec<EnumVariant>,
    pub(crate) scalars: Vec<Type>,
    pub(crate) depth: usize,
}
impl std::fmt::Debug for EnumType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnumType")
            .field("name", &self.name)
            .field("id", &self.id)
            .finish()
    }
}
impl PartialEq for EnumType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.arguments.len() == other.arguments.len()
            && equal_arguments(&self.arguments, &other.arguments)
    }
}
impl Eq for EnumType {}

#[derive(joky_image_derive::Image, Clone)]
pub(crate) struct EnumVariant {
    pub name: String,
    pub payload: Option<Type>,
    /// Payload offset in the flattened scalar slots, never a byte offset.
    pub offset: usize,
}

/// Tuple syntax and canonical layout. Equality compares ordered element types.
#[derive(joky_image_derive::Image, Clone)]
pub struct TupleType {
    pub(crate) elements: Vec<Type>,
    pub(crate) owners: Vec<Arc<Type>>,
    pub(crate) scalars: Vec<Type>,
    pub(crate) depth: usize,
}
// Shared zero-sized layouts can form deep DAGs. Do not expand those DAGs
// recursively in comparisons or debug output.
impl std::fmt::Debug for TupleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TupleType")
            .field("arity", &self.elements.len())
            .field("scalar_slots", &self.scalars.len())
            .field("depth", &self.depth)
            .finish()
    }
}
impl PartialEq for TupleType {
    fn eq(&self, other: &Self) -> bool {
        self.elements.len() == other.elements.len()
            && equal_types(self.elements.iter().zip(&other.elements))
    }
}

// Aggregate arguments can form shared DAGs, including phantom types.
// Compare all kinds in one traversal instead of recursively expanding each edge.
pub(crate) fn equal_types<'a>(pairs: impl Iterator<Item = (&'a Type, &'a Type)>) -> bool {
    let mut pending: Vec<_> = pairs.collect();
    let mut tuples = std::collections::HashSet::new();
    let mut enums = std::collections::HashSet::new();
    let mut records = std::collections::HashSet::new();
    let mut functions = std::collections::HashSet::new();
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (Type::FixedArray(a), Type::FixedArray(b))
            | (Type::FixedArrayInit(a), Type::FixedArrayInit(b)) => {
                if a.length != b.length {
                    return false;
                }
                pending.push((&a.element, &b.element));
            }
            (Type::Function(a), Type::Function(b)) => {
                if a.suspends != b.suspends
                    || a.noescape != b.noescape
                    || a.signature.params.len() != b.signature.params.len()
                {
                    return false;
                }
                if !Arc::ptr_eq(a, b) && functions.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.signature.params.iter().zip(&b.signature.params));
                    pending.push((&a.signature.ret, &b.signature.ret));
                }
            }
            (Type::Reference(a), Type::Reference(b)) => {
                if a.mutable != b.mutable {
                    return false;
                }
                pending.push((&a.target, &b.target));
            }
            (Type::Record(a), Type::Record(b)) => {
                if a.id != b.id || a.arguments.len() != b.arguments.len() {
                    return false;
                }
                if !Arc::ptr_eq(a, b) && records.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    for (a, b) in a.arguments.iter().zip(&b.arguments) {
                        match (a, b) {
                            (GenericArgument::Type(a), GenericArgument::Type(b)) => {
                                pending.push((a, b))
                            }
                            _ if a == b => {}
                            _ => return false,
                        }
                    }
                }
            }
            (Type::Tuple(a), Type::Tuple(b)) => {
                if a.elements.len() != b.elements.len() {
                    return false;
                }
                if !Arc::ptr_eq(a, b) && tuples.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.elements.iter().zip(&b.elements));
                }
            }
            (Type::Enum(a), Type::Enum(b)) => {
                if a.id != b.id || a.arguments.len() != b.arguments.len() {
                    return false;
                }
                if !Arc::ptr_eq(a, b) && enums.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    for (a, b) in a.arguments.iter().zip(&b.arguments) {
                        match (a, b) {
                            (GenericArgument::Type(a), GenericArgument::Type(b)) => {
                                pending.push((a, b))
                            }
                            _ if a == b => {}
                            _ => return false,
                        }
                    }
                }
            }
            _ if a != b => return false,
            _ => {}
        }
    }
    true
}
impl Eq for TupleType {}
impl Type {
    /// Resolve an inline member to its type and flattened scalar offset.
    pub(crate) fn inline_field(&self, name: &str) -> Option<(usize, &Type)> {
        match self {
            Self::Record(record) => record
                .fields
                .iter()
                .find(|field| field.name == name)
                .map(|field| (field.offset, &field.ty)),
            Self::Tuple(tuple) => {
                let index: usize = name.parse().ok()?;
                let ty = tuple.elements.get(index)?;
                let offset = tuple.elements[..index]
                    .iter()
                    .map(|element| element.scalar_types().len())
                    .sum();
                Some((offset, ty))
            }
            _ => None,
        }
    }

    /// Build a tuple type annotation. Resolution checks nonempty Copy or owning elements
    /// and computes its layout before compilation; this is not a host ABI type.
    pub fn tuple(elements: Vec<Type>) -> Self {
        Self::Tuple(Arc::new(TupleType {
            elements,
            owners: Vec::new(),
            scalars: Vec::new(),
            depth: 0,
        }))
    }

    /// Element type and resolved length of a FixedArray.
    pub fn fixed_array_element_type(&self) -> Option<(&Type, u64)> {
        match self {
            Self::FixedArray(a) => match a.length {
                ConstArgument::Value(length) => Some((&a.element, length)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Element type of the compiler-recognized standard Array declaration.
    pub fn array_element_type(&self) -> Option<&Type> {
        let Self::Record(r) = self else { return None };
        if !r.array {
            return None;
        }
        let [field] = r.fields.as_slice() else {
            return None;
        };
        match &field.ty {
            Self::Buffer(element) => Some(element),
            _ => None,
        }
    }

    /// Element type of the compiler-recognized standard List declaration.
    pub fn list_element_type(&self) -> Option<&Type> {
        let Self::Record(r) = self else { return None };
        if !r.list {
            return None;
        }
        r.fields.first()?.ty.array_element_type()
    }

    /// Key and value types of the compiler-recognized standard Map declaration.
    pub fn map_entry_types(&self) -> Option<(&Type, &Type)> {
        let Self::Record(r) = self else { return None };
        if !r.map {
            return None;
        }
        Some((
            r.fields.get(1)?.ty.array_element_type()?,
            r.fields.get(2)?.ty.array_element_type()?,
        ))
    }

    /// Element type of the compiler-recognized standard Set declaration.
    pub fn set_element_type(&self) -> Option<&Type> {
        let Self::Record(r) = self else { return None };
        if !r.set {
            return None;
        }
        Some(r.fields.first()?.ty.map_entry_types()?.0)
    }

    /// Resolved tuple members in declaration order, without physical ABI lanes.
    pub fn tuple_element_types(&self) -> Option<&[Type]> {
        match self {
            Self::Tuple(t) => Some(&t.elements),
            _ => None,
        }
    }

    /// Resolve a nominal record field in a compiler-provided type snapshot.
    pub fn record_field_type(&self, name: &str) -> Option<&Type> {
        match self {
            Self::Record(r) => r.fields.iter().find(|f| f.name == name).map(|f| &f.ty),
            _ => None,
        }
    }

    /// Find a variant's declaration index and optional payload type.
    pub fn enum_variant(&self, name: &str) -> Option<(usize, Option<&Type>)> {
        match self {
            Self::Enum(e) => e
                .variants
                .iter()
                .enumerate()
                .find(|(_, v)| v.name == name)
                .map(|(i, v)| (i, v.payload.as_ref())),
            _ => None,
        }
    }
}

fn equal_arguments(a: &[GenericArgument], b: &[GenericArgument]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| match (a, b) {
            (GenericArgument::Type(_), GenericArgument::Type(_)) => true,
            _ => a == b,
        })
        && equal_types(
            a.iter()
                .zip(b)
                .filter_map(|(a, b)| Some((a.as_type()?, b.as_type()?))),
        )
}

impl PartialEq<Type> for GenericArgument {
    fn eq(&self, other: &Type) -> bool {
        self.as_type() == Some(other)
    }
}

impl Type {
    pub(crate) fn is_place(&self) -> bool {
        match self {
            Self::FixedArray(_) | Self::FixedArrayInit(_) => true,
            Self::Record(_) | Self::Tuple(_) | Self::Enum(_) => {
                TypeWalk::new(self, TypeEdges::Inline).any(|t| {
                    matches!(t, Self::FixedArray(_) | Self::FixedArrayInit(_))
                        || matches!(t, Self::Enum(e) if e.compact)
                })
            }
            _ => false,
        }
    }
    pub(crate) fn fixed_shape(&self) -> &FixedArrayType {
        match self {
            Self::FixedArray(a) | Self::FixedArrayInit(a) => a,
            Self::Reference(r) => r.target.fixed_shape(),
            _ => unreachable!("checked fixed-array shape"),
        }
    }
    pub(crate) fn fixed_length(&self) -> u64 {
        match self.fixed_shape().length {
            ConstArgument::Value(n) => n,
            ConstArgument::Parameter(_) => unreachable!("concrete fixed-array length"),
        }
    }
}
