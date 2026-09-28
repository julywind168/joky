//! Inherent member lookup. Methods become ordinary internal functions.
use super::*;
use crate::parser::ast::AssociatedConst;
use std::{cell::RefCell, collections::HashSet, rc::Rc};

pub(super) struct Method {
    pub target: Type,
    pub name: String,
    pub function: String,
    pub receiver: bool,
    pub generic: bool,
}

pub(super) struct ConstMember {
    pub target: Type,
    pub parameters: Vec<Arc<crate::parser::ast::TypeParameter>>,
    pub declaration: AssociatedConst,
    pub value: RefCell<Option<Expr>>,
}

pub(super) fn owner_name(ty: &Type) -> String {
    ty.nominal()
        .map_or_else(|| ty.to_string(), |(name, _)| name.to_owned())
}

pub(super) fn member_key(ty: &Type, name: &str) -> String {
    format!("{}.{name}", owner_name(ty))
}

pub(super) fn same_owner(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Record(a), Type::Record(b)) => a.id == b.id,
        (Type::Enum(a), Type::Enum(b)) => a.id == b.id,
        _ => a == b,
    }
}

#[derive(Default)]
pub(super) struct Members {
    pub methods: Vec<Method>,
    pub constants: Vec<Rc<ConstMember>>,
    pub functions: HashSet<String>,
    pub private: HashSet<String>,
}

impl Members {
    pub fn collect(
        module: &mut AstModule,
        nominals: &HashMap<String, Type>,
    ) -> Result<Self, Error> {
        let mut members = Self::default();
        let mut names = HashSet::new();
        for implementation in &module.implementations {
            let target = &implementation.target;
            let name = &implementation.target_name;
            validate_nominal_type(target, nominals, &implementation.span)?;
            for (member, span) in implementation
                .methods
                .iter()
                .map(|f| (&f.name, f.span))
                .chain(implementation.constants.iter().map(|c| (&c.name, c.span)))
            {
                if !names.insert((name.clone(), member.clone()))
                    || matches!(target, Type::Enum(e) if e.variants.iter().any(|v| v.name == *member))
                {
                    return Err(type_error(
                        &span,
                        ErrorCode::DuplicateDefinition,
                        format!("Duplicate associated member `{name}.{member}`"),
                    ));
                }
            }
            for definition in &implementation.methods {
                let receiver = definition.params.first().is_some_and(|p| p.name == "self");
                if definition
                    .params
                    .iter()
                    .enumerate()
                    .any(|(i, p)| p.name == "self" && (i != 0 || p.ty != *target))
                {
                    return Err(type_error(
                        &definition.span,
                        ErrorCode::TypeMismatch,
                        "Invalid self parameter",
                    ));
                }
                let function = format!("{name}.{}", definition.name);
                if definition.scope.is_some() && !definition.public {
                    members.private.insert(function.clone());
                }
                let mut definition = definition.clone();
                // Methods inherit their impl parameters only when becoming
                // internal function templates, after type normalization.
                definition
                    .type_params
                    .clone_from(&implementation.type_params);
                members.methods.push(Method {
                    target: target.clone(),
                    name: definition.name.clone(),
                    function: function.clone(),
                    receiver,
                    generic: !implementation.type_params.is_empty(),
                });
                members.functions.insert(function.clone());
                definition.name = function;
                module.items.push(Item::FnDef(definition));
            }
            for declaration in &implementation.constants {
                if declaration.ty.integer().is_none() && declaration.ty != Type::Bool {
                    return Err(type_error(
                        &declaration.span,
                        ErrorCode::TypeMismatch,
                        "Associated constants currently require an integer or Bool type",
                    ));
                }
                if declaration.scope.is_some() && !declaration.public {
                    members
                        .private
                        .insert(member_key(target, &declaration.name));
                }
                members.constants.push(Rc::new(ConstMember {
                    target: target.clone(),
                    parameters: implementation.type_params.clone(),
                    declaration: declaration.clone(),
                    value: RefCell::new(None),
                }));
            }
        }
        Ok(members)
    }

    pub fn method(&self, ty: &Type, name: &str) -> Option<&Method> {
        self.methods
            .iter()
            .find(|m| same_owner(&m.target, ty) && m.name == name)
    }
}

impl FunctionPlan {
    fn member_visible(&self, ty: &Type, name: &str) -> bool {
        self.definition_scope.as_ref().map_or_else(
            || {
                !self
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
        let ty = match name.as_str() {
            "Int32" => Type::Int32,
            "Int64" => Type::Int64,
            "UInt32" => Type::UInt32,
            "UInt64" => Type::UInt64,
            "Bool" => Type::Bool,
            "Unit" => Type::Unit,
            _ => match self.named_type(&name) {
                Some(ty) => ty.clone(),
                None => return Ok(None),
            },
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
        Ok(self
            .generics
            .borrow()
            .members
            .constants
            .iter()
            .find(|c| same_owner(&c.target, &ty) && c.declaration.name == *name)
            .cloned())
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
        if self.enum_constructor(expr, vars).is_some() {
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
                    && (!path.contains('.') || !self.nominals.contains_key(root))
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
                "Unconstrained type parameters have no associated members",
            ));
        }
        let associated = self.member_type(receiver, vars, types)?;
        let ty = associated
            .clone()
            .or_else(|| self.bound_hint(receiver, vars, functions, types))
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "Cannot determine method receiver type",
                )
            })?;
        let context = self.generics.borrow();
        let method = context
            .members
            .method(&ty, name)
            .filter(|_| self.member_visible(&ty, name))
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::UnknownName,
                    format!("Type `{ty}` has no method `{name}`"),
                )
            })?;
        let bound_receiver =
            associated.is_none() || super::expression_type_arguments(receiver).is_some();
        if !type_args.is_empty() && (!method.generic || bound_receiver) {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Only an unbound generic impl namespace accepts member type arguments",
            ));
        }
        if associated.is_none() && !method.receiver {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                format!("`{ty}.{name}` is an associated function, not an instance method"),
            ));
        }
        let mut arguments = Vec::with_capacity(args.len() + 1);
        if associated.is_none() {
            arguments.push(receiver.clone());
        }
        arguments.extend_from_slice(args);
        let type_args = if method.generic && bound_receiver {
            match &ty {
                Type::Record(r) => r.arguments.clone(),
                Type::Enum(e) => e.arguments.clone(),
                _ => unreachable!("generic implementations have nominal targets"),
            }
        } else {
            type_args.clone()
        };
        Ok(Some(Expr::Call {
            callee: format!("joky/member/{}", method.function),
            type_args,
            args: arguments,
            span,
        }))
    }
}

impl FunctionPlan {
    pub(super) fn member_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Option<Flow>, Error> {
        if let Some(call) = self.member_call(expr, vars, functions, types)? {
            return self
                .call_expr(&call, vars, functions, suspends, types)
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
        Ok(None)
    }
}
