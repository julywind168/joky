use super::*;

#[derive(Debug, Default)]
struct PartialAggregateMove {
    variant: Option<usize>,
    fields: HashSet<usize>,
}

// Aggregate decomposition is atomic within one basic block. This keeps partial
// state out of CFG joins and gives codegen a complete Project/Deinit protocol.
pub(super) fn verify_aggregate_move_protocol(
    function: &MirFunction,
    types: &TypeTable,
    reachable: &HashSet<MirBlockId>,
) -> Result<(), Diagnostic> {
    for block in function
        .blocks
        .iter()
        .filter(|block| reachable.contains(&block.id))
    {
        let mut partial = HashMap::<MirValueId, PartialAggregateMove>::new();
        for statement in &block.statements {
            match statement {
                MirStatement::Project {
                    destination,
                    base,
                    access: MirFieldAccess::Index(index),
                } => {
                    let base_type = function.value_types[base.0];
                    let field_type = match base_type {
                        Type::Tuple(id) => types.tuple_elements(id)[*index],
                        Type::Struct(id) => types.struct_fields(id)[*index].1,
                        Type::Class(id) => types.class_fields(id)[*index].1,
                        Type::Dyn(id) => {
                            Type::Function(types.dynamic_types[id].methods[*index].signature)
                        }
                        _ => unreachable!("field projection type checked before move protocol"),
                    };
                    let moves_field = types.is_owned(field_type)
                        && function.value_ownership[destination.0] == MirOwnership::Owned;
                    if moves_field {
                        if !matches!(base_type, Type::Tuple(_) | Type::Struct(_)) {
                            return Err(Diagnostic::codegen(format!(
                                "MIR function '{}' moves a field out of a class",
                                function.name
                            )));
                        }
                        let state = partial.entry(*base).or_default();
                        if !state.fields.insert(*index) {
                            return Err(duplicate_aggregate_move(function, *base, *index));
                        }
                    } else if partial.contains_key(base) && types.is_owned(field_type) {
                        return Err(partial_aggregate_use(function, *base));
                    }
                }
                MirStatement::EnumProject {
                    destination,
                    value,
                    variant,
                    field,
                } => {
                    let field_type = match function.value_types[value.0] {
                        Type::Enum(enum_id) => {
                            types.enum_variants(enum_id)[*variant].fields[*field].1
                        }
                        Type::Option(option_id) if *variant == 0 && *field == 0 => {
                            types.option_type(option_id)
                        }
                        Type::Result(result_id) if *field == 0 && *variant < 2 => {
                            let (ok, err) = types.result_types(result_id);
                            if *variant == 0 {
                                ok
                            } else {
                                err
                            }
                        }
                        _ => unreachable!("enum projection type checked before move protocol"),
                    };
                    let moves_field = types.is_owned(field_type)
                        && function.value_ownership[destination.0] == MirOwnership::Owned;
                    if moves_field {
                        let state = partial
                            .entry(*value)
                            .or_insert_with(|| PartialAggregateMove {
                                variant: Some(*variant),
                                fields: HashSet::new(),
                            });
                        if state.variant != Some(*variant) {
                            return Err(mixed_enum_variant_move(function, *value));
                        }
                        if !state.fields.insert(*field) {
                            return Err(duplicate_aggregate_move(function, *value, *field));
                        }
                    } else if let Some(state) = partial.get(value) {
                        if state.variant != Some(*variant) || types.is_owned(field_type) {
                            return Err(partial_aggregate_use(function, *value));
                        }
                    }
                }
                MirStatement::Deinit { value, variant, .. } => {
                    let value_type = function.value_types[value.0];
                    let expected = owned_deinit_fields(value_type, *variant, types);
                    let moved = partial.remove(value);
                    if let Some(state) = &moved {
                        if state.variant != *variant {
                            return Err(mixed_enum_variant_move(function, *value));
                        }
                    }
                    let moved_fields = moved.map(|state| state.fields).unwrap_or_default();
                    if moved_fields != expected {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' deinitializes aggregate %{} before all owned fields are moved",
                            function.name, value.0
                        )));
                    }
                }
                _ => {
                    if let Some(value) = statement_operands(statement)
                        .into_iter()
                        .find(|value| partial.contains_key(value))
                    {
                        return Err(partial_aggregate_use(function, value));
                    }
                }
            }
        }
        if let Some(value) = partial.keys().next() {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' carries partially moved aggregate %{} across a basic block edge",
                function.name, value.0
            )));
        }
    }
    Ok(())
}

