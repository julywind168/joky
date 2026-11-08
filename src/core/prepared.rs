//! Immutable front-end inputs shared by VM and object compilation.
use super::{
    ir::ModulePlan,
    resolver::{self, BindingRegistry},
};
use crate::{parser::ast::Module, Error, TypeRegistry};
use std::collections::HashMap;

/// Standalone front-end checking with the standard host declarations.
/// No VM, executable code, runtime services, or entry point is created.
pub struct Checker {
    types: TypeRegistry,
}

impl Default for Checker {
    fn default() -> Self {
        Self::new()
    }
}

impl Checker {
    /// Create a checker with the same standard declarations as the CLI backends.
    pub fn new() -> Self {
        Self {
            types: crate::stdlib::module_registry().types().clone(),
        }
    }

    /// Configure implicit imports for subsequent source resolution.
    pub fn set_prelude_enabled(&mut self, enabled: bool) {
        self.types.set_prelude_enabled(enabled);
    }

    /// Resolve source files or in-memory modules with this checker's declarations.
    pub fn type_registry(&self) -> &TypeRegistry {
        &self.types
    }

    /// Check a resolved module, including unused functions and generic templates.
    /// This runs the shared semantic and ownership passes without code generation.
    /// Custom host registration and CLI entry-point policy are outside this API.
    pub fn check_module(&self, module: &Module) -> Result<(), Error> {
        let registry = crate::stdlib::module_registry();
        let prepared = PreparedModule::new(module, &self.types, registry.metadata())?;
        let imports = registry
            .import_layouts(prepared.ast(), &HashMap::new())
            .map_err(|error| error.with_sources(prepared.ast().sources()))?;
        prepared.plan(&imports).map(|_| ())
    }
}

/// Owns the normalized AST and declaration environment for one compilation.
/// Backend output retains sources and code owners, never this preparation state.
pub(crate) struct PreparedModule {
    ast: Module,
    types: TypeRegistry,
    bindings: BindingRegistry,
}

impl PreparedModule {
    pub(crate) fn new(
        module: &Module,
        types: &TypeRegistry,
        bindings: &BindingRegistry,
    ) -> Result<Self, Error> {
        let normalized = types.resolve_aliases(module)?;
        let ast = resolver::resolve(&normalized, bindings)
            .map_err(|error| error.with_sources(normalized.sources()))?;
        bindings
            .validate_declarations(&ast)
            .map_err(|error| error.with_sources(ast.sources()))?;
        Ok(Self {
            ast,
            types: types.clone(),
            bindings: bindings.clone(),
        })
    }

    pub(crate) fn ast(&self) -> &Module {
        &self.ast
    }

    pub(crate) fn types(&self) -> &TypeRegistry {
        &self.types
    }

    pub(crate) fn bindings(&self) -> &BindingRegistry {
        &self.bindings
    }

    /// Host binding must supply complete generated CPS layouts before planning.
    pub(crate) fn plan(&self, imports: &HashMap<String, usize>) -> Result<ModulePlan, Error> {
        ModulePlan::with_import_layouts_and_types(&self.ast, imports, &self.types)
            .map_err(|error| error.with_sources(self.ast.sources()))
    }
}

