//! Conservative, interprocedural region checking over typed MIR. Summaries use
//! symbolic input/current regions, so factories inherit the caller's allocation
//! region without erasing lifetimes at ordinary function boundaries.
use super::*;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

type Set = BTreeSet<Origin>;
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord,
)]
enum Origin {
    /// Allocation region inherited by an ordinary call or structured task.
    Current,
    /// Lexical identity within this function; cannot appear in an exported result.
    Local(usize),
    /// Regions reachable through a parameter (including closure captures).
    Input(usize),
    /// Lifetime bound of a parameter's mutable storage, distinct from its contents.
    Storage(usize),
}
#[derive(Clone, Default, PartialEq, Eq)]
struct Flow {
    refs: Set,
    /// Keep alias identities as well as current contents, so later writes to an
    /// initially empty container remain visible through every copy/projection.
    stores: BTreeSet<usize>,
}
impl Flow {
    fn union(&mut self, other: &Self) {
        self.refs.extend(&other.refs);
        self.stores.extend(&other.stores);
    }
}
#[derive(Clone, Default, PartialEq, Eq)]
struct Heap {
    owner: Set,
    refs: Set,
    inputs: BTreeSet<usize>,
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    returned: Set,
    opens_region: bool,
    opaque: Set,
    /// User destructors and unknown language calls may observe these references
    /// during late cleanup. The root stays alive until all such cleanup drains.
    root_only: Set,
    aliases: BTreeSet<usize>,
    writes: Vec<Set>,
    constraints: BTreeSet<(Origin, Origin)>,
}

impl Summary {
    pub(crate) fn is_trivial(&self) -> bool {
        !self.opens_region
            && self.returned.is_empty()
            && self.opaque.is_empty()
            && self.root_only.is_empty()
            && self.aliases.is_empty()
            && self.writes.iter().all(Set::is_empty)
            && self.constraints.is_empty()
    }
}

fn relevant(types: &TypeTable, ty: Type, seen: &mut HashSet<Type>) -> bool {
    if !seen.insert(ty) {
        return false;
    }
    let children: Vec<Type> = match ty {
        Type::Cown(_)
        | Type::Function(_)
        | Type::Dyn(_)
        | Type::Param(_)
        | Type::SeqBuilder
        | Type::Batch
        | Type::CCallback => return true,
        Type::Class(id) => types.class_fields(id).iter().map(|(_, ty)| *ty).collect(),
        Type::Struct(id) => types.struct_fields(id).iter().map(|(_, ty)| *ty).collect(),
        Type::Enum(id) => types
            .enum_variants(id)
            .iter()
            .flat_map(|v| v.fields.iter().map(|(_, ty)| *ty))
            .collect(),
        Type::Tuple(id) => types.tuple_elements(id).to_vec(),
        Type::Option(id) => vec![types.option_type(id)],
        Type::Result(id) => {
            let (a, b) = types.result_types(id);
            vec![a, b]
        }
        Type::List(id) | Type::MutList(id) | Type::MutListCursor(id) => vec![types.list_type(id)],
        Type::Map(id)
        | Type::MutMap(id)
        | Type::MutSet(id)
        | Type::MapCursor(id)
        | Type::MapKeyCursor(id)
        | Type::MapValueCursor(id)
        | Type::MutMapCursor(id)
        | Type::MutSetCursor(id) => {
            let m = types.map_info(id);
            vec![m.key, m.value]
        }
        _ => vec![],
    };
    children.into_iter().any(|ty| relevant(types, ty, seen))
}
fn carries(types: &TypeTable, ty: Type) -> bool {
    relevant(types, ty, &mut HashSet::new())
}

/// Per block: the lexical scope stack observed at each statement boundary.
type ScopeStacks = Vec<Vec<Vec<usize>>>;
/// Per block: whether a when lease is held at each statement boundary.
type LeaseFlags = Vec<Vec<bool>>;