fn owned_deinit_fields(
    value_type: Type,
    variant: Option<usize>,
    types: &TypeTable,
) -> HashSet<usize> {
    match (value_type, variant) {
        (Type::Tuple(id), None) => types
            .tuple_elements(id)
            .iter()
            .enumerate()
            .filter_map(|(index, field)| types.is_owned(*field).then_some(index))
            .collect(),
        (Type::Struct(id), None) => types
            .struct_fields(id)
            .iter()
            .enumerate()
            .filter_map(|(index, (_, field))| types.is_owned(*field).then_some(index))
            .collect(),
        (Type::Enum(id), Some(variant)) => types.enum_variants(id)[variant]
            .fields
            .iter()
            .enumerate()
            .filter_map(|(index, (_, field))| types.is_owned(*field).then_some(index))
            .collect(),
        (Type::Option(id), Some(0)) => types
            .is_owned(types.option_type(id))
            .then_some(0)
            .into_iter()
            .collect(),
        (Type::Option(_), Some(1)) => HashSet::new(),
        (Type::Result(id), Some(variant)) if variant < 2 => {
            let (ok, err) = types.result_types(id);
            types
                .is_owned(if variant == 0 { ok } else { err })
                .then_some(0)
                .into_iter()
                .collect()
        }
        _ => unreachable!("deinit metadata checked before move protocol"),
    }
}

fn duplicate_aggregate_move(function: &MirFunction, value: MirValueId, field: usize) -> Diagnostic {
    Diagnostic::codegen(format!(
        "MIR function '{}' moves field {} out of aggregate %{} more than once",
        function.name, field, value.0
    ))
}

fn mixed_enum_variant_move(function: &MirFunction, value: MirValueId) -> Diagnostic {
    Diagnostic::codegen(format!(
        "MIR function '{}' mixes enum variants while moving fields out of %{}",
        function.name, value.0
    ))
}

fn partial_aggregate_use(function: &MirFunction, value: MirValueId) -> Diagnostic {
    Diagnostic::codegen(format!(
        "MIR function '{}' uses partially moved aggregate %{}",
        function.name, value.0
    ))
}

