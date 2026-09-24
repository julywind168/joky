//! Core expression lowering for typed MIR.

use super::*;
use crate::hir::{CoreExpr, CoreExprKind};
use crate::syntax::BinaryOp;

impl Lowerer<'_> {
    pub(in crate::mir::lower) fn lower_value(
        &mut self,
        expression: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let previous = self.current_span;
        self.current_span = self.source_spans.get(&expression.id).copied();
        let result = self.lower_value_inner(expression, function_names, types);
        self.current_span = previous;
        result
    }

    fn lower_value_inner(
        &mut self,
        expression: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if !self.is_open() {
            return Ok(None);
        }
        if matches!(&expression.kind, CoreExprKind::Call { .. }) {
            return self.lower_call(expression, function_names, types);
        }
        match &expression.kind {
            CoreExprKind::Tuple(elements) if elements.is_empty() && expression.ty == Type::Unit => {
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Unit { destination });
                Ok(Some(destination))
            }
            CoreExprKind::Unit => {
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Unit { destination });
                Ok(Some(destination))
            }
            CoreExprKind::Integer(value) => {
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Const {
                    destination,
                    value: MirConstant::Integer(*value),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Float(value) => {
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Const {
                    destination,
                    value: MirConstant::Float(*value),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Duration(value) => {
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Const {
                    destination,
                    value: MirConstant::Integer(*value),
                });
                Ok(Some(destination))
            }
            CoreExprKind::String(value) => {
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Const {
                    destination,
                    value: MirConstant::String(value.clone()),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Bytes(value) => {
                let text = self.next_value(Type::String);
                self.push_statement(MirStatement::Const {
                    destination: text,
                    value: MirConstant::String(value.clone()),
                });
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::RuntimeCall {
                    destination,
                    intrinsic: RuntimeIntrinsic::BytesFromString,
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value: text,
                    }],
                });
                Ok(Some(destination))
            }
            CoreExprKind::InterpolatedString(parts) => {
                let mut result = None;
                for (literal, expression) in parts {
                    if !literal.is_empty() {
                        let text = self.next_value(Type::String);
                        self.push_statement(MirStatement::Const {
                            destination: text,
                            value: MirConstant::String(literal.clone()),
                        });
                        result = Some(match result {
                            Some(previous) => self.debug_concat(previous, text),
                            None => text,
                        });
                    }
                    if let Some(expression) = expression {
                        let Some(displayed) = self.lower_show(expression, function_names, types)?
                        else {
                            return Ok(None);
                        };
                        result = Some(match result {
                            Some(previous) => self.debug_concat(previous, displayed),
                            None => displayed,
                        });
                    }
                }
                Ok(Some(result.unwrap_or_else(|| self.debug_text(""))))
            }
            CoreExprKind::Boolean(value) => {
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Const {
                    destination,
                    value: MirConstant::Boolean(*value),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Name(name) => {
                if let Some((local, index)) = self.expression_capture(expression, name) {
                    return self
                        .read_capture(local, index, expression.ty, false)
                        .map(Some);
                }
                if name == "None" && matches!(expression.ty, Type::Option(_)) {
                    let Type::Option(id) = expression.ty else {
                        unreachable!()
                    };
                    let destination = self.next_value(expression.ty);
                    self.push_statement(MirStatement::EnumConstruct {
                        destination,
                        enum_id: MirTypeId::Option(id),
                        variant: 1,
                        arguments: Vec::new(),
                    });
                    return Ok(Some(destination));
                }
                if let Some(local) = self.resolve_expression_local(expression, name) {
                    let destination = self.next_value(expression.ty);
                    if types.is_owned(expression.ty) {
                        self.push_statement(MirStatement::TakeLocal { destination, local });
                    } else if types.is_shared(expression.ty) {
                        let borrowed = self.next_value(expression.ty);
                        self.push_statement(MirStatement::Read {
                            destination: borrowed,
                            local,
                        });
                        self.push_statement(MirStatement::Dup {
                            destination,
                            value: borrowed,
                        });
                    } else {
                        self.push_statement(MirStatement::Read { destination, local });
                    }
                    return Ok(Some(destination));
                }
                if let Some(receiver_local) = self.receiver_local {
                    let receiver_ty = self.locals[receiver_local.0].ty;
                    let is_field = match receiver_ty {
                        Type::Struct(id) => types
                            .struct_fields(id)
                            .iter()
                            .any(|(field, _)| field == name),
                        Type::Class(id) => types
                            .class_fields(id)
                            .iter()
                            .any(|(field, _)| field == name),
                        _ => false,
                    };
                    if is_field {
                        let base = if types.is_owned(receiver_ty) {
                            let base =
                                self.next_value_with_ownership(receiver_ty, MirOwnership::Borrowed);
                            self.push_statement(MirStatement::BorrowLocal {
                                destination: base,
                                local: receiver_local,
                            });
                            base
                        } else {
                            let base = self.next_value(receiver_ty);
                            self.push_statement(MirStatement::Read {
                                destination: base,
                                local: receiver_local,
                            });
                            base
                        };
                        let destination = if types.is_owned(expression.ty) {
                            self.next_value_with_ownership(expression.ty, MirOwnership::Borrowed)
                        } else {
                            self.next_value(expression.ty)
                        };
                        let index = match receiver_ty {
                            Type::Struct(id) => types
                                .struct_fields(id)
                                .iter()
                                .position(|(field, _)| field == name),
                            Type::Class(id) => types
                                .class_fields(id)
                                .iter()
                                .position(|(field, _)| field == name),
                            _ => None,
                        }
                        .expect("field was checked above");
                        self.push_statement(MirStatement::Project {
                            destination,
                            base,
                            access: MirFieldAccess::Index(index),
                        });
                        return Ok(Some(destination));
                    }
                }
                Err(Diagnostic::codegen(format!(
                    "MIR local '{name}' was not resolved"
                )))
            }
            CoreExprKind::Block(expressions) => {
                self.bindings.push(HashMap::new());
                let closure_bindings = self.closure_bindings.clone();
                let mut result = None;
                for (index, expression) in expressions.iter().enumerate() {
                    if !self.is_open() {
                        break;
                    }
                    result = match &expression.kind {
                        CoreExprKind::Call {
                            callee, arguments, ..
                        } if index + 1 < expressions.len()
                            && matches!(&callee.kind, CoreExprKind::Name(name) if name == "echo") =>
                        {
                            self.lower_echo(arguments, true, function_names, types)?
                        }
                        _ => self.lower_value(expression, function_names, types)?,
                    };
                    if index + 1 < expressions.len() {
                        self.discard_value(result.take());
                    }
                }
                self.bindings.pop();
                self.closure_bindings = closure_bindings;
                Ok(result)
            }
            CoreExprKind::Let {
                name,
                value,
                mutable,
                ..
            } => {
                let closure_name = if *mutable {
                    None
                } else {
                    match &value.kind {
                        CoreExprKind::Closure {
                            function_name,
                            captures,
                            ..
                        } => self.local_closure_binding(function_name, captures),
                        CoreExprKind::Name(source) => self.closure_bindings.get(source).cloned(),
                        _ => None,
                    }
                };
                let value = self.lower_value(value, function_names, types)?;
                let local = self.new_local(
                    name,
                    value
                        .as_ref()
                        .map(|id| self.value_types[id.0])
                        .unwrap_or(Type::Unit),
                );
                self.bind_local(name, local);
                if *mutable {
                    self.source_locals.insert(expression.id, local);
                    self.mutable_locals
                        .insert(local, self.current_span.unwrap_or(crate::Span::new(0, 0)));
                }
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Bind {
                    local,
                    value,
                    destination,
                });
                if let Some(closure_name) = closure_name {
                    self.closure_bindings.insert(name.clone(), closure_name);
                } else {
                    self.closure_bindings.remove(name);
                }
                Ok(Some(destination))
            }
            CoreExprKind::LetPattern {
                pattern,
                binding_ids,
                mutable,
                value,
            } => {
                let Some(value) = self.lower_value(value, function_names, types)? else {
                    return Ok(None);
                };
                self.lower_pattern_bindings_with_mutable(pattern, value, types, *mutable)?;
                for (name, id) in binding_ids {
                    if let Some(local) = self.resolve_local(name) {
                        self.source_locals.insert(*id, local);
                    }
                    self.closure_bindings.remove(name);
                }
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Unit { destination });
                Ok(Some(destination))
            }
            CoreExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.lower_if(condition, then_branch, else_branch, function_names, types),
            CoreExprKind::For {
                index,
                item,
                iterable,
                body,
                limit,
            } => self.lower_for(
                expression,
                index.as_deref(),
                item,
                iterable,
                body,
                limit.as_deref(),
                function_names,
                types,
            ),
            CoreExprKind::ForWorker {
                index,
                item,
                input_type,
                state,
                body,
                collect,
            } => {
                self.lower_for_iterations(
                    index.as_deref(),
                    item,
                    *input_type,
                    state,
                    body,
                    *collect,
                    function_names,
                    types,
                )?;
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Unit { destination });
                Ok(Some(destination))
            }
            CoreExprKind::While { condition, body } => {
                self.lower_while(condition, body, function_names, types)?;
                Ok(None)
            }
            CoreExprKind::Loop { body } => self.lower_loop(body, function_names, types),
            CoreExprKind::Break { value } => {
                self.lower_break(value.as_deref(), function_names, types)?;
                Ok(None)
            }
            CoreExprKind::Abort { value } => {
                let Some(target) = self.resumable_handlers.iter().rev().find_map(|handlers| {
                    handlers
                        .values()
                        .find(|target| target.aborts)
                        .and_then(|target| target.abort_target)
                }) else {
                    return Err(Diagnostic::codegen(
                        "MIR abort has no enclosing resumable handler",
                    ));
                };
                let Some(value) = self.lower_value(value, function_names, types)? else {
                    return Ok(None);
                };
                self.terminate(MirTerminator::Goto {
                    target,
                    arguments: vec![value],
                })?;
                Ok(None)
            }
            CoreExprKind::Continue => {
                self.lower_continue(types)?;
                Ok(None)
            }
            CoreExprKind::When {
                cowns,
                bindings,
                until,
                body,
            } => self.lower_when(
                cowns,
                bindings.as_deref(),
                until.as_deref(),
                body,
                function_names,
                types,
            ),
            CoreExprKind::Unwrap { value, propagate } => {
                self.lower_unwrap(value, *propagate, function_names, types)
            }
            CoreExprKind::Cast {
                value,
                mode,
                target,
            } => self.lower_cast(value, *mode, *target, expression, function_names, types),
            CoreExprKind::Unary { op, expression } => {
                let Some(operand) = self.lower_value(expression, function_names, types)? else {
                    return Ok(None);
                };
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Unary {
                    destination,
                    op: *op,
                    operand,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Binary { .. } => {
                self.lower_binary_value(expression, function_names, types)
            }
            CoreExprKind::Tuple(elements) => {
                let mut values = Vec::with_capacity(elements.len());
                for element in elements {
                    let Some(value) = self.lower_value(element, function_names, types)? else {
                        return Ok(None);
                    };
                    values.push(value);
                }
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Tuple {
                    destination,
                    elements: values,
                });
                Ok(Some(destination))
            }
            CoreExprKind::CollectionLiteral(literal) => {
                self.lower_collection_literal(literal, expression.ty, function_names, types)
            }
            CoreExprKind::StructInit { name, fields }
                if matches!(expression.ty, Type::Struct(_) | Type::Class(_)) =>
            {
                let fields = self.lower_constructor_fields(
                    expression.ty,
                    fields
                        .iter()
                        .map(|(label, value)| (Some(label.as_str()), value)),
                    function_names,
                    types,
                )?;
                if let Some(item) = types.range_item(expression.ty) {
                    let zero = self.next_value(item);
                    self.push_statement(MirStatement::Const {
                        destination: zero,
                        value: MirConstant::Integer(0),
                    });
                    let invalid = self.next_value(Type::Bool);
                    self.push_statement(MirStatement::Binary {
                        destination: invalid,
                        op: BinaryOp::Equal,
                        left: fields[2],
                        right: zero,
                    });
                    let failure = self.new_block();
                    let valid = self.new_block();
                    self.terminate(MirTerminator::Branch {
                        condition: invalid,
                        then_block: failure,
                        else_block: valid,
                    })?;
                    self.switch_to(failure);
                    self.lower_panic(
                        &[crate::hir::CoreCallArgument {
                            label: None,
                            value: CoreExpr {
                                id: expression.id,
                                ty: Type::String,
                                kind: CoreExprKind::String("range step must not be zero".into()),
                            },
                        }],
                        function_names,
                        types,
                    )?;
                    self.switch_to(valid);
                }
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Construct {
                    destination,
                    type_id: MirTypeId::from_type(expression.ty).expect("struct/class type"),
                    fields,
                });
                Ok(Some(destination))
            }
            CoreExprKind::Field {
                value,
                access: FieldAccess::Name(variant),
            } if matches!(&value.kind, CoreExprKind::Name(name)
                    if self.resolve_local(name).is_none()
                        && types.enum_type(name) == Some(expression.ty))
                && matches!(expression.ty, Type::Enum(_)) =>
            {
                let CoreExprKind::Name(_enum_name) = &value.kind else {
                    unreachable!("enum constructors require a named enum");
                };
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::EnumConstruct {
                    destination,
                    enum_id: MirTypeId::from_type(expression.ty).expect("enum type"),
                    variant: types
                        .enum_variants(match expression.ty {
                            Type::Enum(id) => id,
                            _ => unreachable!(),
                        })
                        .iter()
                        .position(|candidate| candidate.name == *variant)
                        .expect("enum variant resolved"),
                    arguments: Vec::new(),
                });
                Ok(Some(destination))
            }
            CoreExprKind::Match { value, arms } => {
                let borrowed = matches!(&value.kind, CoreExprKind::Name(name)
                    if self.resolve_expression_local(value, name)
                        .is_some_and(|local| self.locals[local.0].ownership == MirOwnership::Borrowed));
                let scrutinee = if borrowed {
                    self.lower_borrowed_value(value, function_names, types)?
                } else {
                    self.lower_value(value, function_names, types)?
                };
                let Some(scrutinee) = scrutinee else {
                    return Ok(None);
                };
                self.lower_match(scrutinee, arms, expression.ty, function_names, types)
            }
            CoreExprKind::Assign { target, value } => {
                if let CoreExprKind::Name(name) = &target.kind {
                    if let Some((local, index)) = self.expression_capture(target, name) {
                        let Some(value) = self.lower_value(value, function_names, types)? else {
                            return Ok(None);
                        };
                        if !self.is_open() {
                            return Ok(None);
                        }
                        let receiver = self.borrow_capture_state(local);
                        let destination = self.next_value(Type::Unit);
                        self.push_statement(MirStatement::Store {
                            destination,
                            receiver,
                            access: MirFieldAccess::Index(index),
                            value,
                        });
                        return Ok(Some(destination));
                    }
                    let local = self.resolve_expression_local(target, name).ok_or_else(|| {
                        Diagnostic::codegen("local assignment target was not resolved")
                    })?;
                    let value = self.lower_value(value, function_names, types)?;
                    if !self.is_open() {
                        return Ok(None);
                    }
                    let destination = self.next_value(Type::Unit);
                    self.assignments.insert(destination);
                    self.push_statement(MirStatement::Bind {
                        destination,
                        local,
                        value,
                    });
                    return Ok(Some(destination));
                }
                let CoreExprKind::Field {
                    value: receiver,
                    access,
                } = &target.kind
                else {
                    return Err(Diagnostic::codegen("unsupported MIR assignment target"));
                };
                let Some(receiver) = self.lower_borrowed_value(receiver, function_names, types)?
                else {
                    return Ok(None);
                };
                let Some(value) = self.lower_value(value, function_names, types)? else {
                    return Ok(None);
                };
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Store {
                    destination,
                    receiver,
                    access: resolve_field_access(access, self.value_types[receiver.0], types)?,
                    value,
                });
                Ok(Some(destination))
            }
            CoreExprKind::CompoundAssign {
                target,
                operator,
                value,
            } => self.lower_compound_assign(target, *operator, value, function_names, types),
            CoreExprKind::Field { value, access }
                if !(matches!(value.ty, Type::Enum(_))
                    && matches!(access, FieldAccess::Name(_))) =>
            {
                if types.is_owned(expression.ty)
                    && matches!(value.ty, Type::Tuple(_) | Type::Struct(_))
                {
                    return self.lower_owned_field(value, access, function_names, types);
                }
                let Some(base) = self.lower_borrowed_value(value, function_names, types)? else {
                    return Ok(None);
                };
                let mir_access = resolve_field_access(access, value.ty, types)?;
                let destination = if types.is_owned(expression.ty) {
                    self.next_value_with_ownership(expression.ty, MirOwnership::Borrowed)
                } else {
                    self.next_value(expression.ty)
                };
                self.push_statement(MirStatement::Project {
                    destination,
                    base,
                    access: mir_access,
                });
                if types.is_shared(value.ty) || types.is_shared(expression.ty) {
                    let result = if types.is_shared(expression.ty) {
                        let duplicate =
                            self.next_value_with_ownership(expression.ty, MirOwnership::Shared);
                        self.push_statement(MirStatement::Dup {
                            destination: duplicate,
                            value: destination,
                        });
                        duplicate
                    } else {
                        destination
                    };
                    if types.is_shared(value.ty) {
                        self.discard_value(Some(base));
                    }
                    return Ok(Some(result));
                }
                Ok(Some(destination))
            }
            CoreExprKind::Do { body, handlers } => {
                self.lower_do(expression.ty, body, handlers, function_names, types)
            }
            CoreExprKind::Parallel(arms) => {
                self.lower_parallel(arms, expression.ty, function_names, types)
            }
            CoreExprKind::Race(arms) => self.lower_race(arms, expression.ty, function_names, types),
            CoreExprKind::Region(body) => {
                let scope = self.new_task_scope();
                if crate::hir::iteration_has_branches(body) {
                    self.branch_regions.insert(scope);
                }
                let exit = self.new_block();
                self.push_statement(MirStatement::ScopeEnter {
                    scope,
                    region: true,
                });
                self.task_scopes.push(scope);
                let entry = self.new_block();
                self.mark_scoped(entry);
                self.terminate(MirTerminator::Goto {
                    target: entry,
                    arguments: vec![],
                })?;
                self.switch_to(entry);
                let result = self.lower_value(body, function_names, types)?;
                if self.is_open() && self.branch_regions.contains(&scope) {
                    let complete = self.new_block();
                    self.lower_task_failure_check(scope, complete, types)?;
                    self.switch_to(complete);
                }
                let from = self.current;
                let completes = self.is_open();
                if completes {
                    // Scope-edge drops run before the region group is destroyed.
                    self.blocks[from.0].terminator = Some(MirTerminator::Goto {
                        target: exit,
                        arguments: result.into_iter().collect(),
                    });
                }
                self.task_scopes.pop();
                if !completes {
                    self.blocks[exit.0].terminator = Some(MirTerminator::Unreachable);
                    return Ok(None);
                }
                self.switch_to(exit);
                let result = result.map(|value| {
                    let destination = self.next_value(expression.ty);
                    self.push_statement(MirStatement::Phi {
                        destination,
                        incoming: vec![(from, value)],
                    });
                    destination
                });
                self.push_statement(MirStatement::ScopeExit { scope });
                Ok(result)
            }
            CoreExprKind::Branch(body) => {
                let scope = *self
                    .task_scopes
                    .last()
                    .ok_or_else(|| Diagnostic::codegen("MIR branch has no enclosing task scope"))?;
                self.lower_task_create(expression.id, body, scope, function_names, types)?;
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Unit { destination });
                Ok(Some(destination))
            }
            CoreExprKind::Closure {
                function_name,
                captures,
                ..
            } => self.lower_closure(expression, function_name, captures, function_names, types),
            _ => Err(Diagnostic::codegen(format!(
                "unsupported expression in MIR lowering: {:?}",
                expression.kind
            ))),
        }
    }

    fn lower_compound_assign(
        &mut self,
        target: &CoreExpr,
        operator: BinaryOp,
        value: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let place = target.ty;
        if let CoreExprKind::Name(name) = &target.kind {
            if let Some((local, index)) = self.expression_capture(target, name) {
                let receiver = self.borrow_capture_state(local);
                let current = self.next_value(place);
                self.push_statement(MirStatement::Project {
                    destination: current,
                    base: receiver,
                    access: MirFieldAccess::Index(index),
                });
                let Some(right) = self.lower_value(value, function_names, types)? else {
                    return Ok(None);
                };
                if !self.is_open() {
                    return Ok(None);
                }
                let updated = self.next_value(place);
                self.push_statement(MirStatement::Binary {
                    destination: updated,
                    op: operator,
                    left: current,
                    right,
                });
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Store {
                    destination,
                    receiver,
                    access: MirFieldAccess::Index(index),
                    value: updated,
                });
                return Ok(Some(destination));
            }
            let local = self
                .resolve_expression_local(target, name)
                .ok_or_else(|| Diagnostic::codegen("local assignment target was not resolved"))?;
            let current = self.next_value(place);
            self.push_statement(MirStatement::Read {
                destination: current,
                local,
            });
            let Some(right) = self.lower_value(value, function_names, types)? else {
                return Ok(None);
            };
            if !self.is_open() {
                return Ok(None);
            }
            let updated = self.next_value(place);
            self.push_statement(MirStatement::Binary {
                destination: updated,
                op: operator,
                left: current,
                right,
            });
            let destination = self.next_value(Type::Unit);
            self.assignments.insert(destination);
            self.push_statement(MirStatement::Bind {
                destination,
                local,
                value: Some(updated),
            });
            return Ok(Some(destination));
        }
        let CoreExprKind::Field {
            value: receiver,
            access,
        } = &target.kind
        else {
            return Err(Diagnostic::codegen("unsupported MIR assignment target"));
        };
        let Some(receiver) = self.lower_borrowed_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let current = self.next_value(place);
        self.push_statement(MirStatement::Project {
            destination: current,
            base: receiver,
            access: resolve_field_access(access, self.value_types[receiver.0], types)?,
        });
        let Some(right) = self.lower_value(value, function_names, types)? else {
            return Ok(None);
        };
        if !self.is_open() {
            return Ok(None);
        }
        let updated = self.next_value(place);
        self.push_statement(MirStatement::Binary {
            destination: updated,
            op: operator,
            left: current,
            right,
        });
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Store {
            destination,
            receiver,
            access: resolve_field_access(access, self.value_types[receiver.0], types)?,
            value: updated,
        });
        Ok(Some(destination))
    }

    fn lower_binary_value(
        &mut self,
        expression: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let mut stack = Vec::new();
        let mut current = expression;
        while let CoreExprKind::Binary { op, left, right } = &current.kind {
            stack.push((current, *op, right.as_ref()));
            current = left.as_ref();
        }
        let Some(mut left) = self.lower_value(current, function_names, types)? else {
            return Ok(None);
        };
        for (node, op, right) in stack.into_iter().rev() {
            if matches!(op, BinaryOp::And | BinaryOp::Or) {
                let Some(value) =
                    self.lower_logical_with_left(op, left, right, function_names, types)?
                else {
                    return Ok(None);
                };
                left = value;
                continue;
            }
            let Some(right) = self.lower_value(right, function_names, types)? else {
                return Ok(None);
            };
            left = self.emit_binary(node.ty, op, left, right)?;
        }
        Ok(Some(left))
    }

    fn emit_binary(
        &mut self,
        ty: Type,
        op: BinaryOp,
        left: MirValueId,
        right: MirValueId,
    ) -> Result<MirValueId, Diagnostic> {
        if self.value_types[left.0] == Type::String {
            let intrinsic = match op {
                BinaryOp::Add => RuntimeIntrinsic::StringConcat,
                BinaryOp::Equal => RuntimeIntrinsic::StringEqual,
                BinaryOp::NotEqual => RuntimeIntrinsic::StringNotEqual,
                _ => return Err(Diagnostic::codegen("unsupported String binary operation")),
            };
            let destination = self.next_value(ty);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic,
                arguments: vec![
                    MirCallArgument {
                        parameter: 0,
                        value: left,
                    },
                    MirCallArgument {
                        parameter: 1,
                        value: right,
                    },
                ],
            });
            return Ok(destination);
        }
        let destination = self.next_value(ty);
        self.push_statement(MirStatement::Binary {
            destination,
            op,
            left,
            right,
        });
        Ok(destination)
    }
}
