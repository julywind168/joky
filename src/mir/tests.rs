use super::*;

use super::verifier::*;
use crate::{sema, syntax};
use std::collections::HashMap;
fn lower(source: &str) -> MirProgram {
    let program = syntax::parse_program(source).unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    MirProgram::lower(&core).unwrap()
}

fn verify_one(mir: &MirProgram, index: usize) -> Result<(), Diagnostic> {
    let mut signatures = HashMap::new();
    for function in &mir.functions {
        signatures.insert(
            function.id,
            MirSignature {
                foreign: false,
                receiver_ownership: function
                    .receiver_local
                    .map(|local| function.locals[local.0].ownership),
                parameter_ownership: function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ownership)
                    .collect(),
                parameter_types: function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ty)
                    .collect(),
                return_type: function.return_type,
            },
        );
    }
    verify_function(&mir.functions[index], mir.types(), &signatures)
}

fn owned_pair_mir() -> (MirProgram, usize) {
    let mir = lower(
        "class Token {}\n\
         struct Pair { let left: Token; let right: Token }\n\
         fn take_left(pair: Pair) -> Token { pair.left }\n\
         fn main() {}",
    );
    let index = mir
        .functions
        .iter()
        .position(|function| function.name == "take_left")
        .expect("expected take_left function");
    (mir, index)
}

mod continuations;
mod expressions;
mod lowering;
mod verification;

#[test]
fn source_spans_survive_optimization_and_continuation_splitting() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\nfn work() effects { time } {\n    time.sleep(1ms)\n    println(42)\n}\nfn main() effects { time } { work() }";
    let mut mir = lower(source);
    crate::mir::passes::MirPassManager::default_pipeline()
        .run(&mut mir)
        .unwrap();
    let work = mir.functions.iter().find(|f| f.name == "work").unwrap();
    let mut found = false;
    assert!(!work.continuations.is_empty());
    for block in &work.blocks {
        for statement in &block.statements {
            if let Some(span) = work.statement_span(statement) {
                found |= &source[span.start()..span.end()] == "println(42)";
            }
        }
    }
    assert!(
        found,
        "resumed instructions retain the original expression span"
    );
    let encoded = bincode::serialize(&mir).unwrap();
    let restored: MirProgram = bincode::deserialize(&encoded).unwrap();
    assert_eq!(
        restored.functions[work.id.0].source.values,
        work.source.values
    );
}
