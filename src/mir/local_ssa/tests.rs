use super::*;
use crate::{sema, syntax};

struct Fixture {
    program: MirProgram,
    index: usize,
}

impl Fixture {
    fn new() -> Self {
        Self::from_source("fn test() -> Int32 { 0 }\nfn main() {}")
    }

    fn from_source(source: &str) -> Self {
        let ast = syntax::parse_program(source).unwrap();
        let types = sema::check_program(&ast).unwrap();
        let core = CoreProgram::lower(ast, types).unwrap();
        let mut program = MirProgram::lower(&core).unwrap();
        let index = program
            .functions
            .iter()
            .position(|f| f.name == "test")
            .unwrap();
        let function = &mut program.functions[index];
        function.blocks.clear();
        function.value_types.clear();
        function.value_ownership.clear();
        function.source.values.clear();
        Self { program, index }
    }

    fn function(&mut self) -> &mut MirFunction {
        &mut self.program.functions[self.index]
    }

    fn block(&mut self, depth: usize) -> usize {
        let function = self.function();
        let index = function.blocks.len();
        function.blocks.push(MirBlock {
            id: MirBlockId(index),
            scoped: false,
            scope_depth: depth,
            statements: Vec::new(),
            terminator: Some(MirTerminator::Unreachable),
        });
        index
    }

    fn local(&mut self, name: &str, ty: Type, ownership: MirOwnership, depth: usize) -> MirLocalId {
        let function = self.function();
        let id = MirLocalId(function.locals.len());
        function.locals.push(MirLocal {
            id,
            name: name.to_owned(),
            ty,
            ownership,
            scope_depth: depth,
        });
        id
    }

    fn constant(
        &mut self,
        block: usize,
        constant: MirConstant,
        ty: Type,
        ownership: MirOwnership,
    ) -> MirValueId {
        let destination = value(self.function(), ty, ownership);
        self.function().blocks[block]
            .statements
            .push(MirStatement::Const {
                destination,
                value: constant,
            });
        destination
    }

    fn int(&mut self, block: usize, number: u64) -> MirValueId {
        self.constant(
            block,
            MirConstant::Integer(number),
            Type::I32,
            MirOwnership::Copy,
        )
    }

    fn bind(&mut self, block: usize, local: MirLocalId, source: MirValueId) {
        let destination = value(self.function(), Type::Unit, MirOwnership::Copy);
        self.function().blocks[block]
            .statements
            .push(MirStatement::Bind {
                destination,
                local,
                value: Some(source),
            });
    }

    fn drop_local(&mut self, block: usize, local: MirLocalId) {
        let destination = value(self.function(), Type::Unit, MirOwnership::Copy);
        self.function().blocks[block]
            .statements
            .push(MirStatement::DropLocal { destination, local });
    }

    fn read(&mut self, block: usize, local: MirLocalId) -> MirValueId {
        let ty = self.function().locals[local.0].ty;
        let ownership = self.function().locals[local.0].ownership;
        let destination = value(self.function(), ty, ownership);
        self.function().blocks[block]
            .statements
            .push(MirStatement::Read { destination, local });
        destination
    }

    fn goto(&mut self, block: usize, target: usize) {
        self.function().blocks[block].terminator = Some(MirTerminator::Goto {
            target: MirBlockId(target),
            arguments: Vec::new(),
        });
    }

    fn branch(&mut self, block: usize, yes: usize, no: usize) {
        let condition = self.constant(
            block,
            MirConstant::Boolean(true),
            Type::Bool,
            MirOwnership::Copy,
        );
        self.function().blocks[block].terminator = Some(MirTerminator::Branch {
            condition,
            then_block: MirBlockId(yes),
            else_block: MirBlockId(no),
        });
    }

    fn return_int(&mut self, block: usize, number: u64) {
        let result = self.int(block, number);
        self.function().blocks[block].terminator = Some(MirTerminator::Return(Some(result)));
    }

