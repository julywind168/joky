//! Inherent member lookup. Methods become ordinary internal functions.
use super::*;
use crate::parser::ast::AssociatedConst;
use std::{cell::RefCell, collections::HashSet, rc::Rc};

#[derive(Clone, Copy)]
pub(super) enum Receiver {
    Value,
    Borrowed { mutable: bool },
}

pub(super) struct Method {
    pub trait_id: Option<u64>,
    pub target: Type,
    pub impl_type_params: Vec<Arc<crate::core::types::TypeParameter>>,
    pub name: String,
    pub function: String,
    pub receiver: Option<Receiver>,
    pub impl_parameters: usize,
    pub method_parameters: usize,
}

/// What a constant belongs to. Module constants carry the declaring module's
/// canonical path; associated constants carry their nominal owner type.
pub(super) enum ConstantOwner {
    Module(String),
    Associated(Type),
}

pub(super) struct ConstMember {
    pub key: String,
    pub owner: ConstantOwner,
    pub parameters: Vec<Arc<crate::core::types::TypeParameter>>,
    pub declaration: AssociatedConst,
    pub value: RefCell<Option<Expr>>,
}

impl ConstantOwner {
    /// The canonical declaration key, shared with `ImportScope::constants`.
    ///
    /// The entry module has no path, so its constants keep their bare name and
    /// remain reachable from the scope that binds them.
    pub(super) fn key(&self, name: &str) -> String {
        match self {
            Self::Module(path) if path.is_empty() => name.to_owned(),
            Self::Module(path) => format!("{path}.{name}"),
            Self::Associated(ty) => member_key(ty, name),
        }
    }

    /// The owner type for associated-constant receiver checks.
    pub(super) fn target(&self) -> Option<&Type> {
        match self {
            Self::Module(_) => None,
            Self::Associated(ty) => Some(ty),
        }
    }
}

/// Scalars a constant may hold: the eight integers, both floats, Rune and Bool.
/// Structs, strings, references, pointers and aggregates stay out of the first
/// version, and Rune keeps its Unicode-scalar checking through the ordinary
/// literal rules rather than opening arbitrary integers.
fn constant_type_supported(ty: &Type) -> bool {
    ty.is_numeric() || matches!(ty, Type::Rune | Type::Bool)
}

pub(super) fn owner_name(ty: &Type) -> String {
    if matches!(ty, Type::BufferSlice(_)) {
        return "joky/slice.Slice".into();
    }
    ty.nominal()
        .map_or_else(|| ty.to_string(), |(name, _)| name.to_owned())
}

pub(super) fn member_key(ty: &Type, name: &str) -> String {
    format!("{}.{name}", owner_name(ty))
}

pub(super) fn same_owner(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::FixedArray(_), Type::FixedArray(_))
        | (Type::BufferSlice(_), Type::BufferSlice(_)) => true,
        (Type::Record(a), Type::Record(b)) => a.id == b.id,
        (Type::Enum(a), Type::Enum(b)) => a.id == b.id,
        _ => a == b,
    }
}

#[derive(Default)]
pub(super) struct Members {
    pub local_records: HashSet<u64>,
    pub traits: super::traits::Traits,
    /// Rigid checking fixes trait selection before concrete types can add
    /// inherent methods or additional traits with the same name.
    pub trait_calls: RefCell<HashMap<Span, u64>>,
    pub methods: Vec<Method>,
    pub constants: Vec<Rc<ConstMember>>,
    /// Declaration-identity index over `constants`, keyed by
    /// `ConstantOwner::key`. Dependency edges are resolved through it, so the
    /// graph never depends on the order constants were collected in.
    pub constants_by_key: HashMap<String, Rc<ConstMember>>,
    pub functions: HashSet<String>,
    pub private: HashSet<String>,
    scoped_names: HashSet<String>,
}

