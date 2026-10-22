//! Corrupt internal bytecode must fail before an indirect entry is dispatched.
use super::*;

#[test]
fn borrowed_dyn_v4_interpreter_checks_table_slot_signature_and_function_id() {
    let ast = crate::parser::parse("struct Empty {} trait Read { fn value(&self) @noescape -> Int32 { 42 } } impl Read for Empty {} fn answer() -> Int32 { let e = Empty {}; let v: &dyn Read = &e; v.value() }").unwrap();
    let types = TypeRegistry::default().with_prelude(false);
    let prepared = PreparedModule::new(&ast, &types, &Default::default()).unwrap();
    let plan = prepared.plan(&HashMap::new()).unwrap();
    let names: BTreeSet<_> = plan.bodies.keys().cloned().collect();
    let ids = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i))
        .collect();
    for corruption in 0..5 {
        let mut module = compile(&prepared, &HashMap::new(), HashMap::new(), ModuleId(1)).unwrap();
        let mut function = lower_function(
            &plan.functions["answer"],
            &plan.bodies["answer"],
            &ids,
            &mut HashMap::new(),
            &mut Default::default(),
        )
        .unwrap();
        let mut changed = false;
        for instruction in &mut function.code {
            match instruction {
                Instruction::Const { value, .. }
                    if corruption == 1 && matches!(value, ConstantValue::DynTable(_)) =>
                {
                    *value = ConstantValue::UInt64(0);
                    changed = true;
                }
                Instruction::Const {
                    value: ConstantValue::DynTable(table),
                    ..
                } if corruption == 4 => {
                    *table = Rc::new(DynMethodTable {
                        entries: table
                            .entries
                            .iter()
                            .map(|(_, s)| (usize::MAX, s.clone()))
                            .collect(),
                    });
                    changed = true;
                }
                Instruction::DynMethod { method, .. } if corruption == 2 => {
                    *method = usize::MAX;
                    changed = true;
                }
                Instruction::DynMethod { signature, .. } if corruption == 3 => {
                    *signature = FunctionSignature::new(vec![], Type::Unit);
                    changed = true;
                }
                _ => {}
            }
        }
        assert_eq!(changed, corruption != 0);
        module.functions.insert("answer".into(), Rc::new(function));
        let result = call_sync(&module, "answer", &[], Default::default());
        if corruption == 0 {
            assert!(matches!(result.unwrap(), Value::Int32(42)));
        } else {
            assert_eq!(
                result.err().unwrap().code(),
                ErrorCode::InvalidCall,
                "corruption {corruption}"
            );
        }
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