    fn normalize_and_verify(&mut self) {
        for block in &mut self.function().blocks {
            if matches!(block.terminator, Some(MirTerminator::Return(_))) {
                block.statements.push(MirStatement::ScopeExit {
                    scope: MirScopeId(0),
                });
            }
        }
        normalize(self.function());
        verify_initialization(self.function()).unwrap();
        self.program.verify().unwrap();
    }
}

#[test]
fn unreachable_predecessors_do_not_change_initialization_or_require_phi() {
    let mut f = Fixture::new();
    for _ in 0..3 {
        f.block(0);
    }
    let local = f.local("counter", Type::I32, MirOwnership::Copy, 0);
    let initial = f.int(0, 1);
    f.bind(0, local, initial);
    f.goto(0, 1);
    let result = f.read(1, local);
    f.function().blocks[1].terminator = Some(MirTerminator::Return(Some(result)));
    f.goto(2, 1);
    f.normalize_and_verify();
    assert_eq!(f.function().blocks.len(), 3);
    assert!(!f.function().blocks[1]
        .statements
        .iter()
        .any(|s| matches!(s, MirStatement::Phi { .. })));
}

#[test]
fn shadowed_branch_locals_do_not_merge_with_the_outer_binding() {
    let mut f = Fixture::new();
    for depth in [0, 1, 1, 0] {
        f.block(depth);
    }
    let outer = f.local("value", Type::I32, MirOwnership::Copy, 0);
    let left = f.local("value", Type::I32, MirOwnership::Copy, 1);
    let right = f.local("value", Type::I32, MirOwnership::Copy, 1);
    let initial = f.int(0, 1);
    f.bind(0, outer, initial);
    f.branch(0, 1, 2);
    for (block, local) in [(1, left), (2, right)] {
        let replacement = f.int(block, block as u64);
        f.bind(block, local, replacement);
        f.goto(block, 3);
    }
    let result = f.read(3, outer);
    f.function().blocks[3].terminator = Some(MirTerminator::Return(Some(result)));
    f.normalize_and_verify();
    assert_eq!(f.function().blocks.len(), 4);
}

#[test]
fn copy_read_requires_initialization_on_every_path() {
    let mut f = Fixture::new();
    for _ in 0..4 {
        f.block(0);
    }
    let local = f.local("counter", Type::I32, MirOwnership::Copy, 0);
    f.branch(0, 1, 2);
    let initial = f.int(1, 1);
    f.bind(1, local, initial);
    f.goto(1, 3);
    f.goto(2, 3);
    f.read(3, local);
    f.return_int(3, 0);
    let error = verify_initialization(f.function()).unwrap_err().to_string();
    assert!(
        error.contains("uninitialized") && error.contains("counter"),
        "{error}"
    );
}

#[test]
fn shared_initialization_disagreement_is_rejected_without_a_later_read() {
    let mut f = Fixture::new();
    for _ in 0..4 {
        f.block(0);
    }
    let local = f.local("text", Type::String, MirOwnership::Shared, 0);
    let initial = f.constant(
        0,
        MirConstant::String("before".into()),
        Type::String,
        MirOwnership::Shared,
    );
    f.bind(0, local, initial);
    f.branch(0, 1, 2);
    f.drop_local(1, local);
    f.goto(1, 3);
    f.goto(2, 3);
    f.return_int(3, 0);
    let error = verify_initialization(f.function()).unwrap_err().to_string();
    assert!(
        error.contains("inconsistent initialization") && error.contains("text"),
        "{error}"
    );
}

