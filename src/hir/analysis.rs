use std::collections::{BTreeMap, HashSet};

use crate::syntax::{walk_expr, ExprVisitor};

use super::*;

// Task functions are materialized after closures, so tasks nested in a
// closure receive the same treatment as tasks in an ordinary function.
pub(super) fn materialize_task_functions(functions: &mut Vec<CoreFunction>, types: &CheckedTypes) {
    let mut task_cursor = 0;
    while task_cursor < functions.len() {
        let mut task_functions = Vec::new();
        collect_task_functions(&functions[task_cursor], types, &mut task_functions);
        for (name, module, captures, return_type, body) in task_functions {
            if functions.iter().any(|function| function.name == name) {
                continue;
            }
            let id = CoreFunctionId(functions.len());
            functions.push(CoreFunction {
                closure_state: None,
                foreign: None,
                id,
                module,
                name,
                visibility: Visibility::Private,
                receiver: None,
                receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                parameters: captures
                    .into_iter()
                    .map(|(name, ty)| CoreParameter { name, ty })
                    .collect(),
                borrowed_parameters: 0,
                parameter_ownership: Vec::new(),
                return_type,
                declared_effects: crate::sema::EffectGroupSet::default(),
                used_effects: EffectSet::new(),
                may_suspend: false,
                body,
            });
        }
        task_cursor += 1;
    }
}

pub(super) fn collect_closure_functions(
    function: &CoreFunction,
    output: &mut Vec<ClosureFunction>,
) {
    collect_closures(&function.body, output);
}

pub(crate) fn task_function_name(owner: &str, id: NodeId) -> String {
    format!("__task_{owner}_{id:?}")
}

/// Abort-only handlers can dispatch one direct operation in the caller's CFG,
/// without creating a task that would capture a surrounding Cown lease.
pub(crate) fn is_single_effect_call(body: &CoreExpr) -> bool {
    match &body.kind {
        CoreExprKind::Block(values) if values.len() == 1 => matches!(
            values[0].kind,
            CoreExprKind::Call {
                effect_operation: Some(_),
                ..
            }
        ),
        CoreExprKind::Call {
            effect_operation: Some(_),
            ..
        } => true,
        _ => false,
    }
}

fn collect_task_functions(
    function: &CoreFunction,
    types: &CheckedTypes,
    output: &mut Vec<TaskFunction>,
) {
    let mut available = function
        .parameters
        .iter()
        .map(|parameter| (parameter.name.clone(), parameter.ty))
        .collect::<BTreeMap<_, _>>();
    if let Some(receiver) = function.receiver {
        available.insert("self".to_owned(), receiver);
    }
    collect_tasks_in_expr(
        &function.name,
        function.module,
        &function.body,
        &mut available,
        types,
        output,
    );
}