impl Members {
    pub fn collect(
        module: &mut AstModule,
        nominals: &HashMap<String, Type>,
        types: &TypeRegistry,
    ) -> Result<Self, Error> {
        let mut members = Self {
            local_records: module
                .records
                .iter()
                .filter(|r| !r.imported)
                .map(|r| r.id)
                .collect(),
            ..Self::default()
        };
        // A public spelling still has one global declaration. Private spellings
        // can overlap only when the defining modules are distinct.
        let mut owners = HashMap::<String, HashSet<Option<&str>>>::new();
        for implementation in module
            .implementations
            .iter()
            .filter(|i| i.trait_name.is_none())
        {
            let module_name = implementation
                .scope
                .as_ref()
                .map_or("", |s| s.module_name.as_str());
            for (member, public, span) in implementation
                .methods
                .iter()
                .map(|f| (&f.name, f.public, f.span))
                .chain(
                    implementation
                        .constants
                        .iter()
                        .map(|c| (&c.name, c.public, c.span)),
                )
            {
                let key = format!("{}.{member}", implementation.target_name);
                let scopes = owners.entry(key.clone()).or_default();
                if !scopes.is_empty() && (public || scopes.contains(&None))
                    || !scopes.insert((!public).then_some(module_name))
                    || matches!(&implementation.target, Type::Enum(e) if e.variants.iter().any(|v| v.name == *member))
                {
                    return Err(type_error(
                        &span,
                        ErrorCode::DuplicateDefinition,
                        format!("Duplicate associated member `{key}`"),
                    ));
                }
                if scopes.len() > 1 {
                    members.scoped_names.insert(key);
                }
            }
        }
        for implementation in &module.implementations {
            let target = &implementation.target;
            validate_nominal_type(target, nominals, &implementation.span)?;
            if implementation.trait_name.is_some() {
                continue;
            }
            let name = &implementation.target_name;
            for definition in &implementation.methods {
                let receiver = definition
                    .params
                    .first()
                    .filter(|p| p.name == "self")
                    .map(|p| {
                        p.ty.reference_type()
                            .map_or(Receiver::Value, |r| Receiver::Borrowed {
                                mutable: r.mutable,
                            })
                    });
                if definition.params.iter().enumerate().any(|(i, p)| {
                    p.name == "self"
                        && (i != 0 || p.ty.reference_type().map_or(&p.ty, |r| &r.target) != target)
                }) {
                    return Err(type_error(
                        &definition.span,
                        ErrorCode::TypeMismatch,
                        "Invalid self parameter",
                    ));
                }
                let function = members.scoped_key(
                    format!("{name}.{}", definition.name),
                    definition.scope.as_deref(),
                );
                if definition.scope.is_some() && !definition.public {
                    members.private.insert(function.clone());
                }
                let mut definition = definition.clone();
                // Keep the two source scopes separate until constructing the
                // internal template: impl parameters precede method parameters.
                let method_parameters = definition.type_params.len();
                definition.type_params = implementation
                    .type_params
                    .iter()
                    .chain(&definition.type_params)
                    .cloned()
                    .collect();
                members.methods.push(Method {
                    trait_id: None,
                    target: target.clone(),
                    impl_type_params: implementation.type_params.clone(),
                    name: definition.name.clone(),
                    function: function.clone(),
                    receiver,
                    impl_parameters: implementation.type_params.len(),
                    method_parameters,
                });
                members.functions.insert(function.clone());
                definition.name = function;
                module.items.push(Item::FnDef(definition));
            }
            for declaration in &implementation.constants {
                if !declaration.ty.is_numeric() && declaration.ty != Type::Bool {
                    return Err(type_error(
                        &declaration.span,
                        ErrorCode::TypeMismatch,
                        "Associated constants currently require a numeric or Bool type",
                    ));
                }
                let key = members.scoped_key(
                    member_key(target, &declaration.name),
                    declaration.scope.as_deref(),
                );
                if declaration.scope.is_some() && !declaration.public {
                    members.private.insert(key.clone());
                }
                members.constants.push(Rc::new(ConstMember {
                    key,
                    owner: ConstantOwner::Associated(target.clone()),
                    parameters: implementation.type_params.clone(),
                    declaration: declaration.clone(),
                    value: RefCell::new(None),
                }));
            }
        }
        // Module constants share the associated-constant graph and evaluator;
        // only their declaration identity differs. Their canonical key joins
        // the declaring module's path with the constant name, matching the
        // spelling `ImportScope::constants` binds for consumers.
        let mut module_constants = HashSet::new();
        for declaration in &module.constants {
            if !constant_type_supported(&declaration.ty) {
                return Err(type_error(
                    &declaration.span,
                    ErrorCode::TypeMismatch,
                    "Module constants require an integer, float, Rune or Bool type",
                ));
            }
            // The entry module has no canonical path, and its constants are
            // keyed by bare name exactly as `ImportScope` binds them.
            // Uniqueness follows the same identity: different modules may
            // each keep a private constant under one shared short name.
            let owner = ConstantOwner::Module(declaration.path.clone().unwrap_or_default());
            if !module_constants.insert(owner.key(&declaration.name)) {
                return Err(type_error(
                    &declaration.span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate module constant `{}`", declaration.name),
                ));
            }
            members.constants.push(Rc::new(ConstMember {
                key: owner.key(&declaration.name),
                owner,
                parameters: Vec::new(),
                declaration: AssociatedConst {
                    public: declaration.public,
                    scope: declaration.scope.clone(),
                    name: declaration.name.clone(),
                    ty: declaration.ty.clone(),
                    value: declaration.value.clone(),
                    span: declaration.span,
                },
                value: RefCell::new(None),
            }));
        }
        for constant in &members.constants {
            members
                .constants_by_key
                .insert(constant.key.clone(), constant.clone());
        }
        super::traits::collect(&mut members, module, types)?;
        Ok(members)
    }

