//! Resolve local bindings at CFG joins before lowering them to backend environments.

use std::collections::{HashSet, VecDeque};

use super::*;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Definition {
    Uninitialized,
    Entry,
    Bind(MirValueId),
}

/// Consumers only distinguish zero, one, or multiple reaching definitions,
/// and whether an uninitialized path is among them. Once there are multiple
/// definitions, their identities are unnecessary: transfer replaces the whole
/// state and joins only add possibilities. Keep this finite lattice inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReachingDefinitions {
    Empty,
    One(Definition),
    Many { uninitialized: bool },
}

impl ReachingDefinitions {
    fn may_be_uninitialized(self) -> bool {
        matches!(
            self,
            Self::One(Definition::Uninitialized)
                | Self::Many {
                    uninitialized: true
                }
        )
    }

    fn is_empty(self) -> bool {
        self == Self::Empty
    }

    fn has_multiple(self) -> bool {
        matches!(self, Self::Many { .. })
    }

    fn has_initialized(self) -> bool {
        !matches!(self, Self::Empty | Self::One(Definition::Uninitialized))
    }

    fn union_with(&mut self, other: Self) {
        if other.is_empty() || *self == other {
            return;
        }
        if self.is_empty() {
            *self = other;
        } else {
            *self = Self::Many {
                uninitialized: self.may_be_uninitialized() || other.may_be_uninitialized(),
            };
        }
    }
}

type State = Vec<ReachingDefinitions>;

struct Flow {
    /// Breadth-first from the entry; fixes which join `normalize` merges next.
    order: Vec<usize>,
    rpo: Vec<usize>,
    predecessors: Vec<Vec<usize>>,
    incoming: Vec<State>,
    entry_locals: HashSet<MirLocalId>,
}

/// For each reachable block, the locals bound in one of its strict
/// dominators. `normalize` reruns reaching definitions after every merge but
/// never needs these, so they stay out of `analyze`.
struct Declarations {
    declared: Vec<Vec<bool>>,
}

impl Declarations {
    fn in_scope(&self, local: usize, index: usize) -> bool {
        self.declared[index][local]
    }
}

fn successors(block: &MirBlock) -> Vec<usize> {
    match block.terminator.as_ref() {
        Some(MirTerminator::Goto { target, .. }) => vec![target.0],
        Some(MirTerminator::Branch {
            then_block,
            else_block,
            ..
        }) => {
            if then_block == else_block {
                vec![then_block.0]
            } else {
                vec![then_block.0, else_block.0]
            }
        }
        _ => Vec::new(),
    }
}

fn transfer(state: &mut State, statement: &MirStatement) {
    let (local, definition) = match statement {
        MirStatement::Bind {
            local, destination, ..
        } => (*local, Definition::Bind(*destination)),
        MirStatement::TakeLocal { local, .. } | MirStatement::DropLocal { local, .. } => {
            (*local, Definition::Uninitialized)
        }
        _ => return,
    };
    state[local.0] = ReachingDefinitions::One(definition);
}