fn collect_tasks_in_expr(
    owner: &str,
    module: CoreModuleId,
    expression: &CoreExpr,
    available: &mut BTreeMap<String, Type>,
    types: &CheckedTypes,
    output: &mut Vec<TaskFunction>,
) {
    match &expression.kind {
        CoreExprKind::For {
            index,
            item,
            iterable,
            body,
            limit,
        } => {
            collect_tasks_in_expr(owner, module, iterable, available, types, output);
            if let Some(limit) = limit {
                collect_tasks_in_expr(owner, module, limit, available, types, output);
                let state = format!("<for-state-{:?}>", expression.id);
                let worker = CoreExpr {
                    id: body.id,
                    ty: Type::Unit,
                    kind: CoreExprKind::ForWorker {
                        index: index.clone(),
                        item: item.clone(),
                        input_type: iterable.ty,
                        state: state.clone(),
                        body: body.clone(),
                        collect: matches!(expression.ty, Type::List(_)),
                    },
                };
                let mut captures: Vec<_> = task_captures(&worker)
                    .into_iter()
                    .filter(|(name, _)| available.contains_key(name) || *name == state)
                    .collect();
                captures.sort_by(|a, b| a.0.cmp(&b.0));
                output.push((
                    task_function_name(owner, body.id),
                    module,
                    captures,
                    Type::Unit,
                    worker,
                ));
            } else {
                let mut scoped = available.clone();
                let cursor = types.cursor_type(iterable.id).unwrap_or(iterable.ty);
                scoped.insert(
                    item.clone(),
                    types
                        .cursor_item_type(cursor)
                        .expect("checked Cursor input"),
                );
                if let Some(index) = index {
                    scoped.insert(index.clone(), Type::U64);
                }
                collect_tasks_in_expr(owner, module, body, &mut scoped, types, output);
            }
        }
        CoreExprKind::ForWorker {
            index,
            item,
            input_type,
            body,
            ..
        } => {
            let mut scoped = available.clone();
            let Type::List(id) = input_type else {
                unreachable!("checked for input")
            };
            scoped.insert(item.clone(), types.list_type(*id));
            if let Some(index) = index {
                scoped.insert(index.clone(), Type::U64);
            }
            collect_tasks_in_expr(owner, module, body, &mut scoped, types, output);
        }
        CoreExprKind::Parallel(arms) | CoreExprKind::Race(arms) => {
            for arm in arms {
                let captures = task_captures(arm)
                    .into_iter()
                    .filter(|(name, _)| available.contains_key(name))
                    .collect();
                output.push((
                    task_function_name(owner, arm.id),
                    module,
                    captures,
                    arm.ty,
                    arm.clone(),
                ));
            }
        }
        CoreExprKind::Branch(body) => {
            let captures = task_captures(body)
                .into_iter()
                .filter(|(name, _)| available.contains_key(name))
                .collect();
            output.push((
                task_function_name(owner, expression.id),
                module,
                captures,
                body.ty,
                (**body).clone(),
            ));
        }
        CoreExprKind::Do { body, handlers } => {
            let direct_abort_handler = is_single_effect_call(body)
                && !handlers.is_empty()
                && handlers.iter().all(|handler| {
                    !handler.resumes
                        && types.effects().operation_mode(handler.operation)
                            == Some(crate::sema::EffectMode::Aborts)
                });
            let is_normal_handler = !handlers.is_empty()
                && handlers.iter().all(|handler| {
                    !handler.resumes
                        && types.effects().operation_mode(handler.operation)
                            == Some(crate::sema::EffectMode::Normal)
                });
            if !direct_abort_handler
                && !handlers.iter().all(|handler| handler.resumes)
                && !(is_normal_handler && !contains_non_normal_effect(body, types))
            {
                let captures = task_captures(body)
                    .into_iter()
                    .filter(|(name, _)| available.contains_key(name))
                    .collect();
                output.push((
                    task_function_name(owner, body.id),
                    module,
                    captures,
                    body.ty,
                    (**body).clone(),
                ));
            }
            collect_tasks_in_expr(owner, module, body, &mut available.clone(), types, output);
            for handler in handlers {
                collect_tasks_in_expr(
                    owner,
                    module,
                    &handler.value,
                    &mut available.clone(),
                    types,
                    output,
                );
            }
        }
        CoreExprKind::Let { name, value, .. } => {
            collect_tasks_in_expr(owner, module, value, available, types, output);
            available.insert(name.clone(), value.ty);
        }
        CoreExprKind::LetPattern { pattern, value, .. } => {
            collect_tasks_in_expr(owner, module, value, available, types, output);
            add_pattern_types(pattern, value.ty, available, types);
        }
        CoreExprKind::Block(values) => {
            let mut scoped = available.clone();
            for value in values {
                collect_tasks_in_expr(owner, module, value, &mut scoped, types, output);
            }
        }
        CoreExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_tasks_in_expr(owner, module, condition, available, types, output);
            collect_tasks_in_expr(
                owner,
                module,
                then_branch,
                &mut available.clone(),
                types,
                output,
            );
            collect_tasks_in_expr(
                owner,
                module,
                else_branch,
                &mut available.clone(),
                types,
                output,
            );
        }
        CoreExprKind::Match { value, arms } => {
            collect_tasks_in_expr(owner, module, value, available, types, output);
            for arm in arms {
                let mut scoped = available.clone();
                for name in pattern_binding_names(&arm.pattern) {
                    // Task-capture collection only queries these names, so a
                    // binding's exact type is not needed here.
                    scoped.insert(name, value.ty);
                }
                collect_tasks_in_expr(owner, module, &arm.value, &mut scoped, types, output);
            }
        }
        CoreExprKind::Closure { .. } => {}
        _ => visit_core_children(expression, &mut |child| {
            collect_tasks_in_expr(owner, module, child, &mut available.clone(), types, output)
        }),
    }
}