/// Check source without generating code. Execution and entry-point parity have
/// dedicated tests; static diagnostic and complexity-limit matrices use this path.
#[cfg(test)]
pub(crate) fn check_source(source: &str, modules: &crate::ModuleSources) -> Result<(), Error> {
    // Reuse parsed standard-library sources across cases, just as VM loading
    // does. Type resolution creates its own compilation scope for each source.
    thread_local! {
        static REGISTRY: crate::modules::ModuleRegistry = Default::default();
    }
    REGISTRY.with(|registry| {
        let ast = crate::parser::parse(source)?;
        let ast = registry.types().resolve_with_modules(&ast, modules)?;
        PreparedModule::new(&ast, registry.types(), registry.metadata())?
            .plan(&HashMap::new())
            .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backends::{interp, jit},
        parser::ast::Item,
        runtime::suspension::ModuleId,
        Type,
    };

    #[test]
    fn source_diagnostics_preserve_constructs_types_and_failure_causes() {
        use crate::ErrorCode::*;
        let mut modules = crate::ModuleSources::new();
        modules
            .insert(
                "app/lib",
                "pub struct P { pub n:Int32 } pub fn one()->Int32{1}",
            )
            .unwrap();
        modules
            .insert("foo", "pub struct Foreign { pub n:Int32 }")
            .unwrap();
        for (source, code, message, location) in [
            ("fn main(){let x:Int32=0..3}", TypeMismatch, "Result type mismatch: expected Int32, got joky/iter.Range<Int32>", "0..3"),
            ("fn main(){let a:Int64=0; let b:Int32=2; let x=a..b}", TypeMismatch, "Range endpoints must have the same type: start is Int64, end is Int32", "b"),
            ("fn main(){for x in 5 { println(x) }}", TypeMismatch, "Cannot iterate over Int32: for requires an IntoIterator implementation", "5"),
            ("const A:Bool=1 fn main(){}", TypeMismatch, "Type mismatch for constant `A`: expected Bool, got Int32", ""),
            ("const A:Int32=B\nconst B:Int32=A\nfn main(){}", InvalidState, "Cyclic module constant dependency involving `A`", ""),
            ("import joky/array.{type Array}\nfn f(a:&Array<Int32>) { a.borrow_ref(0).unwrap() } fn main(){}", UnknownName, "Type '&Int32' has no method 'unwrap'", ""),
            ("fn main(){let x=1s + 1}", TypeMismatch, "Argument type mismatch for `+`: expected joky/internal/time.Duration, got Int32", "1"),
            ("fn grow<T>(x:T,n:Int32) { if n > 0 { grow((x,x),n-1) } } fn main(){grow(1,1)}", ResourceLimit, "Tuple layout exceeds 65536 scalar slots", "(x,x)"),
            ("fn tag<T>(x:Option<Int32>)->Int32{1} fn main(){println(tag(None))}", TypeMismatch, "Cannot infer type parameter `T` for `tag`; use explicit generic arguments", "tag(None)"),
            ("fn id<T>(t:T)->T{t} trait Get{fn get(&self) @noescape -> Int32} fn f(s:&dyn Get){let v=id((s,s))} fn main(){}", TypeMismatch, "Dyn views cannot be stored in tuples", "(s,s)"),
            ("fn some<T>(x:T)->Option<T>{Some(x)} fn main(){let x:Int32=some(2)}", TypeMismatch, "Result type mismatch: expected Int32, got joky/option.Option<Int32>", "some(2)"),
            ("fn main() @suspends { let x = race { { 1 }, { true } } }", TypeMismatch, "Result type mismatch for race branch: expected Int32, got Bool", "{ true }"),
            ("fn f(x:&Int32) @suspends {join { { x }, {} }} fn main(){}", InvalidCall, "join branch results must be reference-free values, got &Int32", "{ x }"),
            ("fn main() @suspends { var n=3; join { { n=4 }, { } } }", InvalidAssignment, "Cannot assign to `n`: the join branch captures a snapshot of this binding", ""),
            ("fn main() @suspends { join { { let b=Some(1)?; b }, {} } }", TypeMismatch, "Postfix `?` needs a result type for the join branch matching the propagated joky/option.Option value", "Some(1)?"),
            ("trait Src { type I: Copy } fn main(){}", TypeMismatch, "Copy is a built-in parameter constraint; Copy bounds on associated types are not supported", ""),
            ("fn main(){let x=[0,..5]}", TypeMismatch, "Cannot spread Int32: expected an array or slice with element type Int32", ""),
            ("import joky/array.{type Array}\nfn main(){\n let p = { var a = Array.new::<Int32>(); a.push(2); a.borrow_ref(0) }\n println(*p)\n}", InvalidAssignment, "Borrowed value would outlive its owner at the end of this scope", ""),
            ("import joky/array.{type Array}\nimport joky/arena.{type Arena}\nfn main(){let a=Arena.new(); let p=a.alloc(1); var xs=Array.new::<&Int32>(); xs.push(p)}", InvalidAssignment, "A reference into an explicit arena cannot be retained beyond that arena's lifetime", ""),
            ("struct Item { s:String } fn take(v:Item){} fn main(){let v=Item{s:\"v\"};take(v);take(v)}", InvalidAssignment, "Use of a moved or uninitialized owning value", ""),
            ("fn main(){println!(3)}", InvalidCall, "Joky has no `println!` macro; call `println(...)`", "println!"),
            ("#tset struct S{} fn main(){}", UnknownName, "Unknown attribute #tset", "#tset"),
            ("import app/lib.{one as f,one as f} fn main(){}", DuplicateDefinition, "Imported function 'f' is already defined", ""),
            ("import app/lib.{type P}\nimpl P { fn v(self)->Int32 { self.n } }\nfn main(){}", TypeMismatch, "Cannot define inherent methods for `P`: the type is defined in another module", ""),
            ("import foo
impl foo.Foreign { fn v(self)->Int32 { self.n } } fn main(){}", TypeMismatch, "Cannot define inherent methods for `foo.Foreign`: the type is defined in another module", ""),
            ("trait Mk { fn mk(x:Int32)->Self }\nfn f<T:Mk>() -> T { Mk.mk::<T>() }\nfn main(){}", InvalidCall, "Argument count mismatch for `Mk.mk`", ""),
            ("trait A { fn x(self) }\ntrait B { fn y(self) }\nimpl<T:B> A for T { fn x(self) {} }\nimpl<T:A> B for T { fn y(self) {} }\nfn need<T:A>(t:T) {}\nfn main(){need(1)}", RecursiveType, "Cyclic trait or projection dependency: Int32: A -> Int32: B -> Int32: A", ""),
            ("trait Get {fn get(&self)->Int32} struct S{} impl Get for S {fn get(&self)->Bool{true}} fn main(){}", TypeMismatch, "Signature does not match trait method 'Get.get': expected return type Int32, got Bool", ""),
            ("trait Say {fn v(&self)->Int32} struct S{} impl<T> Say for T {fn v(&self)->Int32{1}} impl Say for S {fn v(&self)->Int32{2}} fn main(){let s=S{};println(s.v())}", TypeMismatch, "Overlapping trait implementations for `S` provide `v`; make their implementation bounds disjoint", ""),
        ] {
            let err = check_source(source, &modules).unwrap_err();
            assert_eq!(err.code(), code, "{source}: {err}");
            assert_eq!(err.message(), message, "{source}");
            let slice = err.span().and_then(|span|span.slice(source)).expect("diagnostic in source");
            if !location.is_empty() { assert_eq!(slice,location,"{source}"); }
        }
    }

    #[test]
    fn ownership_diagnostics_preserve_binding_and_move_locations() {
        for (source, declaration, moved, used, name, clone) in [
            ("fn main() {\n let s = \"hello\"\n let t = s\n println(s)\n println(t)\n}", 2, 3, 4, "s", true),
            ("fn take(s: String) {}\nfn f(s: String) {\n take(s)\n println(s)\n}", 2, 3, 4, "s", true),
            ("fn take(s: String) {}\nfn f(flag: Bool) {\n let s=\"hello\"\n if flag { take(s) }\n println(s)\n}", 3, 4, 5, "s", true),
            ("fn take(s: String) {}\nfn main() {\n let s=\"hello\"\n for i in 0..2 { take(s) }\n}", 3, 4, 4, "s", true),
            ("fn take(s: String) {}\nfn main() {\n var s=\"hello\"\n take(s)\n s=\"again\"\n take(s)\n println(s)\n}", 3, 6, 7, "s", true),
            ("fn take(s: String) {}\nfn main() {\n let s=\"outer\"\n { let s=\"inner\"\n take(s)\n println(s) }\n println(s)\n}", 4, 5, 6, "s", true),
            ("fn take(a: Array<Int32>) {}\nfn main() {\n let a=[42]\n take(a)\n println(a.len())\n}", 3, 4, 5, "a", false),
            ("struct Item { text: String }\nfn take(v: Item) {}\nfn main() {\n let v=Item {text:\"hello\"}\n take(v)\n take(v)\n}", 4, 5, 6, "v", false),
        ] {
            let error = check_source(source, &Default::default()).unwrap_err();
            assert_eq!(error.code(), crate::ErrorCode::InvalidAssignment);
            let labels = error.labels();
            assert_eq!(labels.len(), 3, "{source}: {error:?}");
            for (label, line) in labels.iter().zip([declaration, moved, used]) {
                assert_eq!(label.span.line_column(source).0, line, "{source}: {label:?}");
                assert!(label.message.contains(&format!("`{name}`")));
            }
            assert_eq!(error.span(), Some(labels[2].span));
            assert_eq!(error.help().iter().any(|help| help.contains(".clone()")), clone);
            assert!(error.help()[0].contains(&format!("`&{name}`")));
            if moved == used {
                assert!(labels[1].message.contains("previous loop iteration"));
            }
        }
    }

    #[test]
    fn preparation_snapshot_is_independent_of_later_source_edits() {
        let mut ast =
            crate::parser::parse("type Number = Int32 fn value() -> Number { 42 }").unwrap();
        let types = TypeRegistry::default();
        let bindings = BindingRegistry::default();
        let prepared = PreparedModule::new(&ast, &types, &bindings).unwrap();
        assert!(!ast.type_aliases.is_empty());
        assert!(prepared.ast().type_aliases.is_empty());
        let Item::FnDef(function) = ast.items.last_mut().unwrap() else {
            panic!("expected function")
        };
        function.ret_ty = Type::Bool;
        // A new preparation validates the caller's current AST; the previous
        // snapshot still describes a valid Int32 function for both backends.
        let changed = PreparedModule::new(&ast, &types, &bindings).unwrap();
        assert!(changed.plan(&HashMap::new()).is_err());
        let interpreted =
            interp::compile(&prepared, &HashMap::new(), HashMap::new(), ModuleId(1)).unwrap();
        assert!(matches!(
            interp::call_sync(&interpreted, "value", &[], Default::default()).unwrap(),
            interp::Value::Int32(42)
        ));
        let native = jit::compile(&prepared, &[], &HashMap::new(), vec![], ModuleId(2)).unwrap();
        let value: unsafe extern "C" fn() -> i32 =
            unsafe { std::mem::transmute(native.exports["value"].entry) };
        assert_eq!(unsafe { value() }, 42);
    }
}
