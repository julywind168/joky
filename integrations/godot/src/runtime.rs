//! Engine-independent script convention and immutable interpreter snapshots.
use joky::{
    parser::{self, ast::Item},
    vm::{InterpreterLimits, VmEngine, VmMode},
    FunctionSignature, Type,
};
use std::collections::BTreeMap;

pub const TEMPLATE: &str = "struct State { count: Int32 }\n\nfn create() -> State { State { count: 0 } }\n\nfn _ready(state: &mut State) {\n    state.count = state.count + 1\n}\n\nfn count(state: &State) -> Int32 { state.count }\n";

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
        let (line, column) = error
            .span()
            .unwrap_or(joky::Span::empty(0))
            .line_column(text);
        Self {
            path: file_path.into(),
            line: error.line().unwrap_or(line) as i32,
            column: column as i32,
            message: error.message().into(),
        }
    }
    pub(crate) fn contract(path: &str, source: &str, offset: usize, message: &str) -> Self {
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
    pub task: bool,
}
pub struct Program {
    pub vm: VmEngine,
    pub methods: BTreeMap<String, Method>,
    pub properties: BTreeMap<String, crate::property::Property>,
    pub bridge: std::rc::Rc<crate::node::Bridge>,
    pub path: String,
    pub source: String,
}
fn value_scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Unit
            | Type::Bool
            | Type::Rune
            | Type::Int8
            | Type::Int16
            | Type::Int32
            | Type::Int64
            | Type::UInt8
            | Type::UInt16
            | Type::UInt32
            | Type::UInt64
            | Type::Float32
            | Type::Float64
    )
}
impl Program {
    pub fn compile(path: &str, source: &str) -> Result<Self, Diagnostic> {
        let ast = parser::parse(source).map_err(|e| Diagnostic::error(path, source, &e.into()))?;
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.set_interpreter_limits(InterpreterLimits {
            max_frames: 256,
            instruction_budget: Some(100_000),
            poll_budget: Some(10_000),
            ..Default::default()
        })
        .expect("valid fixed limits");
        vm.configure_tasks(std::rc::Rc::new(joky::runtime::executor::SystemClock), 256)
            .expect("valid fixed task limits");
        crate::tasks::register(&mut vm).map_err(|e| Diagnostic::error(path, source, &e))?;
        crate::node::register(&mut vm).map_err(|e| Diagnostic::error(path, source, &e))?;
        let mut modules = joky::ModuleSources::new();
        modules
            .insert("godot/node", crate::node::SOURCE)
            .expect("valid module path");
        modules
            .insert("godot/task", crate::tasks::SOURCE)
            .expect("valid module path");
        vm.load_source_with_modules(source, &modules)
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
            if function.name.starts_with("task_") {
                let signature = vm
                    .function_signature(&function.name)
                    .filter(|s| {
                        function.suspends
                            && function.type_params.is_empty()
                            && s.ret == Type::Unit
                            && s.params.iter().all(value_scalar)
                    })
                    .ok_or_else(|| {
                        Diagnostic::contract(
                            path,
                            source,
                            function.span.start(),
                            "task_ entries require @suspends, scalar inputs, and a Unit result",
                        )
                    })?;
                methods.insert(
                    function.name.clone(),
                    Method {
                        signature,
                        parameters: function.params.iter().map(|p| p.name.clone()).collect(),
                        line: function.span.line_column(source).0 as i32,
                        task: true,
                    },
                );
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
                return Err(Diagnostic::contract(path, source, function.span.start(), "_ready requires a nonescaping State receiver, no extra arguments, and a Unit result"));
            }
            if matches!(function.name.as_str(), "_process" | "_physics_process")
                && !signature.as_ref().is_some_and(|s| {
                    s.params.len() == 2 && s.params[1] == Type::Float64 && s.ret == Type::Unit
                })
            {
                return Err(Diagnostic::contract(path, source, function.span.start(), "Frame callbacks require a nonescaping State receiver, one Float64 delta argument, and a Unit result"));
            }
            if let Some(signature) = signature {
                let line = function.span.line_column(source).0 as i32;
                methods.insert(
                    function.name.clone(),
                    Method {
                        signature,
                        task: false,
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
        let properties = crate::property::collect(path, source, &ast.items, &vm, &methods)?;
        Ok(Self {
            properties,
            bridge: Default::default(),
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
    fn embedded_scripts_keep_filesystem_capability_unavailable() {
        let source = r#"import joky/fs
            struct State { value: Int32 }
            fn create() -> State { State { value: 0 } }
            fn unsupported(path: String) @suspends -> Bool {
                match fs.open(path, fs.OpenOptions.new()) {
                    Err(error) => match error.kind { fs.ErrorKind.Unsupported => true, _ => false }
                    Ok(file) => { let ignored = file.close(); false }
                }
            }
            fn probe() @suspends -> Int32 {
                if !unsupported("res://unavailable.txt") { return 1 }
                if !unsupported("user://unavailable.txt") { return 2 }
                if !unsupported("unavailable.txt") { return 3 }
                42
            }
        "#;
        let program =
            Program::compile("res://embedded.jk", source).unwrap_or_else(|error| panic!("{error}"));
        let task = program.vm.prepare_async("probe", &[]).unwrap();
        assert_eq!(joky::runtime::driver::block_on(task).unwrap(), 42);
    }

    #[test]
    fn borrowed_dyn_views_stay_inside_checked_instance_methods() {
        let source = include_str!("../demo/borrowed_dyn.jk");
        let program =
            Program::compile("res://borrowed_dyn.jk", source).unwrap_or_else(|e| panic!("{e}"));
        assert!(!program.methods.contains_key("inspect"));
        assert!(!program.methods.contains_key("update"));
        let id = program.vm.create_instance("create", &[]).unwrap();
        let bytes = program.vm.instance_bytes();
        assert_eq!(
            program.vm.call_instance(id, "answer", &[]).unwrap(),
            V::Int32(42)
        );
        assert_eq!(
            program
                .vm
                .call_instance(id, "change", &[V::Int32(22)])
                .unwrap(),
            V::Int32(44)
        );
        assert_eq!(program.vm.instance_bytes(), bytes);
        program.vm.close_instance(id).unwrap();
        assert_eq!(program.vm.instance_bytes(), 0);
        assert_eq!(program.vm.instance_peak_bytes(), 0);
    }
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
        let source = r#"import joky/mem

            import joky/array.{type Array}
            struct State { text: String, values: Array<Int32> }
            fn create() -> State { State { text: "initial", values: Array.new::<Int32>() } }
            fn _ready(state: &mut State) {
                mem.replace(&mut state.text, "updated");
                state.values.push(42)
            }
            fn count(state: &State) -> UInt64 { state.values.len() }
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
        let escaping = format!(
            "struct Saved {{ state: &State }}\n{}",
            TEMPLATE.replace(
                "state.count = state.count + 1",
                "let saved = alloc(Saved { state: state })"
            )
        );
        assert!(Program::compile("x.jk", &escaping).is_err());
        let failure = TEMPLATE.replace("state.count + 1", "1 / state.count");
        let program =
            Program::compile("res://failure.jk", &failure).unwrap_or_else(|e| panic!("{e}"));
        let id = program.vm.create_instance("create", &[]).unwrap();
        let error = program.vm.call_instance(id, "_ready", &[]).unwrap_err();
        assert_eq!(program.diagnostic(&error).line, 6);
    }
    #[test]
    fn g1_metadata_is_static_and_frame_and_property_contracts_are_checked() {
        let source = include_str!("../demo/mover.jk");
        let program = Program::compile("res://mover.jk", source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(program.vm.instance_count(), 0);
        assert_eq!(program.properties["speed"].default, V::Float64(60.0));
        assert!(program.methods.contains_key("label_size"));
        assert_eq!(
            program.methods["_process"].signature.params[1],
            Type::Float64
        );
        for bad in [
            source.replace("60.0 }", "1.0 / 0.0 }"),
            source.replace("delta: Float64", "delta: Float32"),
            source.replace("state.speed }", "state.speed + 1.0 }"),
            source.replace("fn set_speed", "fn not_set_speed"),
        ] {
            assert!(Program::compile("res://bad.jk", &bad).is_err());
        }
        let id = program.vm.create_instance("create", &[]).unwrap();
        program
            .vm
            .call_instance(id, "set_speed", &[V::Float64(12.5)])
            .unwrap();
        assert_eq!(
            program.vm.call_instance(id, "get_speed", &[]).unwrap(),
            V::Float64(12.5)
        );
        program.vm.close_instance(id).unwrap();
        assert_eq!(program.vm.instance_peak_bytes(), 0);
    }
    #[test]
    fn json3_task_local_values_compile_with_scalar_state_exports() {
        let program = Program::compile(
            "res://network_json.jk",
            include_str!("../demo/network_json.jk"),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(program.methods["task_session"].task);
        assert!(program.methods["task_pressure"].task);
        assert_eq!(
            program.methods["task_session"].signature.params,
            vec![Type::Int32, Type::Int32]
        );
        assert_eq!(program.vm.instance_task_count(), 0);
    }
    #[test]
    fn g2_task_exports_compile_without_borrowed_receivers_or_execution() {
        let source = include_str!("../demo/async.jk");
        let program = Program::compile("res://async.jk", source).unwrap_or_else(|e| panic!("{e}"));
        assert!(Program::compile(
            "res://async_mover.jk",
            include_str!("../demo/async_mover.jk")
        )
        .is_ok());
        assert!(program.methods["task_game"].task);
        assert_eq!(
            program.methods["task_game"].signature.params,
            vec![Type::Float64]
        );
        assert_eq!(program.vm.instance_task_count(), 0);
        for bad in [
            source.replace(
                "fn task_frames() @suspends",
                "fn task_frames(state: &mut State) @suspends",
            ),
            source.replace(
                "fn task_frames() @suspends",
                "fn task_frames() @suspends -> Int32",
            ),
        ] {
            assert!(Program::compile("res://bad.jk", &bad).is_err());
        }
    }
    #[test]
    fn g1_literal_defaults_preserve_scalar_ranges_and_float_bits() {
        for (ty, literal, expected) in [
            ("Int8", "-128", V::Int8(-128)),
            ("Int64", "-9223372036854775808", V::Int64(i64::MIN)),
            ("UInt64", "9223372036854775807", V::UInt64(i64::MAX as u64)),
            ("Float32", "-0.0", V::Float32(-0.0)),
            ("Float64", "-0.0", V::Float64(-0.0)),
            ("Bool", "true", V::Bool(true)),
            ("Rune", "'界'", V::Rune('界')),
        ] {
            let source = format!("struct State {{ value: {ty} }} fn create() -> State {{ State {{ value: {literal} }} }} fn default_value() -> {ty} {{ {literal} }} fn get_value(s: &State) -> {ty} {{ s.value }} fn set_value(s: &mut State, value: {ty}) {{ s.value = value }}");
            let program =
                Program::compile("res://defaults.jk", &source).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(program.vm.instance_count(), 0);
            let actual = program.properties["value"].default;
            match (actual, expected) {
                (V::Float32(a), V::Float32(b)) => assert_eq!(a.to_bits(), b.to_bits()),
                (V::Float64(a), V::Float64(b)) => assert_eq!(a.to_bits(), b.to_bits()),
                (a, b) => assert_eq!(a, b),
            }
            if ty == "UInt64" {
                assert!(Program::compile(
                    "x",
                    &source.replace("9223372036854775807", "18446744073709551615")
                )
                .is_err());
            }
        }
        let source = include_str!("../demo/mover.jk");
        let hidden = source.replace("target: node.none()", "target: Node { token: 1 }");
        assert!(Program::compile("res://forged.jk", &hidden).is_err());
    }
}