fn contains_non_normal_effect(expression: &CoreExpr, types: &CheckedTypes) -> bool {
    if let CoreExprKind::Call {
        callee,
        effect_operation,
        ..
    } = &expression.kind
    {
        if let Some(operation) = effect_operation {
            if types.effects().operation_mode(*operation) != Some(crate::sema::EffectMode::Normal)
                || types
                    .effects()
                    .operation_info(*operation)
                    .is_some_and(|info| info.suspends)
            {
                return true;
            }
        } else if call_has_non_normal_effect(callee, types) {
            return true;
        }
    }
    let mut found = false;
    visit_core_children(expression, &mut |child| {
        found |= contains_non_normal_effect(child, types);
    });
    found
}

fn call_has_non_normal_effect(callee: &CoreExpr, types: &CheckedTypes) -> bool {
    let effects = match &callee.kind {
        CoreExprKind::Name(name) => types.function_effects(name),
        CoreExprKind::Field {
            value,
            access: FieldAccess::Name(method),
        } => match value.ty {
            Type::Struct(id) => types.function_effects(&format!("struct_{id}_{method}")),
            Type::Class(id) => types.function_effects(&format!("class_{id}_{method}")),
            _ => None,
        },
        _ => None,
    }
    .or_else(|| match callee.ty {
        Type::Function(id) => Some(&types.function_type(id).effects),
        _ => None,
    });

    effects.is_some_and(|effects| {
        effects.iter().any(|operation| {
            types.effects().operation_mode(operation) != Some(crate::sema::EffectMode::Normal)
                || types
                    .effects()
                    .operation_info(operation)
                    .is_some_and(|info| info.suspends)
        })
    })
}

pub(crate) fn task_captures(expression: &CoreExpr) -> Vec<(String, Type)> {
    fn visit(
        expression: &CoreExpr,
        bound: &mut Vec<HashSet<String>>,
        captures: &mut BTreeMap<String, Type>,
    ) {
        match &expression.kind {
            CoreExprKind::For {
                index,
                item,
                iterable,
                body,
                limit,
            } => {
                visit(iterable, bound, captures);
                if let Some(limit) = limit {
                    visit(limit, bound, captures);
                }
                bound.push(std::iter::once(item.clone()).chain(index.clone()).collect());
                visit(body, bound, captures);
                bound.pop();
            }
            CoreExprKind::ForWorker {
                index,
                item,
                state,
                body,
                ..
            } => {
                captures.entry(state.clone()).or_insert(Type::Batch);
                bound.push(std::iter::once(item.clone()).chain(index.clone()).collect());
                visit(body, bound, captures);
                bound.pop();
            }
            CoreExprKind::Name(name) => {
                if !bound.iter().rev().any(|scope| scope.contains(name)) {
                    captures.entry(name.clone()).or_insert(expression.ty);
                }
            }
            CoreExprKind::Let { name, value, .. } => {
                visit(value, bound, captures);
                bound
                    .last_mut()
                    .expect("task capture scope")
                    .insert(name.clone());
            }
            CoreExprKind::LetPattern { pattern, value, .. } => {
                visit(value, bound, captures);
                bound
                    .last_mut()
                    .unwrap()
                    .extend(pattern_binding_names(pattern));
            }
            CoreExprKind::Block(values) => {
                bound.push(HashSet::new());
                for value in values {
                    visit(value, bound, captures);
                }
                bound.pop();
            }
            CoreExprKind::Closure {
                captures: values, ..
            } => {
                for (name, ty) in values {
                    if !bound.iter().rev().any(|scope| scope.contains(name)) {
                        captures.entry(name.clone()).or_insert(*ty);
                    }
                }
            }
            CoreExprKind::Match { value, arms } => {
                visit(value, bound, captures);
                for arm in arms {
                    bound.push(pattern_bindings(&arm.pattern));
                    visit(&arm.value, bound, captures);
                    bound.pop();
                }
            }
            CoreExprKind::Do { body, handlers } => {
                visit(body, bound, captures);
                for handler in handlers {
                    bound.push(
                        handler
                            .parameters
                            .iter()
                            .flat_map(pattern_binding_names)
                            .collect(),
                    );
                    visit(&handler.value, bound, captures);
                    bound.pop();
                }
            }
            CoreExprKind::When {
                cowns,
                bindings,
                until,
                body,
            } => {
                for cown in cowns {
                    visit(cown, bound, captures);
                }
                bound.push(
                    bindings
                        .as_ref()
                        .map(|patterns| patterns.iter().flat_map(pattern_binding_names).collect())
                        .unwrap_or_default(),
                );
                if let Some(until) = until {
                    visit(until, bound, captures);
                }
                visit(body, bound, captures);
                bound.pop();
            }
            _ => visit_core_children(expression, &mut |child| visit(child, bound, captures)),
        }
    }

    let mut captures = BTreeMap::new();
    visit(expression, &mut vec![HashSet::new()], &mut captures);
    captures.into_iter().collect()
}