    pub(super) fn scoped_key(
        &self,
        key: String,
        scope: Option<&crate::core::imports::DefinitionScope>,
    ) -> String {
        if self.scoped_names.contains(&key) {
            if let Some(scope) = scope {
                return format!("joky/private/{}/{key}", scope.module_name);
            }
        }
        key
    }

    pub fn method(
        &self,
        ty: &Type,
        name: &str,
        scope: Option<&crate::core::imports::DefinitionScope>,
    ) -> Option<&Method> {
        let scoped = if self.scoped_names.is_empty() {
            None
        } else {
            let key = member_key(ty, name);
            self.scoped_names
                .contains(&key)
                .then(|| self.scoped_key(key, scope))
        };
        self.methods.iter().find(|m| {
            m.trait_id.is_none()
                && same_owner(&m.target, ty)
                && (m.impl_type_params.is_empty()
                    || target_matches(&m.target, ty, &m.impl_type_params))
                && (m.impl_parameters > 0 || m.target == *ty)
                && m.name == name
                && scoped.as_ref().is_none_or(|key| key == &m.function)
        })
    }
}

fn target_matches(
    formal: &Type,
    actual: &Type,
    parameters: &[Arc<crate::core::types::TypeParameter>],
) -> bool {
    let mut substitutions = crate::core::types::Substitutions::default();
    super::generics::unify(
        formal,
        actual,
        parameters,
        &mut substitutions,
        Span::empty(0),
    )
    .is_ok()
        && parameters
            .iter()
            .all(|parameter| substitutions.argument(parameter).is_some())
}

impl FunctionPlan {
    pub(super) fn member_visible(&self, ty: &Type, name: &str) -> bool {
        self.definition_scope.as_ref().map_or_else(
            || {
                // Root scopes can also carry compiler-only members, such as
                // the private builder used by SQL template expansion.
                self.scope.members.contains(&member_key(ty, name))
                    || !self
                        .generics
                        .borrow()
                        .members
                        .private
                        .contains(&member_key(ty, name))
            },
            |scope| scope.members.contains(&member_key(ty, name)),
        )
    }

    pub(super) fn member_type(
        &self,
        base: &Expr,
        vars: &HashMap<String, Binding>,
        types: &TypeRegistry,
    ) -> Result<Option<Type>, Error> {
        let Some(name) = super::expression_path(base) else {
            return Ok(None);
        };
        let arguments = super::expression_type_arguments(base);
        let root = name.split('.').next().unwrap();
        if vars.contains_key(root) || types.is_parameter_name(root) {
            return Ok(None);
        }
        let Some(ty) =
            super::super::types::primitive_type(&name).or_else(|| self.named_type(&name).cloned())
        else {
            return Ok(None);
        };
        if let Some(arguments) = arguments {
            let ty = match ty {
                Type::Record(r) => types.instantiate_record(&r, arguments.to_vec()),
                Type::Enum(e) => types.instantiate_enum(&e, arguments.to_vec()),
                _ => return Ok(None),
            }
            .map_err(|error| error.with_span(base.span()))?;
            validate_nominal_type(&ty, &self.nominals, &base.span())?;
            Ok(Some(ty))
        } else {
            Ok(Some(ty))
        }
    }