/// A lexical stack is attached to every statement, including resumed blocks.
fn scopes(function: &MirFunction) -> Result<(ScopeStacks, LeaseFlags), Diagnostic> {
    let regions: HashSet<_> = function
        .blocks
        .iter()
        .flat_map(|b| &b.statements)
        .filter_map(|s| match s {
            MirStatement::ScopeEnter {
                scope,
                region: true,
            } => Some(scope.0),
            _ => None,
        })
        .collect();
    let mut entries = vec![None; function.blocks.len()];
    entries[function.entry.0] = Some((Vec::new(), 0usize));
    let mut work = VecDeque::from([function.entry.0]);
    let mut result = vec![Vec::new(); function.blocks.len()];
    let mut leases = vec![Vec::new(); function.blocks.len()];
    while let Some(index) = work.pop_front() {
        let (mut stack, mut held) = entries[index].clone().unwrap();
        for s in &function.blocks[index].statements {
            result[index].push(stack.clone());
            leases[index].push(held != 0);
            match s {
                MirStatement::ScopeEnter {
                    scope,
                    region: true,
                } => {
                    if held != 0 {
                        return Err(Diagnostic::codegen(
                            "region cannot be entered while holding a when lease",
                        ));
                    }
                    stack.push(scope.0);
                }
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownAcquireMany,
                    arguments,
                    ..
                }
                | MirStatement::CownAcquire {
                    arguments,
                    wait_for_change: false,
                    ..
                } => held += arguments.len(),
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownAcquire(_),
                    ..
                } => held += 1,
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownRelease,
                    ..
                } => held = held.saturating_sub(1),
                MirStatement::ScopeExit { scope }
                    if regions.contains(&scope.0) && stack.pop() != Some(scope.0) =>
                {
                    return Err(Diagnostic::codegen("unbalanced region exit"));
                }
                _ => {}
            }
        }
        result[index].push(stack.clone());
        let successors = match &function.blocks[index].terminator {
            Some(MirTerminator::Goto { target, .. }) => vec![target.0],
            Some(MirTerminator::Branch {
                then_block,
                else_block,
                ..
            }) => vec![then_block.0, else_block.0],
            _ => Vec::new(),
        };
        for next in successors {
            match &entries[next] {
                Some(previous) if previous != &(stack.clone(), held) => {
                    return Err(Diagnostic::codegen(
                        "inconsistent region lifetime at control-flow merge",
                    ))
                }
                Some(_) => {}
                None => {
                    entries[next] = Some((stack.clone(), held));
                    work.push_back(next);
                }
            }
        }
    }
    Ok((result, leases))
}

