//! Engine-independent script convention and immutable interpreter snapshots.
use joky::{
    parser::{self, ast::Item},
    vm::{InterpreterLimits, VmEngine, VmMode},
    FunctionSignature, Type,
};
use std::collections::BTreeMap;

pub const TEMPLATE: &str = "struct State { count: Int32 }\n\nfn create() -> State { State { count: 0 } }\n\nfn _ready(state: &mut State) @noescape {\n    state.count = state.count + 1\n}\n\nfn count(state: &State) @noescape -> Int32 { state.count }\n";

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub path: String,
    pub line: i32,
    pub column: i32,
    pub message: String,
}
impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.path, self.line, self.column, self.message
        )
    }
}
impl Diagnostic {
    pub fn error(path: &str, source: &str, error: &joky::Error) -> Self {
        let file = error
            .span()
            .and_then(|s| error.sources().get(s.source_id()));
        let text = file.map_or(source, |f| f.text());
        let file_path = file
            .filter(|f| f.name() != "<entry>")
            .map_or(path, |f| f.name());
        let offset = error.span().map_or(0, |s| s.start()).min(text.len());
        let prefix = &text.as_bytes()[..offset];
        let line = prefix.iter().filter(|&&b| b == b'\n').count() + 1;
        let start = prefix
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |n| n + 1);
        Self {
            path: file_path.into(),
            line: error.line().unwrap_or(line) as i32,
            column: String::from_utf8_lossy(&prefix[start..]).chars().count() as i32 + 1,
            message: error.message().into(),
        }
    }
    fn contract(path: &str, source: &str, offset: usize, message: &str) -> Self {
        Self::error(
            path,
            source,
            &joky::Error::new(joky::ErrorPhase::TypeCheck, message)
                .with_span(joky::Span::new(offset, offset)),
        )
    }
}

pub struct Method {
    pub signature: FunctionSignature,
    pub parameters: Vec<String>,
    pub line: i32,
}
pub struct Program {
    pub vm: VmEngine,
    pub methods: BTreeMap<String, Method>,
    pub path: String,
    pub source: String,
}
impl Program {
    pub fn compile(path: &str, source: &str) -> Result<Self, Diagnostic> {
        let ast = parser::parse(source).map_err(|e| Diagnostic::error(path, source, &e.into()))?;
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.set_interpreter_limits(InterpreterLimits {
            max_frames: 256,
            instruction_budget: Some(100_000),
            poll_budget: None,
        })
        .expect("valid fixed limits");
        vm.load_module(&ast)
            .map_err(|e| Diagnostic::error(path, source, &e))?;
        let ctor = vm
            .instance_function_signature("create")
            .filter(|s| {
                s.params.is_empty()
                    && matches!(s.ret, Type::Record(_))
                    && s.ret.to_string() == "State"
            })
            .ok_or_else(|| {
                Diagnostic::contract(
                    path,
                    source,
                    0,
                    "Joky scripts require fn create() -> State returning a supported persistent record",
                )
            })?;
        let mut methods = BTreeMap::new();
        for item in &ast.items {
            let Item::FnDef(function) = item else {
                continue;
            };
            if function.name == "create" {
                continue;
            }
            let signature = vm.instance_function_signature(&function.name).filter(
                |s| matches!(s.params.first(), Some(Type::Reference(r)) if r.target == ctor.ret),
            );
            if function.name == "_ready"
                && !signature
                    .as_ref()
                    .is_some_and(|s| s.params.len() == 1 && s.ret == Type::Unit)
            {
                return Err(Diagnostic::contract(path, source, function.span.start(), "_ready requires a @noescape State receiver, no extra arguments, and a Unit result"));
            }
            if let Some(signature) = signature {
                let line = source.as_bytes()[..function.span.start()]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count() as i32
                    + 1;
                methods.insert(
                    function.name.clone(),
                    Method {
                        signature,
                        line,
                        parameters: function
                            .params
                            .iter()
                            .skip(1)
                            .map(|p| p.name.clone())
                            .collect(),
                    },
                );
            }
        }
        Ok(Self {
            vm,
            methods,
            path: path.into(),
            source: source.into(),
        })
    }
    pub fn diagnostic(&self, error: &joky::Error) -> Diagnostic {
        Diagnostic::error(&self.path, &self.source, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joky::vm::InstanceValue as V;
    #[test]
    fn script_convention_compiles_without_running_and_instances_are_independent() {
        let program =
            Program::compile("res://counter.jk", TEMPLATE).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(program.vm.instance_count(), 0);
        let a = program.vm.create_instance("create", &[]).unwrap();
        let b = program.vm.create_instance("create", &[]).unwrap();
        program.vm.call_instance(a, "_ready", &[]).unwrap();
        assert_eq!(
            program.vm.call_instance(a, "count", &[]).unwrap(),
            V::Int32(1)
        );
        assert_eq!(
            program.vm.call_instance(b, "count", &[]).unwrap(),
            V::Int32(0)
        );
        program.vm.close_instance(a).unwrap();
        program.vm.close_instance(b).unwrap();
        assert_eq!(program.vm.instance_bytes(), 0);
    }
    #[test]
    fn script_state_retains_p2_string_and_array_owners_between_calls() {
        let source = r#"
            import joky/array.{type Array}
            struct State { text: String values: Array<Int32> }
            fn create() -> State { State { text: "initial" values: Array.new::<Int32>() } }
            fn _ready(state: &mut State) @noescape {
                memory.replace(&mut state.text, "updated");
                state.values.push(42)
            }
            fn count(state: &State) @noescape -> UInt64 { state.values.len() }
        "#;
        let program = Program::compile("res://owners.jk", source).unwrap_or_else(|e| panic!("{e}"));
        let first = program.vm.create_instance("create", &[]).unwrap();
        let second = program.vm.create_instance("create", &[]).unwrap();
        program.vm.call_instance(first, "_ready", &[]).unwrap();
        assert_eq!(
            program.vm.call_instance(first, "count", &[]).unwrap(),
            V::UInt64(1)
        );
        assert_eq!(
            program.vm.call_instance(second, "count", &[]).unwrap(),
            V::UInt64(0)
        );
        program.vm.close_instance(first).unwrap();
        program.vm.close_instance(second).unwrap();
        assert_eq!(program.vm.instance_bytes(), 0);
        assert_eq!(program.vm.instance_peak_bytes(), 0);
    }
    #[test]
    fn diagnostics_preserve_path_and_line_and_reject_bad_contracts() {
        let bad = TEMPLATE.replace("state.count + 1", "state.count + true");
        let error = Program::compile("res://broken.jk", &bad).err().unwrap();
        assert_eq!(error.path, "res://broken.jk");
        assert_eq!(error.line, 6);
        assert!(Program::compile("x.jk", "fn create() -> Int32 { 1 }").is_err());
        assert!(Program::compile("x.jk", &TEMPLATE.replace("@noescape", "")).is_err());
        let failure = TEMPLATE.replace("state.count + 1", "1 / state.count");
        let program =
            Program::compile("res://failure.jk", &failure).unwrap_or_else(|e| panic!("{e}"));
        let id = program.vm.create_instance("create", &[]).unwrap();
        let error = program.vm.call_instance(id, "_ready", &[]).unwrap_err();
        assert_eq!(program.diagnostic(&error).line, 6);
    }
}