    pub(super) fn associated_constant(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        types: &TypeRegistry,
    ) -> Result<Option<Rc<ConstMember>>, Error> {
        let Expr::Field {
            base,
            name,
            type_args,
            ..
        } = expr
        else {
            return Ok(None);
        };
        if !type_args.is_empty() {
            return Ok(None);
        }
        let Some(ty) = self.member_type(base, vars, types)? else {
            return Ok(None);
        };
        if !self.member_visible(&ty, name) {
            return Ok(None);
        }
        let context = self.generics.borrow();
        let key = context
            .members
            .scoped_key(member_key(&ty, name), self.definition_scope.as_deref());
        let constant = context
            .members
            .constants_by_key
            .get(&key)
            .filter(|c| {
                c.owner.target().is_some_and(|target| {
                    same_owner(target, &ty) && (!c.parameters.is_empty() || *target == ty)
                })
            })
            .cloned();
        if let Some(c) = &constant {
            if let Some(owner) = c.owner.target().filter(|_| !c.parameters.is_empty()) {
                let mut substitutions = crate::core::types::Substitutions::default();
                generics::unify(owner, &ty, &c.parameters, &mut substitutions, expr.span())?;
                for p in &c.parameters {
                    if let Some(actual) = substitutions.get(&p.id) {
                        context
                            .members
                            .traits
                            .check_bounds(p, actual, expr.span())?;
                    }
                }
            }
        }
        Ok(constant)
    }