struct Analysis<'a> {
    function: &'a MirFunction,
    program: &'a MirProgram,
    summaries: &'a HashMap<MirFunctionId, Summary>,
    values: Vec<Flow>,
    locals: Vec<Flow>,
    heap: Vec<Heap>,
    local_owners: Vec<Set>,
    stacks: Vec<Vec<Vec<usize>>>,
    leases: Vec<Vec<bool>>,
    held: bool,
    summary: Summary,
}
impl Analysis<'_> {
    fn error(&self, message: &str, statement: Option<&MirStatement>) -> Diagnostic {
        Diagnostic::semantic(
            format!("region: {message} (in '{}')", self.function.name),
            statement
                .and_then(|s| self.function.statement_span(s))
                .or(self.function.source.body)
                .unwrap_or(crate::Span::new(0, 0)),
        )
    }
    fn refs(&self, flow: &Flow) -> Set {
        let mut refs = flow.refs.clone();
        for &store in &flow.stores {
            refs.extend(&self.heap[store].refs);
        }
        refs
    }
    fn owners(&self, flow: &Flow) -> Set {
        flow.stores
            .iter()
            .flat_map(|&s| self.heap[s].owner.iter().copied())
            .collect()
    }
    fn map(&self, origin: Origin, args: &[Flow], current: Origin) -> Set {
        match origin {
            Origin::Current => Set::from([current]),
            Origin::Input(i) => args.get(i).map(|a| self.refs(a)).unwrap_or_default(),
            Origin::Storage(i) => args.get(i).map(|a| self.owners(a)).unwrap_or_default(),
            Origin::Local(_) => Set::from([origin]),
        }
    }
    fn constraint(
        &mut self,
        source: Origin,
        target: Origin,
        stack: &[usize],
        s: &MirStatement,
    ) -> Result<(), Diagnostic> {
        if source == target {
            return Ok(());
        }
        match (source, target) {
            (Origin::Local(a), Origin::Local(b)) => {
                if stack
                    .iter()
                    .position(|&r| r == a)
                    .zip(stack.iter().position(|&r| r == b))
                    .is_some_and(|(a, b)| a <= b)
                {
                    return Ok(());
                }
                return Err(self.error(
                    "Cown reference escapes its region through an outer store or result",
                    Some(s),
                ));
            }
            (Origin::Local(_), _) => {
                return Err(self.error(
                    "Cown reference escapes its region through an outer store or result",
                    Some(s),
                ))
            }
            (_, Origin::Local(_)) | (Origin::Input(_) | Origin::Storage(_), Origin::Current) => {
                return Ok(())
            }
            _ => {
                self.summary.constraints.insert((source, target));
            }
        }
        Ok(())
    }
    fn store(
        &mut self,
        target: &Flow,
        value: &Flow,
        stack: &[usize],
        s: &MirStatement,
    ) -> Result<(), Diagnostic> {
        let refs = self.refs(value);
        let owners = self.owners(target);
        for &source in &refs {
            for &owner in &owners {
                self.constraint(source, owner, stack, s)?;
            }
        }
        for &store in &target.stores {
            self.heap[store].refs.extend(&refs);
        }
        Ok(())
    }
    fn opaque(&mut self, refs: Set, s: &MirStatement) -> Result<(), Diagnostic> {
        if refs
            .iter()
            .any(|r| matches!(r, Origin::Current | Origin::Local(_)))
        {
            return Err(self.error("Cown references require a verified region contract across indirect calls, effects or type erasure",Some(s)));
        }
        self.summary.opaque.extend(refs);
        Ok(())
    }

    fn require_root(&mut self, refs: Set, s: &MirStatement) -> Result<(), Diagnostic> {
        if refs.iter().any(|r| matches!(r, Origin::Local(_))) {
            return Err(self.error("user Drop or indirect call may observe Cown references after region cleanup; only root-region references are supported here", Some(s)));
        }
        self.summary.root_only.extend(refs);
        Ok(())
    }
    fn call(
        &mut self,
        id: MirFunctionId,
        args: &[Flow],
        current: Origin,
        stack: &[usize],
        s: &MirStatement,
    ) -> Result<Flow, Diagnostic> {
        let summary = self.summaries.get(&id).cloned().unwrap_or_default();
        if self.held && summary.opens_region {
            return Err(self.error("a call inside when may enter a region", Some(s)));
        }
        self.summary.opens_region |= summary.opens_region;
        let opaque = summary
            .opaque
            .iter()
            .flat_map(|&r| self.map(r, args, current))
            .collect();
        self.opaque(opaque, s)?;
        let root_only = summary
            .root_only
            .iter()
            .flat_map(|&r| self.map(r, args, current))
            .collect();
        self.require_root(root_only, s)?;
        for (source, target) in summary.constraints {
            for source in self.map(source, args, current) {
                for target in self.map(target, args, current) {
                    self.constraint(source, target, stack, s)?;
                }
            }
        }
        for (i, writes) in summary.writes.iter().enumerate() {
            if let Some(target) = args.get(i) {
                let value = Flow {
                    refs: writes
                        .iter()
                        .flat_map(|&r| self.map(r, args, current))
                        .collect(),
                    stores: BTreeSet::new(),
                };
                self.store(target, &value, stack, s)?;
            }
        }
        let mut result = Flow {
            refs: summary
                .returned
                .iter()
                .flat_map(|&r| self.map(r, args, current))
                .collect(),
            stores: BTreeSet::new(),
        };
        for i in summary.aliases {
            if let Some(arg) = args.get(i) {
                result.stores.extend(&arg.stores);
            }
        }
        Ok(result)
    }
    fn arguments(&self, args: &[MirCallArgument]) -> Vec<Flow> {
        let mut result =
            vec![Flow::default(); args.iter().map(|a| a.parameter + 1).max().unwrap_or(0)];
        for a in args {
            result[a.parameter] = self.values[a.value.0].clone();
        }
        result
    }
    fn run(mut self) -> Result<Summary, Diagnostic> {
        let f = self.function;
        let mut tasks = HashMap::<MirTaskId, Flow>::new();
        loop {
            let before = (
                self.values.clone(),
                self.locals.clone(),
                self.heap.clone(),
                self.summary.clone(),
                tasks.clone(),
            );
            for (b, block) in f.blocks.iter().enumerate() {
                if self.stacks[b].is_empty() {
                    continue;
                }
                for (i, s) in block.statements.iter().enumerate() {
                    let stack = self.stacks[b][i].clone();
                    self.held = self.leases[b][i];
                    let current = stack.last().map_or(Origin::Current, |&r| Origin::Local(r));
                    let destination = super::lower::statement_destination(s);
                    let mut out = Flow::default();
                    let cleanup = matches!(
                        s,
                        MirStatement::Drop { .. }
                            | MirStatement::DropLocal { .. }
                            | MirStatement::Deinit { .. }
                    );
                    if !cleanup && !matches!(s, MirStatement::Phi { .. }) {
                        for operand in super::verifier::statement_operands(s) {
                            if self
                                .refs(&self.values[operand.0])
                                .iter()
                                .any(|r| matches!(r, Origin::Local(id) if !stack.contains(id)))
                            {
                                return Err(self.error(
                                    "Cown reference is used after its region exits",
                                    Some(s),
                                ));
                            }
                        }
                    }
                    match s {
                        MirStatement::ScopeEnter { region: true, .. } => {
                            self.summary.opens_region = true
                        }
                        MirStatement::Read { local, .. }
                        | MirStatement::BorrowLocal { local, .. }
                        | MirStatement::TakeLocal { local, .. } => {
                            out = self.locals[local.0].clone()
                        }
                        MirStatement::Bind {
                            local,
                            value: Some(value),
                            ..
                        } => {
                            out = self.values[value.0].clone();
                            for source in self.refs(&out) {
                                for target in self.local_owners[local.0].clone() {
                                    self.constraint(source, target, &stack, s)?;
                                }
                            }
                            self.locals[local.0].union(&out);
                            out = Flow::default();
                        }
                        MirStatement::Dup { value, .. }
                        | MirStatement::Move { value, .. }
                        | MirStatement::Project { base: value, .. }
                        | MirStatement::EnumProject { value, .. }
                        | MirStatement::DynamicUpcast { value, .. }
                        | MirStatement::DynamicValue { value, .. } => {
                            out = self.values[value.0].clone()
                        }
                        MirStatement::Phi { incoming, .. } => {
                            for (_, v) in incoming {
                                out.union(&self.values[v.0]);
                            }
                        }
                        MirStatement::Store {
                            receiver, value, ..
                        } => self.store(
                            &self.values[receiver.0].clone(),
                            &self.values[value.0].clone(),
                            &stack,
                            s,
                        )?,
                        MirStatement::RuntimeCall {
                            intrinsic: RuntimeIntrinsic::CownNew(_),
                            arguments,
                            ..
                        } => {
                            out.refs.insert(current);
                            // Moving a unique payload transfers its storage to the Cown.
                            for arg in arguments {
                                let value = self.values[arg.value.0].clone();
                                for source in self.refs(&value) {
                                    self.constraint(source, current, &stack, s)?;
                                }
                                for store in value.stores {
                                    self.heap[store].owner = Set::from([current]);
                                }
                            }
                        }
                        MirStatement::RuntimeCall {
                            intrinsic:
                                RuntimeIntrinsic::CownPayload(_) | RuntimeIntrinsic::CownAcquire(_),
                            arguments,
                            ..
                        } => {
                            let id = destination.unwrap().0;
                            let refs = self.refs(&self.values[arguments[0].value.0]);
                            self.heap[id].owner.extend(&refs);
                            self.heap[id].refs.extend(refs);
                            out.stores.insert(id);
                        }
                        MirStatement::Call {
                            function,
                            arguments,
                            ..
                        } => {
                            let args = self.arguments(arguments);
                            out = self.call(*function, &args, current, &stack, s)?;
                        }
                        MirStatement::MethodCall {
                            method,
                            receiver,
                            arguments,
                            ..
                        } => {
                            let mut args = vec![self.values[receiver.0].clone()];
                            args.extend(self.arguments(arguments));
                            out = self.call(*method, &args, current, &stack, s)?;
                        }
                        MirStatement::TaskCreate {
                            function,
                            task,
                            arguments,
                            ..
                        } => {
                            let args = self.arguments(arguments);
                            let flow = self.call(*function, &args, current, &stack, s)?;
                            tasks.entry(*task).or_default().union(&flow);
                        }
                        MirStatement::TaskJoin { task, .. } => {
                            out = tasks.get(task).cloned().unwrap_or_default()
                        }
                        MirStatement::RaceSelect { tasks: ids, .. } => {
                            for id in ids {
                                if let Some(flow) = tasks.get(id) {
                                    out.union(flow);
                                }
                            }
                        }
                        MirStatement::CallIndirect {
                            callee, arguments, ..
                        } => {
                            if self.held {
                                return Err(self.error("indirect calls inside when need a verified region-entry contract",Some(s)));
                            }
                            self.summary.opens_region = true;
                            // An unavailable body may create a Cown-carrying user
                            // destructor, including through another helper.
                            self.require_root(Set::from([current]), s)?;
                            let mut args = self.arguments(arguments);
                            args.push(self.values[callee.0].clone());
                            for arg in &args {
                                out.union(arg);
                            }
                            let mut sources = self.refs(&out);
                            sources.insert(current);
                            // Unknown language closures may copy any input or a newly
                            // allocated Cown into captured state. Require that worst-case
                            // contract instead of erasing the captures' region bounds.
                            for target in &args {
                                for owner in
                                    self.refs(target).into_iter().chain(self.owners(target))
                                {
                                    for &source in &sources {
                                        self.constraint(source, owner, &stack, s)?;
                                    }
                                }
                                self.store(
                                    target,
                                    &Flow {
                                        refs: sources.clone(),
                                        stores: BTreeSet::new(),
                                    },
                                    &stack,
                                    s,
                                )?;
                            }
                            if destination
                                .is_some_and(|d| carries(&self.program.types, f.value_types[d.0]))
                            {
                                out.refs.insert(current);
                            }
                        }
                        MirStatement::TaskAbort { .. }
                        | MirStatement::HandlerRequest { .. }
                        | MirStatement::ResumableRequest { .. }
                        | MirStatement::Suspend { .. } => {
                            let refs = super::verifier::statement_operands(s)
                                .iter()
                                .flat_map(|v| self.refs(&self.values[v.0]))
                                .collect();
                            self.opaque(refs, s)?;
                            if destination
                                .is_some_and(|d| carries(&self.program.types, f.value_types[d.0]))
                            {
                                return Err(self.error("effect or erased return value needs a verified region contract",Some(s)));
                            }
                        }
                        MirStatement::RuntimeCall { arguments, .. } => {
                            for arg in arguments {
                                out.union(&self.values[arg.value.0]);
                            }
                            // Runtime containers may mutate their receiver. Preserve every
                            // possible stored dependency, including aliases of empty containers.
                            for arg in arguments {
                                if matches!(
                                    f.value_types[arg.value.0],
                                    Type::MutList(_)
                                        | Type::MutMap(_)
                                        | Type::MutSet(_)
                                        | Type::SeqBuilder
                                        | Type::Batch
                                ) {
                                    self.store(&self.values[arg.value.0].clone(), &out, &stack, s)?;
                                }
                            }
                        }
                        _ if !cleanup => {
                            for v in super::verifier::statement_operands(s) {
                                out.union(&self.values[v.0]);
                            }
                        }
                        _ => {}
                    }
                    if let Some(d) = destination {
                        let ty = f.value_types[d.0];
                        if carries(&self.program.types, ty) && !cleanup {
                            if self.program.types.drop_contract(ty).0 {
                                self.require_root(self.refs(&out), s)?;
                            }
                            let fresh = matches!(
                                s,
                                MirStatement::Construct { .. }
                                    | MirStatement::Tuple { .. }
                                    | MirStatement::EnumConstruct { .. }
                                    | MirStatement::FunctionValue { .. }
                                    | MirStatement::Call { .. }
                                    | MirStatement::MethodCall { .. }
                                    | MirStatement::RuntimeCall { .. }
                            );
                            if fresh && !matches!(ty, Type::Cown(_)) {
                                let refs = self.refs(&out);
                                if self.heap[d.0].owner.is_empty() {
                                    self.heap[d.0].owner.insert(current);
                                }
                                self.heap[d.0].refs.extend(refs);
                                out.stores.insert(d.0);
                            }
                            self.values[d.0].union(&out);
                        }
                    }
                }
                if let Some(MirTerminator::Return(Some(v))) = block.terminator {
                    let flow = &self.values[v.0];
                    let refs = self.refs(flow);
                    if refs.iter().any(|r| matches!(r, Origin::Local(_))) {
                        return Err(self
                            .error("returned value contains a Cown from an exited region", None));
                    }
                    self.summary.returned.extend(refs);
                    for &store in &flow.stores {
                        self.summary.aliases.extend(&self.heap[store].inputs);
                    }
                }
            }
            for heap in &self.heap {
                for &p in &heap.inputs {
                    self.summary.writes[p].extend(&heap.refs);
                }
            }
            if before
                == (
                    self.values.clone(),
                    self.locals.clone(),
                    self.heap.clone(),
                    self.summary.clone(),
                    tasks.clone(),
                )
            {
                break;
            }
        }
        Ok(self.summary)
    }
}