#[test]
fn shared_reads_preserve_initialization_and_live_rebinding_is_rejected() {
    let mut f = Fixture::new();
    f.block(0);
    let local = f.local("text", Type::String, MirOwnership::Shared, 0);
    let initial = f.constant(
        0,
        MirConstant::String("before".into()),
        Type::String,
        MirOwnership::Shared,
    );
    f.bind(0, local, initial);
    f.read(0, local);
    f.read(0, local);
    verify_initialization(f.function()).unwrap();
    let replacement = f.constant(
        0,
        MirConstant::String("after".into()),
        Type::String,
        MirOwnership::Shared,
    );
    f.bind(0, local, replacement);
    let error = verify_initialization(f.function()).unwrap_err().to_string();
    assert!(
        error.contains("already live") && error.contains("text"),
        "{error}"
    );
}

#[test]
fn shared_loop_reinitialization_produces_a_verified_header_phi() {
    let mut f = Fixture::new();
    for _ in 0..4 {
        f.block(0);
    }
    let local = f.local("text", Type::String, MirOwnership::Shared, 0);
    let initial = f.constant(
        0,
        MirConstant::String("before".into()),
        Type::String,
        MirOwnership::Shared,
    );
    f.bind(0, local, initial);
    f.goto(0, 1);
    f.branch(1, 2, 3);
    f.drop_local(2, local);
    let replacement = f.constant(
        2,
        MirConstant::String("after".into()),
        Type::String,
        MirOwnership::Shared,
    );
    f.bind(2, local, replacement);
    f.goto(2, 1);
    f.drop_local(3, local);
    f.return_int(3, 0);
    f.normalize_and_verify();
    assert!(matches!(
        f.function().blocks[1].statements.first(),
        Some(MirStatement::Phi { .. })
    ));
    let blocks = f.function().blocks.len();
    normalize(f.function());
    assert_eq!(
        f.function().blocks.len(),
        blocks,
        "normalization is idempotent"
    );
}

#[test]
fn unchanged_owned_local_keeps_a_borrow_alive_across_a_diamond() {
    let mut f = Fixture::from_source("class Token { let value: Int32 }\nfn test(token: Token) -> Int32 { token.value }\nfn main() {}");
    for _ in 0..4 {
        f.block(0);
    }
    let local = f.function().parameters[0].local;
    let ty = f.function().locals[local.0].ty;
    let borrow = value(f.function(), ty, MirOwnership::Borrowed);
    f.function().blocks[0]
        .statements
        .push(MirStatement::BorrowLocal {
            destination: borrow,
            local,
        });
    f.branch(0, 1, 2);
    f.goto(1, 3);
    f.goto(2, 3);
    let result = value(f.function(), Type::I32, MirOwnership::Copy);
    f.function().blocks[3]
        .statements
        .push(MirStatement::Project {
            destination: result,
            base: borrow,
            access: MirFieldAccess::Index(0),
        });
    f.drop_local(3, local);
    f.function().blocks[3].terminator = Some(MirTerminator::Return(Some(result)));
    f.normalize_and_verify();
    assert_eq!(f.function().blocks.len(), 4);
    assert!(!f
        .function()
        .blocks
        .iter()
        .flat_map(|b| &b.statements)
        .any(|s| matches!(s, MirStatement::TakeLocal { .. })));
}

#[test]
fn local_phi_preserves_existing_phi_arguments_and_predecessors() {
    let mut f = Fixture::new();
    for _ in 0..4 {
        f.block(0);
    }
    let local = f.local("counter", Type::I32, MirOwnership::Copy, 0);
    let initial = f.int(0, 0);
    f.bind(0, local, initial);
    f.branch(0, 1, 2);
    let mut incoming = Vec::new();
    for block in [1, 2] {
        let replacement = f.int(block, block as u64);
        f.bind(block, local, replacement);
        incoming.push((MirBlockId(block), replacement));
        f.function().blocks[block].terminator = Some(MirTerminator::Goto {
            target: MirBlockId(3),
            arguments: vec![replacement],
        });
    }
    let selected = value(f.function(), Type::I32, MirOwnership::Copy);
    f.function().blocks[3].statements.push(MirStatement::Phi {
        destination: selected,
        incoming,
    });
    let current = f.read(3, local);
    let result = value(f.function(), Type::I32, MirOwnership::Copy);
    f.function().blocks[3]
        .statements
        .push(MirStatement::Binary {
            destination: result,
            op: syntax::BinaryOp::Add,
            left: selected,
            right: current,
        });
    f.function().blocks[3].terminator = Some(MirTerminator::Return(Some(result)));
    f.normalize_and_verify();
    assert_eq!(
        f.function().blocks[3]
            .statements
            .iter()
            .take_while(|statement| matches!(statement, MirStatement::Phi { .. }))
            .count(),
        2
    );
}