    /// Convert dotted local paths and arbitrary receivers to the same static call.
    /// This only inspects types; evaluation remains in ordinary call planning.
    pub(super) fn member_call(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Option<Expr>, Error> {
        if let Some(call) = self.addition_call(expr, vars, functions, types)? {
            return Ok(Some(call));
        }
        if let Some(call) = self.equality_call(expr, vars, functions, types)? {
            return Ok(Some(call));
        }
        if matches!(expr, Expr::Call { callee, .. } if matches!(callee.as_str(), "Ptr.null" | "Ptr.is_null"))
        {
            return Ok(None);
        }
        // Fields containing functions are ordinary value calls, including arbitrary receivers.
        let field_call = match expr {
            Expr::Call {
                callee,
                args,
                type_args,
                span,
            } => self
                .local_callee(callee, vars, *span)
                .map(|value| (value, args, type_args, *span)),
            Expr::MethodCall(call) => Some((
                Expr::Field {
                    base: Box::new(call.receiver.clone()),
                    name: call.name.clone(),
                    type_args: Vec::new(),
                    span: call.span,
                },
                &call.args,
                &call.type_args,
                call.span,
            )),
            _ => None,
        };
        if let Some((value, args, type_args, span)) = field_call {
            if matches!(
                self.type_hint(&value, vars, functions, types),
                Some(Type::Function(_))
            ) {
                if !type_args.is_empty() {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidCall,
                        "Function values do not accept type arguments",
                    ));
                }
                return Ok(Some(Expr::Invoke {
                    callee: Box::new(value),
                    args: args.clone(),
                    span,
                }));
            }
        }
        if let Some(call) = self.explicit_trait_call(expr, vars, functions, types)? {
            return Ok(Some(call));
        }
        if self.enum_constructor(expr, vars)?.is_some() {
            return Ok(None);
        }
        let (receiver, name, type_args, args, span) = match expr {
            Expr::MethodCall(call) => (
                &call.receiver,
                call.name.as_str(),
                &call.type_args,
                &call.args,
                call.span,
            ),
            Expr::Call {
                callee,
                type_args,
                args,
                span,
            } => {
                let Some((path, name)) = callee.rsplit_once('.') else {
                    return Ok(None);
                };
                let associated = Expr::Ident {
                    name: path.into(),
                    span: *span,
                };
                if self.member_type(&associated, vars, types)?.is_some() {
                    return self.member_call(
                        &Expr::MethodCall(Box::new(crate::parser::ast::MethodCall {
                            receiver: associated,
                            name: name.into(),
                            type_args: type_args.clone(),
                            args: args.clone(),
                            span: *span,
                        })),
                        vars,
                        functions,
                        types,
                    );
                }
                let root = path.split('.').next().unwrap();
                if !vars.contains_key(root)
                    && (!path.contains('.') || !self.source_nominals().contains_key(root))
                {
                    return Ok(None);
                }
                let mut segments = path.split('.');
                let mut receiver = Expr::Ident {
                    name: segments.next().unwrap().into(),
                    span: *span,
                };
                for field in segments {
                    receiver = Expr::Field {
                        base: Box::new(receiver),
                        name: field.into(),
                        type_args: Vec::new(),
                        span: *span,
                    };
                }
                return self.member_call(
                    &Expr::MethodCall(Box::new(crate::parser::ast::MethodCall {
                        receiver,
                        name: name.into(),
                        type_args: type_args.clone(),
                        args: args.clone(),
                        span: *span,
                    })),
                    vars,
                    functions,
                    types,
                );
            }
            _ => return Ok(None),
        };
        if matches!(receiver, Expr::Ident { name, .. } if !vars.contains_key(name) && types.is_parameter_name(name))
        {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                "Type-parameter namespaces require an explicit Trait.method(...) call",
            ));
        }
        let associated = self.member_type(receiver, vars, types)?;
        if associated == Some(Type::Ptr) && matches!(name, "null" | "is_null") {
            return Ok(Some(Expr::Call {
                callee: format!("Ptr.{name}"),
                type_args: type_args.clone(),
                args: args.clone(),
                span,
            }));
        }
        let receiver_ty = associated
            .clone()
            .or_else(|| self.bound_hint(receiver, vars, functions, types))
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "Cannot determine method receiver type",
                )
            })?;
        let ty = if associated.is_none() {
            match receiver_ty.reference_type() {
                Some(reference) => self.pointee(&reference.target, types)?,
                None => receiver_ty.clone(),
            }
        } else {
            receiver_ty.clone()
        };
        let context = self.generics.borrow();
        let selected_trait = context.members.trait_calls.borrow().get(&span).copied();
        let inherent = if selected_trait.is_none() {
            context
                .members
                .method(&ty, name, self.definition_scope.as_deref())
                .filter(|_| self.member_visible(&ty, name))
        } else {
            None
        };
        if inherent.is_none()
            && associated.is_none()
            && selected_trait.is_none()
            && matches!(&ty, Type::Record(r) if matches!(r.name.as_str(), "joky/domain.ReadHandle" | "joky/domain.WriteHandle"))
            && context
                .members
                .trait_method(&ty, name, selected_trait, &self.scope, span)?
                .is_none()
        {
            if let Type::Record(handle) = &ty {
                if matches!(
                    handle.name.as_str(),
                    "joky/domain.ReadHandle" | "joky/domain.WriteHandle"
                ) {
                    let state = handle.arguments[0].as_type().expect("domain state type");
                    if let Some(method) = context
                        .members
                        .method(state, name, self.definition_scope.as_deref())
                        .filter(|_| self.member_visible(state, name))
                    {
                        let Some(Receiver::Borrowed { mutable }) = method.receiver else {
                            return Err(type_error(
                                &span,
                                ErrorCode::InvalidCall,
                                "Domain handle methods require a borrowed state receiver",
                            ));
                        };
                        if method.method_parameters != 0 || !type_args.is_empty() {
                            return Err(type_error(
                                &span,
                                ErrorCode::InvalidCall,
                                "Generic domain handle methods are not supported",
                            ));
                        }
                        let info = &functions[&method.function];
                        if info.suspends || info.signature.params.len() != args.len() + 1 {
                            return Err(type_error(
                                &span,
                                ErrorCode::InvalidCall,
                                "Domain handle method requires a synchronous matching signature",
                            ));
                        }
                        let mut exprs = vec![Expr::Let {
                            name: "$when_receiver".into(),
                            annotation: None,
                            value: Box::new(receiver.clone()),
                            span,
                        }];
                        let mut arguments = vec![Expr::Ident {
                            name: "$when_receiver".into(),
                            span,
                        }];
                        for (index, arg) in args.iter().enumerate() {
                            let name = format!("$when_arg{index}");
                            exprs.push(Expr::Let {
                                name: name.clone(),
                                annotation: Some(info.signature.params[index + 1].clone()),
                                value: Box::new(arg.clone()),
                                span,
                            });
                            arguments.push(Expr::Ident { name, span });
                        }
                        exprs.push(Expr::When(Box::new(crate::parser::ast::When {
                            method: Some(method.function.clone()),
                            label: None,
                            accesses: vec![("$when_receiver".into(), mutable)],
                            paths: vec![],
                            missing: None,
                            alias: None,
                            body: Expr::Call {
                                callee: format!("joky/member/{}", method.function),
                                type_args: vec![],
                                args: arguments,
                                span,
                            },
                            span,
                        })));
                        return Ok(Some(Expr::Block { exprs, span }));
                    }
                }
            }
        }
        let method = match inherent {
            Some(method) => method,
            None => context
                .members
                .trait_method(&ty, name, selected_trait, &self.scope, span)?
                .ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::UnknownName,
                        format!("Type '{receiver_ty}' has no method '{name}'"),
                    )
                })?,
        };
        if context.checking {
            if let Some(id) = method.trait_id {
                context.members.trait_calls.borrow_mut().insert(span, id);
            }
        }
        let bound_receiver =
            associated.is_none() || super::expression_type_arguments(receiver).is_some();
        let expected_type_args = if bound_receiver {
            method.method_parameters
        } else {
            method.impl_parameters + method.method_parameters
        };
        let inferred_callable_suffix = context.templates.get(&method.function).is_some_and(|f| {
            let start = if bound_receiver {
                method.impl_parameters
            } else {
                0
            };
            type_args.len() <= expected_type_args
                && f.type_params[start + type_args.len()..]
                    .iter()
                    .all(|p| p.callable.is_some())
        });
        if !type_args.is_empty()
            && type_args.len() != expected_type_args
            && !inferred_callable_suffix
        {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                format!("Type argument count mismatch for member `{name}`: expected {expected_type_args}"),
            ));
        }
        if associated.is_none() && method.receiver.is_none() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                format!("`{ty}.{name}` is an associated function, not an instance method"),
            ));
        }
        let mut arguments = Vec::with_capacity(args.len() + 1);
        if associated.is_none() {
            let argument = match method.receiver.unwrap() {
                Receiver::Borrowed { mutable: false }
                    if receiver_ty.reference_type().is_none()
                        && functions[&method.function].can_borrow_argument(0)
                        && !super::indexing::is_index_place(receiver)
                        && expression_path(receiver)
                            .and_then(|path| vars.get(path.split('.').next().unwrap()))
                            .is_none_or(|binding| {
                                let root = slot_type(&binding.slot);
                                receiver_ty.is_copy_value()
                                    && !receiver_ty.is_place()
                                    && (root.contains_references() || root.is_owner())
                            }) =>
                {
                    // Reuse call-scoped storage for shared temporaries and Copy
                    // fields that cannot be borrowed in place.
                    receiver.clone()
                }
                Receiver::Borrowed { mutable } if receiver_ty.reference_type().is_none() => {
                    Expr::Unary {
                        op: if mutable {
                            UnaryOp::BorrowMut
                        } else {
                            UnaryOp::Borrow
                        },
                        operand: Box::new(receiver.clone()),
                        span: receiver.span(),
                    }
                }
                Receiver::Value if receiver_ty.reference_type().is_some() => {
                    return Err(type_error(&span, ErrorCode::TypeMismatch,
                        "A by-value method requires a value receiver; read the reference explicitly"));
                }
                _ => receiver.clone(),
            };
            let argument = if matches!(method.receiver, Some(Receiver::Borrowed { mutable: true }))
                && !args.is_empty()
                && receiver_ty.reference_type().is_none()
                && super::locals::local_path(receiver).is_some_and(|(root, _)| {
                    vars.get(root)
                        .is_some_and(|binding| slot_type(&binding.slot).reference_type().is_none())
                }) {
                Expr::Call {
                    callee: "$reserve_receiver".into(),
                    type_args: vec![],
                    args: vec![argument],
                    span: receiver.span(),
                }
            } else {
                argument
            };
            arguments.push(argument);
        }
        arguments.extend_from_slice(args);
        let mut bound_arguments = if method.impl_parameters > 0 && bound_receiver {
            if method.trait_id.is_some() && matches!(method.target, Type::Parameter(_)) {
                vec![ty.clone().into()]
            } else {
                let mut substitutions = crate::core::types::Substitutions::default();
                super::generics::unify(
                    &method.target,
                    &ty,
                    &method.impl_type_params,
                    &mut substitutions,
                    span,
                )?;
                method
                    .impl_type_params
                    .iter()
                    .map(|parameter| {
                        substitutions.argument(parameter).ok_or_else(|| {
                            type_error(
                                &span,
                                ErrorCode::TypeMismatch,
                                format!(
                                    "Could not infer implementation parameter `{}`",
                                    parameter.name
                                ),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        } else {
            Vec::new()
        };
        bound_arguments.extend_from_slice(type_args);
        Ok(Some(Expr::Call {
            callee: format!("joky/member/{}", method.function),
            type_args: bound_arguments,
            args: arguments,
            span,
        }))
    }
}

impl FunctionPlan {
    /// Resolve a spelled constant reference to a declaration identity.
    ///
    /// `ImportScope::constants` maps every legal spelling — a bare module-level
    /// name, an imported name, `alias.NAME` or a full module path — to the
    /// declaring module's canonical key, so consumers never depend on how the
    /// defining module was spelled. Local bindings shadow the bare spelling,
    /// and a path rooted at a nominal type stays with associated constants.
    pub(super) fn module_constant_key(
        &self,
        path: &str,
        vars: &HashMap<String, Binding>,
        types: &TypeRegistry,
    ) -> Option<String> {
        let root = path.split('.').next()?;
        if vars.contains_key(root) || types.is_parameter_name(root) {
            return None;
        }
        if let Some((owner, _)) = path.rsplit_once('.') {
            if self
                .member_type(
                    &Expr::Ident {
                        name: owner.into(),
                        span: self.current_span,
                    },
                    vars,
                    types,
                )
                .ok()
                .flatten()
                .is_some()
            {
                return None;
            }
        }
        self.scope.constants.get(path).cloned()
    }

    /// Try to plan `expr` as a module constant read.
    ///
    /// Field projection is planned before member lookup, so a `Module.NAME`
    /// read must be recognised ahead of it; otherwise it would be reported as
    /// a field of an undefined variable.
    pub(super) fn module_constant_read(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Option<Flow>, Error> {
        let path = match expr {
            Expr::Field { base, name, .. } => {
                // Only a plain dotted path can name a module constant; any
                // other receiver is an ordinary field projection.
                let Some(owner) = expression_path(base) else {
                    return Ok(None);
                };
                format!("{owner}.{name}")
            }
            _ => return Ok(None),
        };
        self.module_constant_expr(&path, vars, functions, suspends, types)
    }

    /// Inline a module constant read as the literal produced by evaluation.
    pub(super) fn module_constant_expr(
        &mut self,
        path: &str,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Option<Flow>, Error> {
        let Some(key) = self.module_constant_key(path, vars, types) else {
            return Ok(None);
        };
        let context = self.generics.borrow();
        let constant = context.members.constants_by_key.get(&key).cloned();
        drop(context);
        let Some(constant) = constant else {
            return Ok(None);
        };
        let value = constant.value.borrow().clone().ok_or_else(|| {
            type_error(
                &constant.declaration.span,
                ErrorCode::InvalidState,
                "Constant dependency has not been evaluated",
            )
        })?;
        self.expr(&value, vars, functions, suspends, types)
            .map(Some)
    }

    pub(super) fn member_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Option<Flow>, Error> {
        if let Some(call) = self.member_call(expr, vars, functions, types)? {
            return self
                .expr_expected(&call, vars, functions, suspends, types, expected)
                .map(Some);
        }
        if let Some(constant) = self.associated_constant(expr, vars, types)? {
            let value = constant.value.borrow().clone().ok_or_else(|| {
                type_error(
                    &constant.declaration.span,
                    ErrorCode::InvalidState,
                    "Constant dependency has not been evaluated",
                )
            })?;
            return self
                .expr(
                    &value.with_span(expr.span()),
                    vars,
                    functions,
                    suspends,
                    types,
                )
                .map(Some);
        }
        // Module constants occupy the value namespace, so they follow member
        // resolution: `Foo.NAME` on a nominal type never reaches this branch.
        if let Some(path) = expression_path(expr) {
            if let Some(flow) =
                self.module_constant_expr(&path, vars, functions, suspends, types)?
            {
                return Ok(Some(flow));
            }
        }
        Ok(None)
    }
}