pub(super) fn verify_ownership_flow(
    function: &MirFunction,
    reverse_postorder: &[MirBlockId],
    predecessors: &[Vec<(MirBlockId, Vec<MirValueId>)>],
) -> Result<(), Diagnostic> {
    let mut incoming = vec![None; function.blocks.len()];
    let mut outgoing: Vec<Option<IndexSet<MirValueId>>> = vec![None; function.blocks.len()];
    incoming[function.entry.0] = Some(IndexSet::empty(function.value_types.len()));

    // Visit predecessors before successors so forward facts propagate through
    // an acyclic graph in one sweep, independent of HashSet iteration order.
    // Consumed values only grow through the CFG, so a finite union fixpoint is
    // sufficient even in the presence of loops.
    loop {
        let mut changed = false;
        for block_id in reverse_postorder {
            if *block_id != function.entry {
                let mut state = IndexSet::empty(function.value_types.len());
                let mut has_predecessor_state = false;
                for (predecessor, _) in &predecessors[block_id.0] {
                    if let Some(previous) = &outgoing[predecessor.0] {
                        state.union_with(previous);
                        has_predecessor_state = true;
                    }
                }
                if !has_predecessor_state {
                    continue;
                }
                if incoming[block_id.0].as_ref() != Some(&state) {
                    incoming[block_id.0] = Some(state);
                    changed = true;
                }
            }
            let Some(mut state) = incoming[block_id.0].clone() else {
                continue;
            };
            for statement in &function.blocks[block_id.0].statements {
                // SSA destinations are fresh values on every execution of a
                // block. A loop back-edge may carry the same numeric value id
                // from an earlier iteration, so clear a destination's stale
                // consumed bit before processing its new definition.
                if let Some(destination) = statement_destination(statement) {
                    state.remove(&destination);
                }
                for value in consumed_statement_values(function, statement) {
                    state.insert(value);
                }
            }
            if outgoing[block_id.0].as_ref() != Some(&state) {
                outgoing[block_id.0] = Some(state);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for block_id in reverse_postorder {
        let mut state = incoming[block_id.0]
            .clone()
            .unwrap_or_else(|| IndexSet::empty(function.value_types.len()));
        let block = &function.blocks[block_id.0];
        for statement in &block.statements {
            if let Some(destination) = statement_destination(statement) {
                state.remove(&destination);
            }
            if let MirStatement::Phi { incoming, .. } = statement {
                for (predecessor, value) in incoming {
                    if outgoing[predecessor.0]
                        .as_ref()
                        .is_some_and(|consumed| consumed.contains(value))
                    {
                        return Err(ownership_use_error(function, *value, "Phi"));
                    }
                }
                continue;
            }
            for value in statement_operands(statement) {
                if state.contains(&value) {
                    return Err(ownership_use_error(function, value, "statement"));
                }
            }
            for value in consumed_statement_values(function, statement) {
                state.insert(value);
            }
        }
        match block
            .terminator
            .as_ref()
            .expect("reachable block is terminated")
        {
            MirTerminator::Branch { condition, .. } => {
                if state.contains(condition) {
                    return Err(ownership_use_error(function, *condition, "branch"));
                }
            }
            MirTerminator::Goto { arguments, .. } => {
                if let Some(value) = arguments.iter().find(|value| state.contains(value)) {
                    return Err(ownership_use_error(function, *value, "jump"));
                }
            }
            MirTerminator::Return(Some(value)) => {
                if state.contains(value) {
                    return Err(ownership_use_error(function, *value, "return"));
                }
            }
            MirTerminator::Return(None) | MirTerminator::Unreachable => {}
        }
    }
    Ok(())
}

pub(super) fn verify_local_ownership_flow(
    function: &MirFunction,
    types: &TypeTable,
    reverse_postorder: &[MirBlockId],
    predecessors: &[Vec<(MirBlockId, Vec<MirValueId>)>],
) -> Result<(), Diagnostic> {
    if function
        .locals
        .iter()
        .enumerate()
        .any(|(index, local)| local.id.0 != index)
        || function
            .parameters
            .iter()
            .any(|parameter| parameter.local.0 >= function.locals.len())
        || function
            .receiver_local
            .is_some_and(|local| local.0 >= function.locals.len())
    {
        return Err(Diagnostic::codegen("MIR function has an invalid local id"));
    }
    let mut tracked = IndexSet::empty(function.locals.len());
    tracked.extend(
        function
            .locals
            .iter()
            .filter(|local| types.is_owned(local.ty))
            .map(|local| local.id),
    );
    let mut entry_state = IndexSet::empty(function.locals.len());
    entry_state.extend(
        function
            .parameters
            .iter()
            .filter(|parameter| types.is_owned(parameter.ty))
            .map(|parameter| parameter.local),
    );
    if let Some(receiver) = function.receiver_local {
        if types.is_owned(function.locals[receiver.0].ty) {
            entry_state.insert(receiver);
        }
    }

    let mut incoming = vec![tracked.clone(); function.blocks.len()];
    let mut outgoing = vec![tracked.clone(); function.blocks.len()];
    incoming[function.entry.0] = entry_state;
    loop {
        let mut changed = false;
        for block_id in reverse_postorder {
            if *block_id != function.entry {
                let mut next = tracked.clone();
                for (predecessor, _) in &predecessors[block_id.0] {
                    next.intersect_with(&outgoing[predecessor.0]);
                }
                if next != incoming[block_id.0] {
                    incoming[block_id.0] = next;
                    changed = true;
                }
            }
            let mut next = incoming[block_id.0].clone();
            for statement in &function.blocks[block_id.0].statements {
                match statement {
                    MirStatement::Bind {
                        local,
                        value: Some(_),
                        ..
                    } if tracked.contains(local) => {
                        next.insert(*local);
                    }
                    MirStatement::TakeLocal { local, .. }
                    | MirStatement::DropLocal { local, .. } => {
                        next.remove(local);
                    }
                    _ => {}
                }
            }
            if next != outgoing[block_id.0] {
                outgoing[block_id.0] = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for block_id in reverse_postorder {
        if *block_id != function.entry {
            let predecessor_states = predecessors[block_id.0]
                .iter()
                .map(|(predecessor, _)| &outgoing[predecessor.0])
                .collect::<Vec<_>>();
            if predecessor_states.windows(2).any(|states| {
                // Branch-local borrows carry no cleanup obligation. Their
                // later uses still require initialization on every path.
                states[0]
                    .symmetric_difference(states[1])
                    .any(|local| function.locals[local.0].ownership != MirOwnership::Borrowed)
            }) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' merges inconsistent local ownership",
                    function.name
                )));
            }
        }

        let mut state = incoming[block_id.0].clone();
        for statement in &function.blocks[block_id.0].statements {
            match statement {
                MirStatement::Read { destination, local }
                    if matches!(
                        function.value_types.get(destination.0),
                        Some(Type::Class(_))
                    ) && function.value_ownership.get(destination.0)
                        == Some(&MirOwnership::Borrowed) =>
                {
                    if !state.contains(local) {
                        return Err(local_use_after_move(function, *local, "read"));
                    }
                }
                MirStatement::BorrowLocal { local, .. } => {
                    if !state.contains(local) {
                        return Err(local_use_after_move(function, *local, "borrow"));
                    }
                }
                MirStatement::TakeLocal { local, .. } => {
                    if tracked.contains(local) && !state.remove(local) {
                        return Err(local_use_after_move(function, *local, "take"));
                    }
                }
                MirStatement::DropLocal { local, .. } => {
                    if tracked.contains(local) && !state.remove(local) {
                        return Err(local_use_after_move(function, *local, "drop"));
                    }
                }
                MirStatement::Bind {
                    local,
                    value: Some(_),
                    ..
                } if tracked.contains(local) && !state.insert(*local) => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' initializes owned local '{}' twice",
                        function.name, function.locals[local.0].name
                    )));
                }
                _ => {}
            }
        }
        if matches!(
            function.blocks[block_id.0].terminator,
            Some(MirTerminator::Return(_))
        ) && state.iter().any(|local| {
            matches!(
                function.locals[local.0].ownership,
                MirOwnership::Owned | MirOwnership::Shared
            )
        }) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' returns with a live owned local",
                function.name
            )));
        }
    }
    Ok(())
}
/// Check the lexical lifetime of Cown leases independently from ordinary
/// ownership. A Cown capability is consumed by acquire, while the duplicated
/// capability used by `CownPayload`/`CownRelease` identifies the same lease.
/// The flow is deliberately strict at CFG joins: a lease must be either live
/// on every incoming edge or dead on every incoming edge.
pub(super) fn verify_cown_lease_lifetimes(
    function: &MirFunction,
    reachable: &HashSet<MirBlockId>,
) -> Result<(), Diagnostic> {
    let mut aliases = HashMap::<MirValueId, MirValueId>::new();
    for (value, ty) in function.value_types.iter().enumerate() {
        if matches!(ty, Type::Cown(_)) {
            aliases.insert(MirValueId(value), MirValueId(value));
        }
    }
    // Dup/Move chains are emitted before the acquire in `when`; resolve them
    // once up front so release capabilities map back to one lease identity.
    let mut changed = true;
    while changed {
        changed = false;
        for block in &function.blocks {
            for statement in &block.statements {
                let (destination, source) = match statement {
                    MirStatement::Dup { destination, value }
                    | MirStatement::Move { destination, value }
                        if matches!(function.value_types.get(value.0), Some(Type::Cown(_))) =>
                    {
                        (*destination, *value)
                    }
                    _ => continue,
                };
                if let Some(root) = aliases.get(&source).copied() {
                    changed |= aliases.insert(destination, root) != Some(root);
                }
            }
        }
    }

    let mut incoming = vec![None::<HashSet<MirValueId>>; function.blocks.len()];
    let mut outgoing = vec![None::<HashSet<MirValueId>>; function.blocks.len()];
    incoming[function.entry.0] = Some(HashSet::new());
    let mut work = VecDeque::from([function.entry]);
    while let Some(block_id) = work.pop_front() {
        if !reachable.contains(&block_id) {
            continue;
        }
        let mut active = incoming[block_id.0].clone().unwrap_or_default();
        for statement in &function.blocks[block_id.0].statements {
            let root_of = |value: MirValueId| aliases.get(&value).copied().unwrap_or(value);
            match statement {
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownAcquire(payload),
                    arguments,
                    ..
                } => {
                    let Some(argument) = arguments.first() else {
                        continue;
                    };
                    let root = root_of(argument.value);
                    if !matches!(
                        function.value_types.get(argument.value.0),
                        Some(Type::Cown(_))
                    ) || !active.insert(root)
                    {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' acquires an invalid or already leased Cown",
                            function.name
                        )));
                    }
                    let _ = payload;
                }
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownAcquireMany,
                    arguments,
                    ..
                }
                | MirStatement::CownAcquire { arguments, .. } => {
                    let roots = arguments
                        .iter()
                        .map(|argument| root_of(argument.value))
                        .collect::<Vec<_>>();
                    if matches!(
                        statement,
                        MirStatement::CownAcquire {
                            wait_for_change: true,
                            ..
                        }
                    ) {
                        if roots.len() != active.len()
                            || roots.iter().copied().collect::<HashSet<_>>() != active
                        {
                            return Err(Diagnostic::codegen(
                                "Cown condition wait must release the entire active lease set",
                            ));
                        }
                        continue;
                    }
                    if (matches!(statement, MirStatement::CownAcquire { .. }) && !active.is_empty())
                        || arguments.iter().any(|argument| {
                            !matches!(
                                function.value_types.get(argument.value.0),
                                Some(Type::Cown(_))
                            )
                        })
                        || roots.iter().any(|root| active.contains(root))
                        || roots.windows(2).any(|pair| pair[0] == pair[1])
                    {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' acquires an invalid, duplicate, or already leased Cown set",
                            function.name
                        )));
                    }
                    active.extend(roots);
                }
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownPayload(_),
                    arguments,
                    ..
                } => {
                    let Some(argument) = arguments.first() else {
                        continue;
                    };
                    if !active.contains(&root_of(argument.value)) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' projects a Cown payload without an active lease",
                            function.name
                        )));
                    }
                }
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownRelease,
                    arguments,
                    ..
                } => {
                    let Some(argument) = arguments.first() else {
                        continue;
                    };
                    if !active.remove(&root_of(argument.value)) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' releases a Cown without an active lease",
                            function.name
                        )));
                    }
                }
                MirStatement::Suspend { .. }
                | MirStatement::TaskWait { .. }
                | MirStatement::Call {
                    continuation: Some(_),
                    ..
                }
                | MirStatement::MethodCall {
                    continuation: Some(_),
                    ..
                }
                | MirStatement::CallIndirect {
                    continuation: Some(_),
                    ..
                } if !active.is_empty() => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' carries a Cown lease across suspend",
                        function.name
                    )));
                }
                MirStatement::TaskCreate { .. } if !active.is_empty() => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' starts a task while holding a Cown lease",
                        function.name
                    )));
                }
                MirStatement::FunctionValue { captures, .. }
                    if !active.is_empty()
                        && captures.iter().any(|value| {
                            aliases.get(value).is_some_and(|root| active.contains(root))
                        }) =>
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' captures a Cown capability inside a lease",
                        function.name
                    )));
                }
                _ => {}
            }
        }
        if outgoing[block_id.0].as_ref() != Some(&active) {
            outgoing[block_id.0] = Some(active.clone());
            let terminator = function.blocks[block_id.0]
                .terminator
                .as_ref()
                .expect("reachable block is terminated");
            for target in terminator_targets(terminator) {
                if let Some(previous) = &incoming[target.0] {
                    if previous != &active {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' merges inconsistent Cown lease state at bb{}",
                            function.name, target.0
                        )));
                    }
                } else {
                    incoming[target.0] = Some(active.clone());
                    work.push_back(target);
                }
            }
        }
    }
    for block_id in reachable {
        let Some(terminator) = function.blocks[block_id.0].terminator.as_ref() else {
            continue;
        };
        if matches!(terminator, MirTerminator::Return(_))
            && outgoing[block_id.0]
                .as_ref()
                .is_some_and(|state| !state.is_empty())
        {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' returns while holding a Cown lease",
                function.name
            )));
        }
    }
    Ok(())
}