#[test]
fn owned_branch_replacements_merge_even_when_only_cleanup_uses_them() {
    let mut f =
        Fixture::from_source("class Token {}\nfn test(token: Token) -> Int32 { 0 }\nfn main() {}");
    for _ in 0..4 {
        f.block(0);
    }
    let local = f.function().parameters[0].local;
    let ty = f.function().locals[local.0].ty;
    f.branch(0, 1, 2);
    for block in [1, 2] {
        f.drop_local(block, local);
        let replacement = value(f.function(), ty, MirOwnership::Owned);
        f.function().blocks[block]
            .statements
            .push(MirStatement::Construct {
                destination: replacement,
                type_id: MirTypeId::from_type(ty).unwrap(),
                fields: Vec::new(),
            });
        f.bind(block, local, replacement);
        f.goto(block, 3);
    }
    f.drop_local(3, local);
    f.return_int(3, 0);
    f.normalize_and_verify();
    assert!(matches!(
        f.function().blocks[3].statements.first(),
        Some(MirStatement::Phi { .. })
    ));
    assert_eq!(
        f.function()
            .blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| matches!(statement, MirStatement::TakeLocal { .. }))
            .count(),
        2
    );
}

#[test]
fn shared_scope_exit_requires_releasing_the_current_binding() {
    let mut f = Fixture::new();
    f.block(0);
    let local = f.local("text", Type::String, MirOwnership::Shared, 0);
    let initial = f.constant(
        0,
        MirConstant::String("owned".into()),
        Type::String,
        MirOwnership::Shared,
    );
    f.bind(0, local, initial);
    f.return_int(0, 0);
    assert!(verify_initialization(f.function())
        .unwrap_err()
        .to_string()
        .contains("live shared"));
    f.drop_local(0, local);
    f.normalize_and_verify();
}

#[test]
fn inline_reaching_definitions_match_set_lattice() {
    let universe = [
        Definition::Uninitialized,
        Definition::Entry,
        Definition::Bind(MirValueId(0)),
        Definition::Bind(MirValueId(1)),
    ];
    let sets = (0..1 << universe.len())
        .map(|mask| {
            universe
                .iter()
                .enumerate()
                .filter_map(|(i, definition)| (mask & (1 << i) != 0).then_some(*definition))
                .collect::<HashSet<_>>()
        })
        .collect::<Vec<_>>();
    let summarize = |set: &HashSet<Definition>| {
        let mut state = ReachingDefinitions::Empty;
        for &definition in set {
            state.union_with(ReachingDefinitions::One(definition));
        }
        state
    };
    for a in &sets {
        for b in &sets {
            let expected = a.union(b).copied().collect::<HashSet<_>>();
            let mut actual = summarize(a);
            actual.union_with(summarize(b));
            assert_eq!(actual, summarize(&expected));
            assert_eq!(actual.is_empty(), expected.is_empty());
            assert_eq!(actual.has_multiple(), expected.len() > 1);
            assert_eq!(
                actual.may_be_uninitialized(),
                expected.contains(&Definition::Uninitialized)
            );
            assert_eq!(
                actual.has_initialized(),
                expected.iter().any(|d| *d != Definition::Uninitialized)
            );
        }
    }
}