fn analyze(function: &MirFunction) -> Flow {
    let mut order = Vec::new();
    let mut reachable = HashSet::new();
    let mut pending = VecDeque::from([function.entry.0]);
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    while let Some(index) = pending.pop_front() {
        if !reachable.insert(index) {
            continue;
        }
        order.push(index);
        for next in successors(&function.blocks[index]) {
            predecessors[next].push(index);
            pending.push_back(next);
        }
    }
    let empty = vec![ReachingDefinitions::Empty; function.locals.len()];
    let mut incoming = vec![empty.clone(); function.blocks.len()];
    let mut outgoing = incoming.clone();
    let mut entry =
        vec![ReachingDefinitions::One(Definition::Uninitialized); function.locals.len()];
    let entry_locals: HashSet<_> = function
        .parameters
        .iter()
        .map(|p| p.local)
        .chain(function.receiver_local)
        .collect();
    for &local in &entry_locals {
        entry[local.0] = ReachingDefinitions::One(Definition::Entry);
    }
    let rpo = reverse_postorder(function);
    loop {
        let mut changed = false;
        for &index in &rpo {
            let mut state = if index == function.entry.0 {
                entry.clone()
            } else {
                empty.clone()
            };
            for &predecessor in &predecessors[index] {
                for (local, definitions) in state.iter_mut().enumerate() {
                    definitions.union_with(outgoing[predecessor][local]);
                }
            }
            incoming[index] = state.clone();
            for statement in &function.blocks[index].statements {
                transfer(&mut state, statement);
            }
            if state != outgoing[index] {
                outgoing[index] = state;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Flow {
        order,
        rpo,
        predecessors,
        incoming,
        entry_locals,
    }
}

fn reverse_postorder(function: &MirFunction) -> Vec<usize> {
    let entry = function.entry.0;
    let mut visited = vec![false; function.blocks.len()];
    visited[entry] = true;
    let mut postorder = Vec::new();
    let mut stack = vec![(entry, successors(&function.blocks[entry]))];
    while let Some((index, pending)) = stack.last_mut() {
        match pending.pop() {
            Some(next) if !visited[next] => {
                visited[next] = true;
                stack.push((next, successors(&function.blocks[next])));
            }
            Some(_) => {}
            None => {
                postorder.push(*index);
                stack.pop();
            }
        }
    }
    postorder.reverse();
    postorder
}

/// Immediate dominators (Cooper, Harvey and Kennedy), with the entry as its
/// own parent. Unreachable blocks have none.
fn immediate_dominators(function: &MirFunction, flow: &Flow) -> Vec<usize> {
    let order = &flow.rpo;
    let mut rank = vec![usize::MAX; function.blocks.len()];
    for (position, &index) in order.iter().enumerate() {
        rank[index] = position;
    }
    let entry = function.entry.0;
    let mut idom = vec![usize::MAX; function.blocks.len()];
    idom[entry] = entry;
    let intersect = |idom: &[usize], mut a: usize, mut b: usize| {
        while a != b {
            while rank[a] > rank[b] {
                a = idom[a];
            }
            while rank[b] > rank[a] {
                b = idom[b];
            }
        }
        a
    };
    loop {
        let mut changed = false;
        for &index in &order[1..] {
            let next = flow.predecessors[index]
                .iter()
                .copied()
                .filter(|&predecessor| idom[predecessor] != usize::MAX)
                .reduce(|a, b| intersect(&idom, a, b))
                .expect("a reachable block has a processed predecessor");
            if idom[index] != next {
                idom[index] = next;
                changed = true;
            }
        }
        if !changed {
            return idom;
        }
    }
}

fn declarations(function: &MirFunction, flow: &Flow) -> Declarations {
    let idom = immediate_dominators(function, flow);
    let mut declared = vec![Vec::new(); function.blocks.len()];
    declared[function.entry.0] = vec![false; function.locals.len()];
    for &index in &flow.rpo[1..] {
        let parent = idom[index];
        let mut locals = declared[parent].clone();
        for statement in &function.blocks[parent].statements {
            if let MirStatement::Bind { local, .. } = statement {
                locals[local.0] = true;
            }
        }
        declared[index] = locals;
    }
    Declarations { declared }
}

fn initialization_error(function: &MirFunction, local: usize, detail: &str) -> Diagnostic {
    Diagnostic::codegen(format!(
        "MIR function '{}' {} local '{}'",
        function.name, detail, function.locals[local].name
    ))
}

fn validate(function: &MirFunction, flow: &Flow) -> Result<(), Diagnostic> {
    let declarations = declarations(function, flow);
    for &index in &flow.order {
        let mut state = flow.incoming[index].clone();
        for (local, definitions) in state.iter().enumerate() {
            // A branch/iteration-local binding has not begun its lifetime at
            // joins outside its declaration. Any actual use still requires
            // initialization on every path. Scope depths alone cannot express
            // this for synthetic lowering temporaries.
            let in_scope = function.locals[local].scope_depth <= function.blocks[index].scope_depth
                && (flow.entry_locals.contains(&MirLocalId(local))
                    || declarations.in_scope(local, index));
            if definitions.has_multiple()
                && definitions.may_be_uninitialized()
                && function.locals[local].ownership != MirOwnership::Borrowed
                && in_scope
            {
                return Err(initialization_error(
                    function,
                    local,
                    "merges inconsistent initialization of",
                ));
            }
        }
        for statement in &function.blocks[index].statements {
            match statement {
                MirStatement::Read { local, .. }
                | MirStatement::BorrowLocal { local, .. }
                | MirStatement::TakeLocal { local, .. }
                | MirStatement::DropLocal { local, .. } => {
                    if state[local.0].is_empty() || state[local.0].may_be_uninitialized() {
                        return Err(initialization_error(
                            function,
                            local.0,
                            "uses an uninitialized",
                        ));
                    }
                }
                MirStatement::Bind { local, .. }
                    if matches!(
                        function.locals[local.0].ownership,
                        MirOwnership::Owned | MirOwnership::Shared
                    ) && state[local.0].has_initialized() =>
                {
                    return Err(initialization_error(
                        function,
                        local.0,
                        "initializes an already live",
                    ));
                }
                _ => {}
            }
            transfer(&mut state, statement);
        }
        if matches!(
            function.blocks[index].terminator,
            Some(MirTerminator::Return(_))
        ) {
            for (local, definitions) in state.iter().enumerate() {
                if function.locals[local].ownership == MirOwnership::Shared
                    && definitions.has_initialized()
                {
                    return Err(initialization_error(
                        function,
                        local,
                        "returns with a live shared",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Check initialization independently of SSA value ownership, including Copy
/// and Shared slots. Reads of a Shared local do not consume its binding.
pub(crate) fn verify_initialization(function: &MirFunction) -> Result<(), Diagnostic> {
    validate(function, &analyze(function))
}

/// Assignment binds initially omit old-value cleanup. Once the entire CFG is
/// known, the RHS's move state determines whether that cleanup is necessary.
pub(crate) fn prepare_assignments(
    function: &mut MirFunction,
    assignments: &HashSet<MirValueId>,
) -> Result<(), Diagnostic> {
    let mut checked_locals = function.source.mutable_locals.clone();
    for statement in function.blocks.iter().flat_map(|block| &block.statements) {
        if let MirStatement::Bind { local, .. } = statement {
            if matches!(function.locals[local.0].ty, Type::Function(_)) {
                if let Some(span) = function.statement_span(statement) {
                    checked_locals.entry(*local).or_insert(span);
                }
            }
        }
    }
    if checked_locals.is_empty() {
        return Ok(());
    }
    let flow = analyze(function);
    let declarations = declarations(function, &flow);
    let mut replacements = Vec::new();
    let mut mutable_locals = checked_locals
        .iter()
        .map(|(local, span)| (*local, *span))
        .collect::<Vec<_>>();
    mutable_locals.sort_by_key(|(local, _)| local.0);
    for &index in &flow.order {
        let mut state = flow.incoming[index].clone();
        for &(local, declaration_span) in &mutable_locals {
            let in_scope = function.locals[local.0].scope_depth
                <= function.blocks[index].scope_depth
                && declarations.in_scope(local.0, index);
            if in_scope && state[local.0].has_multiple() && state[local.0].may_be_uninitialized() {
                let span = function.blocks[index]
                    .statements
                    .iter()
                    .find_map(|statement| function.statement_span(statement))
                    .unwrap_or(declaration_span);
                return Err(
                    crate::diagnostic::SemanticError::InconsistentInitialization {
                        name: function.locals[local.0].name.clone(),
                        span,
                    }
                    .into(),
                );
            }
        }
        for (position, statement) in function.blocks[index].statements.iter().enumerate() {
            match statement {
                MirStatement::Read { local, .. }
                | MirStatement::BorrowLocal { local, .. }
                | MirStatement::TakeLocal { local, .. }
                | MirStatement::DropLocal { local, .. }
                    if checked_locals.contains_key(local) =>
                {
                    if state[local.0].is_empty() || state[local.0].may_be_uninitialized() {
                        let span = function
                            .statement_span(statement)
                            .unwrap_or(checked_locals[local]);
                        return Err(crate::diagnostic::SemanticError::UseAfterMove {
                            name: function.locals[local.0].name.clone(),
                            span,
                        }
                        .into());
                    }
                }
                MirStatement::Bind {
                    local, destination, ..
                } if assignments.contains(destination)
                    && matches!(
                        function.locals[local.0].ownership,
                        MirOwnership::Shared | MirOwnership::Owned
                    )
                    && state[local.0].has_initialized() =>
                {
                    replacements.push((
                        index,
                        position,
                        *local,
                        function.statement_span(statement),
                    ));
                }
                _ => {}
            }
            transfer(&mut state, statement);
        }
    }
    // Insert backwards so statement positions remain stable within each block.
    replacements.sort_by_key(|(block, position, _, _)| (*block, *position));
    for (index, position, local, span) in replacements.into_iter().rev() {
        let destination = value(function, Type::Unit, MirOwnership::Copy);
        function.source.values[destination.0] = span;
        function.blocks[index]
            .statements
            .insert(position, MirStatement::DropLocal { destination, local });
    }
    Ok(())
}

fn live_locals(function: &MirFunction, flow: &Flow) -> Vec<Vec<bool>> {
    let mut live = vec![vec![false; function.locals.len()]; function.blocks.len()];
    loop {
        let mut changed = false;
        for &index in flow.rpo.iter().rev() {
            let mut next = vec![false; function.locals.len()];
            for target in successors(&function.blocks[index]) {
                for (local, live) in next.iter_mut().zip(&live[target]) {
                    *local |= live;
                }
            }
            for statement in function.blocks[index].statements.iter().rev() {
                match statement {
                    MirStatement::Bind { local, .. } => {
                        next[local.0] = false;
                    }
                    MirStatement::Read { local, .. }
                    | MirStatement::BorrowLocal { local, .. }
                    | MirStatement::TakeLocal { local, .. }
                    | MirStatement::DropLocal { local, .. } => {
                        next[local.0] = true;
                    }
                    _ => {}
                }
            }
            if next != live[index] {
                live[index] = next;
                changed = true;
            }
        }
        if !changed {
            return live;
        }
    }
}

fn value(function: &mut MirFunction, ty: Type, ownership: MirOwnership) -> MirValueId {
    let id = MirValueId(function.value_types.len());
    function.value_types.push(ty);
    function.value_ownership.push(ownership);
    function.source.values.resize(id.0 + 1, None);
    id
}

fn merge_local(
    function: &mut MirFunction,
    target: usize,
    local: MirLocalId,
    predecessors: &[usize],
) {
    let ty = function.locals[local.0].ty;
    let ownership = function.locals[local.0].ownership;
    let destination = value(function, ty, ownership);
    let bind_destination = value(function, Type::Unit, MirOwnership::Copy);
    let mut incoming = Vec::new();
    for &predecessor in predecessors {
        let edge = MirBlockId(function.blocks.len());
        let mut statements = Vec::new();
        let read = value(function, ty, ownership);
        // Owned and Shared slots transfer wholesale on the edge; the target's
        // Bind re-initializes the slot, so no Dup/DropLocal pair is needed.
        statements.push(
            if matches!(ownership, MirOwnership::Owned | MirOwnership::Shared) {
                MirStatement::TakeLocal {
                    destination: read,
                    local,
                }
            } else {
                MirStatement::Read {
                    destination: read,
                    local,
                }
            },
        );
        incoming.push((edge, read));
        let mut arguments = match function.blocks[predecessor].terminator.as_mut().unwrap() {
            MirTerminator::Goto {
                target: next,
                arguments,
            } => {
                assert_eq!(next.0, target);
                *next = edge;
                std::mem::take(arguments)
            }
            MirTerminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                if then_block.0 == target {
                    *then_block = edge;
                }
                if else_block.0 == target {
                    *else_block = edge;
                }
                Vec::new()
            }
            _ => unreachable!("a CFG predecessor has a successor"),
        };
        for statement in &mut function.blocks[target].statements {
            if let MirStatement::Phi { incoming, .. } = statement {
                for (block, _) in incoming {
                    if block.0 == predecessor {
                        *block = edge;
                    }
                }
            }
        }
        arguments.push(read);
        function.blocks.push(MirBlock {
            id: edge,
            scoped: false,
            scope_depth: function.blocks[predecessor].scope_depth,
            statements,
            terminator: Some(MirTerminator::Goto {
                target: MirBlockId(target),
                arguments,
            }),
        });
    }
    let phis = function.blocks[target]
        .statements
        .iter()
        .take_while(|s| matches!(s, MirStatement::Phi { .. }))
        .count();
    function.blocks[target].statements.insert(
        phis,
        MirStatement::Phi {
            destination,
            incoming,
        },
    );
    function.blocks[target].statements.insert(
        phis + 1,
        MirStatement::Bind {
            destination: bind_destination,
            local,
            value: Some(destination),
        },
    );
}

/// Materialize changed local values as ordinary Phi arguments. Transferring
/// owning slots on the edges preserves the verifier's single-owner invariant.
/// Run after scope cleanup insertion; initialization errors remain verifier
/// diagnostics, so normalization never repairs inconsistent ownership states.
pub(crate) fn normalize(function: &mut MirFunction) {
    let mut flow = analyze(function);
    loop {
        let live = live_locals(function, &flow);
        let candidate = flow.order.iter().find_map(|&index| {
            if flow.predecessors[index].len() < 2 {
                return None;
            }
            flow.incoming[index]
                .iter()
                .enumerate()
                .find_map(|(local, definitions)| {
                    (definitions.has_multiple()
                        && !definitions.may_be_uninitialized()
                        && live[index][local])
                        .then_some((index, MirLocalId(local)))
                })
        });
        let Some((target, local)) = candidate else {
            break;
        };
        merge_local(function, target, local, &flow.predecessors[target]);
        flow = analyze(function);
    }
}