fn add_pattern_types(
    pattern: &CorePattern,
    ty: Type,
    available: &mut BTreeMap<String, Type>,
    types: &CheckedTypes,
) {
    match pattern {
        CorePattern::Binding { name, .. } => {
            available.insert(name.clone(), ty);
        }
        CorePattern::Tuple { elements, .. } => {
            if let Type::Tuple(id) = ty {
                for (pattern, ty) in elements.iter().zip(types.tuple_elements(id)) {
                    add_pattern_types(pattern, *ty, available, types);
                }
            }
        }
        _ => {}
    }
}

fn pattern_bindings(pattern: &CorePattern) -> HashSet<String> {
    pattern_binding_names(pattern).into_iter().collect()
}

fn pattern_binding_names(pattern: &CorePattern) -> Vec<String> {
    match pattern {
        CorePattern::Binding { name, .. } => vec![name.clone()],
        CorePattern::EnumVariant { fields, .. } => fields
            .iter()
            .flat_map(|field| pattern_binding_names(&field.pattern))
            .collect(),
        CorePattern::Tuple { elements, .. } => {
            elements.iter().flat_map(pattern_binding_names).collect()
        }
        CorePattern::Wildcard { .. } => Vec::new(),
    }
}

pub(super) fn visit_core_children(expression: &CoreExpr, visitor: &mut impl FnMut(&CoreExpr)) {
    match &expression.kind {
        CoreExprKind::Let { value, .. } | CoreExprKind::LetPattern { value, .. } => visitor(value),
        CoreExprKind::Unary { expression, .. }
        | CoreExprKind::Unwrap {
            value: expression, ..
        }
        | CoreExprKind::Cast {
            value: expression, ..
        }
        | CoreExprKind::Branch(expression)
        | CoreExprKind::Region(expression) => visitor(expression),
        CoreExprKind::Binary { left, right, .. } => {
            visitor(left);
            visitor(right);
        }
        CoreExprKind::Call {
            callee, arguments, ..
        } => {
            visitor(callee);
            for argument in arguments {
                visitor(&argument.value);
            }
        }
        CoreExprKind::Tuple(values)
        | CoreExprKind::Parallel(values)
        | CoreExprKind::Race(values)
        | CoreExprKind::Block(values) => {
            for value in values {
                visitor(value);
            }
        }
        CoreExprKind::CollectionLiteral(literal) => match literal {
            CoreCollectionLiteral::List(values)
            | CoreCollectionLiteral::MutList(values)
            | CoreCollectionLiteral::Set(values)
            | CoreCollectionLiteral::MutSet(values) => {
                for value in values {
                    visitor(value);
                }
            }
            CoreCollectionLiteral::Map(entries) | CoreCollectionLiteral::MutMap(entries) => {
                for entry in entries {
                    visitor(&entry.key);
                    visitor(&entry.value);
                }
            }
        },
        CoreExprKind::StructInit { fields, .. } => {
            for (_, value) in fields {
                visitor(value);
            }
        }
        CoreExprKind::Field { value, .. } => visitor(value),
        CoreExprKind::Assign { target, value }
        | CoreExprKind::CompoundAssign { target, value, .. } => {
            visitor(target);
            visitor(value);
        }
        CoreExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            visitor(condition);
            visitor(then_branch);
            visitor(else_branch);
        }
        CoreExprKind::Match { value, arms } => {
            visitor(value);
            for arm in arms {
                visitor(&arm.value);
            }
        }
        CoreExprKind::For {
            iterable,
            body,
            limit,
            ..
        } => {
            visitor(iterable);
            if let Some(limit) = limit {
                visitor(limit);
            }
            visitor(body);
        }
        CoreExprKind::ForWorker { body, .. } => visitor(body),
        CoreExprKind::While { condition, body } => {
            visitor(condition);
            visitor(body);
        }
        CoreExprKind::Loop { body } => visitor(body),
        CoreExprKind::Break { value } => {
            if let Some(value) = value {
                visitor(value);
            }
        }
        CoreExprKind::Abort { value } => visitor(value),
        CoreExprKind::Do { body, handlers } => {
            visitor(body);
            for handler in handlers {
                visitor(&handler.value);
            }
        }
        CoreExprKind::When {
            cowns, until, body, ..
        } => {
            if let Some(until) = until {
                visitor(until);
            }
            for cown in cowns {
                visitor(cown);
            }
            visitor(body);
        }
        CoreExprKind::Closure { .. }
        | CoreExprKind::Unit
        | CoreExprKind::Integer(_)
        | CoreExprKind::Float(_)
        | CoreExprKind::Duration(_)
        | CoreExprKind::String(_)
        | CoreExprKind::Bytes(_)
        | CoreExprKind::Boolean(_)
        | CoreExprKind::Name(_)
        | CoreExprKind::ExternalSymbol(_)
        | CoreExprKind::Continue => {}
        CoreExprKind::InterpolatedString(parts) => {
            for (_, expression) in parts {
                if let Some(expression) = expression {
                    visitor(expression);
                }
            }
        }
    }
}

