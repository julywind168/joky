//! Inherent member lookup. Methods become ordinary internal functions.
use super::*;
use crate::parser::ast::AssociatedConst;
use std::{cell::RefCell, collections::HashSet, rc::Rc};

pub(super) struct Method {
    pub target: Type,
    pub name: String,
    pub function: String,
    pub receiver: bool,
}

pub(super) struct ConstMember {
    pub target: Type,
    pub declaration: AssociatedConst,
    pub value: RefCell<Option<Expr>>,
}

#[derive(Default)]
pub(super) struct Members {
    pub methods: Vec<Method>,
    pub constants: Vec<Rc<ConstMember>>,
    pub functions: HashSet<String>,
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
                let mut definition = definition.clone();
                members.methods.push(Method {
                    target: target.clone(),
                    name: definition.name.clone(),
                    function: function.clone(),
                    receiver,
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
                members.constants.push(Rc::new(ConstMember {
                    target: target.clone(),
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
            .find(|m| m.target == *ty && m.name == name)
    }
}

impl FunctionPlan {
    fn member_visible(&self, ty: &Type, name: &str) -> bool {
        self.definition_scope
            .as_ref()
            .is_none_or(|scope| scope.members.contains(&format!("{ty}.{name}")))
    }

    pub(super) fn member_type(&self, base: &Expr, vars: &HashMap<String, Binding>) -> Option<Type> {
        let Expr::Ident { name, .. } = base else {
            return None;
        };
        if vars.contains_key(name) {
            return None;
        }
        Some(match name.as_str() {
            "Int32" => Type::Int32,
            "Int64" => Type::Int64,
            "UInt32" => Type::UInt32,
            "UInt64" => Type::UInt64,
            "Bool" => Type::Bool,
            "Unit" => Type::Unit,
            _ => self.nominals.get(name)?.clone(),
        })
    }

    pub(super) fn associated_constant(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
    ) -> Option<Rc<ConstMember>> {
        let Expr::Field {
            base,
            name,
            type_args,
            ..
        } = expr
        else {
            return None;
        };
        if !type_args.is_empty() {
            return None;
        }
        let ty = self.member_type(base, vars)?;
        if !self.member_visible(&ty, name) {
            return None;
        }
        self.generics
            .borrow()
            .members
            .constants
            .iter()
            .find(|c| c.target == ty && c.declaration.name == *name)
            .cloned()
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
        let associated = self.member_type(receiver, vars);
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
        if !type_args.is_empty() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Inherent methods do not accept type arguments",
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
        Ok(Some(Expr::Call {
            callee: method.function.clone(),
            type_args: Vec::new(),
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
        if let Some(constant) = self.associated_constant(expr, vars) {
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