pub(crate) fn verify(program: &MirProgram) -> Result<(), Diagnostic> {
    summarize(program).map(|_| ())
}

pub(crate) fn summarize(
    program: &MirProgram,
) -> Result<HashMap<MirFunctionId, Summary>, Diagnostic> {
    let mut summaries = HashMap::new();
    loop {
        let previous = summaries.clone();
        for f in &program.functions {
            let params: Vec<_> = f
                .receiver_local
                .into_iter()
                .chain(f.parameters.iter().map(|p| p.local))
                .collect();
            let (stacks, leases) = scopes(f)?;
            let mut analysis = Analysis {
                function: f,
                program,
                summaries: &previous,
                values: vec![Flow::default(); f.value_types.len()],
                locals: vec![Flow::default(); f.locals.len()],
                heap: vec![Heap::default(); f.value_types.len() + params.len()],
                local_owners: vec![Set::new(); f.locals.len()],
                stacks,
                leases,
                held: false,
                summary: Summary {
                    writes: vec![Set::new(); params.len()],
                    ..Summary::default()
                },
            };
            for (i, &local) in params.iter().enumerate() {
                if !carries(&program.types, f.locals[local.0].ty) {
                    continue;
                }
                let id = f.value_types.len() + i;
                analysis.locals[local.0].refs.insert(Origin::Input(i));
                if !matches!(f.locals[local.0].ty, Type::Cown(_)) {
                    analysis.heap[id].owner.insert(Origin::Storage(i));
                    analysis.heap[id].inputs.insert(i);
                    analysis.locals[local.0].stores.insert(id);
                }
                analysis.local_owners[local.0].insert(Origin::Current);
            }
            for (b, block) in f.blocks.iter().enumerate() {
                if analysis.stacks[b].is_empty() {
                    continue;
                }
                for (i, s) in block.statements.iter().enumerate() {
                    if let MirStatement::Bind { local, .. } = s {
                        let owner = analysis.stacks[b][i]
                            .last()
                            .map_or(Origin::Current, |&r| Origin::Local(r));
                        analysis.local_owners[local.0].insert(owner);
                    }
                }
            }
            if f.external_symbol.is_some() || f.foreign.is_some() {
                if f.external_symbol.is_some() {
                    if let Some(contract) = &f.imported_region_contract {
                        summaries.insert(f.id, contract.clone());
                        continue;
                    }
                }
                let mut summary = Summary {
                    writes: vec![Set::new(); params.len()],
                    opens_region: true,
                    ..Summary::default()
                };
                if f.foreign.is_some() {
                    summary.opaque.extend((0..params.len()).map(Origin::Input));
                    // Sema restricts the C ABI to scalars, raw pointers and
                    // capture-free callback trampolines. C cannot write a Joky
                    // Cown into a callback's closure environment. Applying the
                    // unknown-language-body writes below would invent a Current
                    // dependency and reject that same callback on the next pass.
                    // Keep the opaque-input check: actual Cown dependencies must
                    // still never cross an unverified native boundary.
                    summaries.insert(f.id, summary);
                    continue;
                }
                if carries(&program.types, f.return_type) {
                    summary.root_only.insert(Origin::Current);
                    summary.returned.insert(Origin::Current);
                    summary
                        .returned
                        .extend((0..params.len()).map(Origin::Input));
                }
                // An unavailable body may store any argument into another.
                for (i, &local) in params.iter().enumerate() {
                    if carries(&program.types, f.locals[local.0].ty) {
                        summary.root_only.insert(Origin::Current);
                        summary.root_only.insert(Origin::Input(i));
                        summary.writes[i].insert(Origin::Current);
                        summary.writes[i].extend((0..params.len()).map(Origin::Input));
                        let target = if matches!(f.locals[local.0].ty, Type::Cown(_)) {
                            Origin::Input(i)
                        } else {
                            Origin::Storage(i)
                        };
                        summary.constraints.insert((Origin::Current, target));
                        for j in 0..params.len() {
                            summary.constraints.insert((Origin::Input(j), target));
                        }
                    }
                }
                summaries.insert(f.id, summary);
            } else {
                summaries.insert(f.id, analysis.run()?);
            }
        }
        if summaries == previous {
            break;
        }
    }
    Ok(summaries)
}