fn collect_closures(expression: &CoreExpr, output: &mut Vec<ClosureFunction>) {
    if let CoreExprKind::Closure {
        function_name,
        parameters,
        captures,
        capture_bindings,
        return_type,
        effects,
        body,
        ..
    } = &expression.kind
    {
        output.push((
            function_name.clone(),
            captures.clone(),
            capture_bindings.clone(),
            parameters.clone(),
            *return_type,
            effects.clone(),
            (**body).clone(),
        ));
        collect_closures(body, output);
    }
    if !matches!(expression.kind, CoreExprKind::Closure { .. }) {
        visit_core_children(expression, &mut |child| collect_closures(child, output));
    }
}

pub(super) struct GenericInstanceCollector<'a> {
    pub(super) types: &'a CheckedTypes,
    pub(super) substitutions: &'a [Type],
    pub(super) instances: Vec<crate::sema::GenericCallInstance>,
}

impl ExprVisitor for GenericInstanceCollector<'_> {
    type Output = ();

    fn visit_expr(&mut self, expression: &Expr) {
        if let Some(instance) = self.types.generic_call(expression) {
            if self
                .types
                .imported_templates
                .contains_key(&instance.function)
            {
                walk_expr(self, expression);
                return;
            }
            let arguments = instance
                .arguments
                .iter()
                .map(|argument| self.types.substitute(*argument, self.substitutions))
                .collect::<Option<Vec<_>>>();
            if let Some(arguments) = arguments.filter(|arguments| {
                arguments
                    .iter()
                    .all(|argument| !self.types.contains_type_parameter(*argument))
            }) {
                self.instances.push(crate::sema::GenericCallInstance {
                    function: instance.function.clone(),
                    arguments,
                    compile_time_arguments: 0,
                });
            }
        }
        walk_expr(self, expression);
    }

    fn default_output(&self) {}
}

/// Branches attach to their nearest iteration scope. Other structured task
/// expressions and function values establish their own lifetime boundary.
pub(crate) fn iteration_has_branches(expression: &CoreExpr) -> bool {
    match &expression.kind {
        CoreExprKind::Branch(_) => true,
        CoreExprKind::Closure { .. }
        | CoreExprKind::Parallel(_)
        | CoreExprKind::Race(_)
        | CoreExprKind::Region(_)
        | CoreExprKind::For { .. } => false,
        _ => {
            let mut found = false;
            visit_core_children(expression, &mut |child| {
                found |= iteration_has_branches(child)
            });
            found
        }
    }
}

pub(super) fn prepare_iteration_types(expression: &CoreExpr, types: &mut CheckedTypes) {
    if let CoreExprKind::For { iterable, .. } = &expression.kind {
        let input = types.cursor_type(iterable.id).unwrap_or(iterable.ty);
        types.ensure_iteration_types(input);
    }
    visit_core_children(expression, &mut |child| {
        prepare_iteration_types(child, types)
    });
}
