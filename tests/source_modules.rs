//! File-independent embedding contract for user source modules.
use joky::vm::{VmEngine, VmMode};
use joky::{ErrorCode, ModuleSources, SourceFile};

fn sources(entries: &[(&str, &str)]) -> ModuleSources {
    let mut modules = ModuleSources::new();
    for (path, source) in entries {
        modules.insert(*path, *source).unwrap();
    }
    modules
}

#[test]
fn generic_impls_keep_definition_scope_through_diamond_imports() {
    for effect in ["", "@suspends"] {
        let base = format!(
            r#"
            pub struct Pair<T> {{ left: T, right: T }}
            pub enum Choice<T> {{ None, Some(T) }}
            fn helper<T>(value: T) {effect} -> T {{ value }}
            fn wrap<T>(value: T) {effect} -> Choice<T> {{ Choice.Some(value) }}
            impl<T> Pair<T> {{
                const BASE: Int32 = 1
                pub const SIZE: Int32 = Self.BASE + 1
            }}
            impl<T: Copy> Pair<T> {{
                pub fn make(left: T, right: T) {effect} -> Self {{ Self {{ left: helper(left), right: right }} }}
                fn private(self) {effect} -> T {{ self.left }}
                pub fn first(self) {effect} -> T {{ self.private() }}
                pub fn swap(self) {effect} -> Self {{ Self.make(self.right, self.left) }}
                fn private_zip<U: Copy>(self, value: U) {effect} -> (T, U) {{
                    let wrapped: Choice<U> = wrap(helper(value))
                    let p = Self.make(self.private(), self.right)
                    (p.first(), wrapped.unwrap(value))
                }}
                pub fn zip<U: Copy>(self, value: U) {effect} -> (T, U) {{ self.private_zip::<U>(value) }}
            }}
            impl<T> Choice<T> {{
                pub fn some(value: T) {effect} -> Self {{ Self.Some(value) }}
                pub fn unwrap(self, fallback: T) {effect} -> T {{ match self {{ None => fallback, Some(value) => value }} }}
            }}
        "#
        );
        let left = format!("import app/base as b.{{type Pair as Renamed}} pub fn make() {effect} -> Renamed<Int64> {{ Renamed.make(int64(21), 0) }}");
        let right = format!("import app/base as b pub fn make() {effect} -> b.Pair<Int64> {{ b.Pair::<Int64>.make(0, 21).swap() }}");
        let modules = sources(&[
            ("app/base", &base),
            ("app/left", &left),
            ("app/right", &right),
            ("app/payload", "pub struct Payload { pub valid: Bool }"),
        ]);
        let source = format!(
            r#"
            import app/base as b.{{type Pair as Saved, type Choice as Renamed}}
            import app/left as l
            import app/right as r
            import app/payload as p.{{type Payload as Foreign}}
            struct Pair<T> {{ wrong: T }}
            fn helper<T>(value: T) {effect} -> Bool {{ false }}
            fn answer() {effect} -> Int32 {{
                let x: Saved<Int64> = l.make()
                let y = r.make()
                let wrapped = Renamed.some(x.first() + y.first())
                let typed = b.Choice::<Int64>.Some(4294967295).unwrap(0)
                let empty = b.Choice::<Int64>.None.unwrap(4294967295)
                let grouped = (b.Pair::<Int64>).make(4294967295, 0).first()
                let (a, payload) = x.zip(Foreign {{ valid: true }})
                let (c, d) = Saved.zip::<Int64, p.Payload>(y, payload)
                let (e, f) = b.Pair::<Int64>.zip::<Foreign>(x, d)
                if Saved.SIZE == 2 && b.Pair::<Bool>.SIZE == 2 && typed == empty && grouped == typed &&
                    a == 21 && c == a && e == a && f.valid {{ int32(wrapped.unwrap(0)) }} else {{ 0 }}
            }}
        "#
        );
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source_with_modules(&source, &modules).unwrap();
            let result = if effect.is_empty() {
                vm.call_int32_0("answer").unwrap()
            } else {
                vm.call_async("answer", &[]).unwrap().cps_result().unwrap() as i32
            };
            assert_eq!(result, 42);
        }
    }
}

#[test]
fn generic_impl_imports_enforce_visibility_identity_and_template_errors() {
    let modules = sources(&[
        ("a", "pub struct Pair<T> { left: T, right: T } impl<T> Pair<T> { const SECRET: Int32 = 42 pub const SIZE: Int32 = Self.SECRET fn private(self) -> T { self.left } pub fn first(self) -> T { self.private() } pub fn make(left: T, right: T) -> Self { Self { left: left, right: right } } fn private_zip<U>(self, value: U) -> (T, U) { (self.left, value) } pub fn zip<U>(self, value: U) -> (T, U) { self.private_zip(value) } }"),
        ("b", "pub struct Pair<T> { left: T, right: T }"),
    ]);
    for source in [
        "import a fn bad(p: a.Pair<Int32>) { p.private() }",
        "import a fn bad() { a.Pair.SECRET }",
        "import a fn bad() { a.Pair::<Int32>.SECRET }",
        "import a fn bad() { a.Pair.private::<Int32>(a.Pair.make(1, 2)) }",
        "import a import b fn bad(p: b.Pair<Int32>) { p.first() }",
        "import a import b fn bad(p: b.Pair<Int32>) { a.Pair.first(p) }",
        "import a fn bad(p: a.Pair<Int64>) { a.Pair.first::<Bool>(p) }",
        "import a fn bad(p: a.Pair<Int64>) { p.private_zip::<Bool>(true) }",
        "import a fn bad(p: a.Pair<Int64>) { a.Pair.private_zip::<Int64, Bool>(p, true) }",
        "import a fn bad(p: a.Pair<Int64>) { a.Pair::<Int64>.private_zip::<Bool>(p, true) }",
        "import a import b fn bad(p: b.Pair<Int32>) { a.Pair.zip::<Int32, Bool>(p, true) }",
        "import a import b fn bad(p: a.Pair<Int32>, v: b.Pair<Bool>) { p.zip::<a.Pair<Bool>>(v) }",
        "import a fn bad() { let a = 1; a.Pair.make(1, 2) }",
        "import a.{type Pair as Saved} fn bad() { let Saved = 1; Saved.make(1, 2) }",
        "import a impl<T> a.Pair<T> { fn first(self) -> T { self.left } }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn current() -> Int32 { 42 }").unwrap();
            assert!(
                vm.load_source_with_modules(source, &modules).is_err(),
                "{source}"
            );
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
    }
    let invalid = "// 🙂\npub struct Pair<T> { value: T } impl<T> Pair<T> { pub fn bad(self) -> T { self.value + self.value } }";
    let error = VmEngine::new_interpreter()
        .load_source_with_modules(
            "import bad fn answer() -> Int32 { 42 }",
            &sources(&[("bad", invalid)]),
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::TypeMismatch);
    let span = error.span().unwrap();
    let file = error.sources().get(span.source_id()).unwrap();
    assert!(file.name().ends_with("bad.jk"));
    assert_eq!(span.slice(file.text()), Some("self.value + self.value"));
}

#[test]
fn generic_aliases_keep_definition_scope_through_diamond_imports_and_renaming() {
    for effect in ["", "@suspends"] {
        let base = format!(
            r#"
            type Local<T> = (T, T)
            struct Hidden<T> {{ value: Local<T> }}
            pub type Value<T> = Hidden<T>
            pub fn make<T: Copy>(value: T) {effect} -> Value<T> {{ Hidden {{ value: (value, value) }} }}
            pub fn first<T>(value: Value<T>) {effect} -> T {{ let (a, b) = value.value; a }}
        "#
        );
        let left = format!(
            r#"
            import app/base as b.{{type Value as Imported}}
            pub type Value<T> = Imported<T>
            pub struct Box<T> {{ value: b.Value<T> }}
            pub enum Choice<T> {{ Some(b.Value<T>), None }}
            pub fn make() {effect} -> Value<Int64> {{
                let boxed = Box {{ value: b.make(int64(21)) }}
                match Choice.Some(boxed.value) {{ Some(value) => value, None => b.make(int64(0)) }}
            }}
        "#
        );
        let right = format!("import app/base as b pub type Value<T> = b.Value<T> pub fn make() {effect} -> Value<Int64> {{ b.make(int64(21)) }}");
        let modules = sources(&[
            ("app/base", &base),
            ("app/left", &left),
            ("app/right", &right),
        ]);
        let source = format!(
            r#"
            import app/base as b.{{type Value as Saved, first}}
            import app/left as l.{{type Value as Renamed}}
            import app/right as r
            struct Hidden<T> {{ wrong: T }}
            type Local<T> = Bool
            fn answer() {effect} -> Int32 {{
                let a: Saved<Int64> = l.make()
                let c: Renamed<Int64> = r.make()
                int32(first(a) + b.first(c))
            }}
        "#
        );
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source_with_modules(&source, &modules).unwrap();
            let answer = if effect.is_empty() {
                vm.call_int32_0("answer").unwrap()
            } else {
                vm.call_async("answer", &[]).unwrap().cps_result().unwrap() as i32
            };
            assert_eq!(answer, 42);
        }
    }
}

#[test]
fn generic_alias_imports_reject_private_names_constructors_and_foreign_nominals() {
    let modules = sources(&[
        ("a", "struct Hidden<T> { value: T } type Private<T> = T pub type Value<T> = Hidden<T> pub type Maybe<T> = Option<T>"),
        ("b", "struct Hidden<T> { value: T } pub type Value<T> = Hidden<T>"),
    ]);
    for source in [
        "import a import b fn bad(x: a.Value<Int32>) -> b.Value<Int32> { x }",
        "import a fn bad(x: a.Value<Int32>) -> a.Value<Bool> { x }",
        "import a.{type Private} fn bad() {}",
        "import a fn bad(x: a.Private<Int32>) {}",
        "import a fn bad() { let x = a.Value::<Int32> { value: 1 } }",
        "import a.{type Value as Alias} fn bad() { let x = Alias::<Int32> { value: 1 } }",
        "import a fn bad() { a.Maybe.Some::<Int32>(1) }",
        "import a fn bad(x: a.Value) {}",
        "import a fn bad(x: a.Value<Int32, Bool>) {}",
        "import a fn bad(x: a.Value<vm.Function>) {}",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(
                VmEngine::with_mode(mode)
                    .load_source_with_modules(source, &modules)
                    .is_err(),
                "{source}"
            );
        }
    }
    for target in ["Missing", "Loop<T>"] {
        let module = format!("// 🙂\npub type Loop<T> = {target}");
        let modules = sources(&[("bad", &module)]);
        let error = VmEngine::new_interpreter()
            .load_source_with_modules("import bad fn answer() -> Int32 { 42 }", &modules)
            .unwrap_err();
        let span = error.span().unwrap();
        let file = error.sources().get(span.source_id()).unwrap();
        assert!(file.name().ends_with("bad.jk"));
        assert_eq!(span.slice(file.text()), Some(target));
    }
}

#[test]
fn generic_records_keep_identity_and_definition_scope_across_diamond_imports() {
    for effect in ["", "@suspends"] {
        let base = format!(
            r#"
            pub struct Pair<T> {{ pub left: T, pub right: T }}
            struct Hidden<T> {{ value: T }}
            pub type Numbers = Pair<Int64>
            pub fn make<T: Copy>(value: T) {effect} -> Pair<T> {{
                let hidden = Hidden::<T> {{ value: value }}
                Pair {{ left: hidden.value, right: value }}
            }}
            pub fn first<T>(p: Pair<T>) {effect} -> T {{ p.left }}
        "#
        );
        let left = format!("import app/base.{{type Pair, make}} pub fn left() {effect} -> Pair<Int64> {{ make(int64(21)) }}");
        let right = format!("import app/base as b pub fn right() {effect} -> b.Pair<Int64> {{ b.Pair::<Int64> {{ left: 21, right: 0 }} }}");
        let modules = sources(&[
            ("app/base", &base),
            ("app/left", &left),
            ("app/right", &right),
        ]);
        let source = format!(
            r#"
            import app/base as b.{{type Pair as Duo, type Numbers, first}}
            import app/left as l
            import app/right as r
            struct Hidden<T> {{ wrong: T }}
            fn answer() {effect} -> Int32 {{
                let a: Numbers = l.left()
                var c: Duo<Int64> = r.right()
                c.right = b.Pair::<Int64> {{ left: 0, right: 1 }}.right
                int32(first(a) + first(c))
            }}
        "#
        );
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source_with_modules(&source, &modules).unwrap();
            let value = if effect.is_empty() {
                vm.call_int32_0("answer").unwrap()
            } else {
                vm.call_async("answer", &[]).unwrap().cps_result().unwrap() as i32
            };
            assert_eq!(value, 42);
        }
    }
}

#[test]
fn generic_record_imports_reject_foreign_identity_and_private_constructors() {
    let modules = sources(&[
        ("a", "pub struct Pair<T> { value: T } struct Hidden<T> { value: T } pub type Alias = Pair<Int32>"),
        ("b", "pub struct Pair<T> { value: T }"),
    ]);
    for source in [
        "import a import b fn bad(p: a.Pair<Int32>) -> b.Pair<Int32> { p }",
        "import a fn bad() { let p: a.Pair<Int64> = a.Pair::<Int32> { value: 1 } }",
        "import a fn bad() { let p = a.Hidden::<Int32> { value: 1 } }",
        "import a.{type Hidden} fn bad() {}",
        "import a fn bad() { let p = a.Alias { value: 1 } }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(
                VmEngine::with_mode(mode)
                    .load_source_with_modules(source, &modules)
                    .is_err(),
                "{source}"
            );
        }
    }
}

fn check(source: &str, modules: &ModuleSources, expected: i32) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        assert_eq!(
            vm.load_source_with_modules(source, modules).unwrap(),
            ["answer"]
        );
        assert_eq!(vm.call_int32_0("answer").unwrap(), expected);
    }
}

#[test]
fn diamond_dependencies_keep_identity_generics_and_definition_scopes() {
    let modules = sources(&[
        (
            "app/value",
            r#"
            pub enum Choice { Empty, Value(Int32) }
            pub fn identity<T>(value: T) -> T { value }
            fn secret(n: Int32) -> Int32 { n + 1 }
            pub fn make(n: Int32) -> Choice { Value(secret(identity(n))) }
        "#,
        ),
        (
            "app/left",
            r#"
            import app/value as values.{type Choice as Answer, make}
            pub fn left() -> Answer { make(20) }
        "#,
        ),
        (
            "app/right",
            r#"
            import app/value.{type Choice, Value}
            pub fn right() -> Choice { Value(21) }
        "#,
        ),
    ]);
    check(
        r#"
        import app/left as l.{left}
        import app/right as r
        import app/value.{type Choice as Answer, Value as Present}
        fn answer() -> Int32 {
            let a: Answer = left()
            let b = r.right()
            let x = match a { Present(n) => n, Answer.Empty => 0 }
            let y = match b { Present(n) => n, Answer.Empty => 0 }
            x + y
        }
    "#,
        &modules,
        42,
    );
}

#[test]
fn public_aliases_records_and_members_use_private_helpers() {
    let modules = sources(&[(
        "app/model",
        r#"
        pub type Number = Int32
        pub type Maybe = Option<Int32>
        pub struct Pair { a: Number, b: Int32 }
        pub type Numbers = Pair
        impl Pair {
            const OFFSET: Int32 = 1
            fn secret(self) -> Int32 { self.a + self.b }
            pub fn total(self) -> Int32 { self.secret() + Pair.OFFSET }
            pub const BASE: Int32 = 20
        }
        pub fn pair(a: Number) -> Pair { Pair { a: a, b: 21 } }
        pub fn some(n: Int32) -> Maybe { Some(n) }
    "#,
    )]);
    check(
        r#"
        import app/model as m.{type Number, type Pair, type Numbers, type Maybe}
        fn answer() -> Int32 {
            let n: Number = Pair.BASE
            let p: Numbers = m.pair(n)
            let value: Maybe = m.some(p.total())
            match value { Some(n) => n, None => 0 }
        }
    "#,
        &modules,
        42,
    );
}

#[test]
fn private_declarations_and_members_do_not_escape_their_module() {
    let modules = sources(&[(
        "model",
        r#"
        enum Hidden { Secret(Int32) }
        struct HiddenRecord { value: Int32 }
        type HiddenAlias = Int32
        fn helper() -> Int32 { 42 }
        pub struct Public { value: Int32 }
        impl Public {
            fn hidden(self) -> Int32 { helper() }
            const SECRET: Int32 = 42
            pub fn value(self) -> Int32 { self.hidden() }
        }
        pub fn make() -> Public { Public { value: helper() } }
    "#,
    )]);
    for source in [
        "import model.{helper} fn answer() -> Int32 { helper() }",
        "import model.{type Hidden} fn answer() -> Int32 { 0 }",
        "import model.{Secret} fn answer() -> Int32 { 0 }",
        "import model.{type HiddenAlias} fn answer() -> Int32 { 0 }",
        "import model fn answer() -> Int32 { model.helper() }",
        "import model fn answer() -> Int32 { model.make().hidden() }",
        "import model fn answer() -> Int32 { model.Public.hidden(model.make()) }",
        "import model as m fn answer() -> Int32 { model.Public.value(m.make()) }",
        "import model.{type Public} fn answer() -> Int32 { Public.SECRET }",
        "import model fn answer() -> Int32 { let x: model.Hidden = model.Hidden.Secret(42); 0 }",
        "import model fn answer() -> Int32 { let x = model.Hidden.Secret(42); 0 }",
        "import model fn answer() -> Int32 { let x = model.HiddenRecord { value: 42 }; x.value }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            assert!(
                vm.load_source_with_modules(source, &modules).is_err(),
                "accepted: {source}"
            );
        }
    }
    check(
        "import model fn answer() -> Int32 { model.make().value() }",
        &modules,
        42,
    );
}

#[test]
fn module_qualified_members_and_public_aliases_preserve_type_visibility() {
    let modules = sources(&[(
        "model",
        r#"
        pub struct Pair { a: Int32 }
        pub type Alias = Pair
        impl Pair {
            pub const VALUE: Int32 = 42
            pub fn make() -> Pair { Pair { a: Pair.VALUE } }
            pub fn read(self) -> Int32 { self.a }
        }
    "#,
    )]);
    check(
        "import model as m fn answer() -> Int32 { m.Pair.read(m.Pair.make()) }",
        &modules,
        42,
    );
    check(
        "import model as m fn answer() -> Int32 { m.Pair.VALUE }",
        &modules,
        42,
    );
    check("import model as m impl Int32 { const COPIED: Int32 = m.Pair.VALUE } fn answer() -> Int32 { Int32.COPIED }", &modules, 42);
    for source in [
        "import model as m fn answer() -> Int32 { let p = m.Alias { a: 42 }; p.a }",
        "import model as m fn answer() -> Int32 { m.Alias.VALUE }",
    ] {
        assert!(VmEngine::new_interpreter()
            .load_source_with_modules(source, &modules)
            .is_err());
    }
}

#[test]
fn host_imports_keep_the_defining_modules_alias_and_binding() {
    let modules = sources(&[(
        "app/engine",
        r#"
        import joky/vm as host
        pub fn value() -> Int32 {
            let h = host.Vm.interpreter()
            { let vm_text = "fn answer() -> Int32 {{ 42 }}"; host.load_source(h, &vm_text) }
            let result = { let vm_text = "answer"; host.call_int32_0(h, &vm_text) }
            host.close(h)
            result
        }
    "#,
    )]);
    check(
        "import app/engine fn answer() -> Int32 { engine.value() }",
        &modules,
        42,
    );
    check(
        "import app/engine import joky/vm fn answer() -> Int32 { engine.value() }",
        &modules,
        42,
    );
    assert!(VmEngine::new_interpreter()
        .load_source_with_modules(
            "import app/engine fn answer() -> Int32 { vm.error_code() }",
            &modules
        )
        .is_err());
}

#[test]
fn modules_cannot_capture_caller_declarations_or_private_dependency_members() {
    let modules = sources(&[
        ("app/base", "pub struct Value {} impl Value { fn hidden(self) -> Int32 { 42 } } pub fn make() -> Value { Value {} }"),
        ("app/consumer", "import app/base pub fn read() -> Int32 { base.make().hidden() }"),
    ]);
    assert!(VmEngine::new_interpreter()
        .load_source_with_modules("import app/consumer", &modules)
        .is_err());
    for body in ["caller()", "(42).caller()", "Int32.CALLER"] {
        let modules = sources(&[(
            "app/isolated",
            &format!("pub fn read() -> Int32 {{ {body} }}"),
        )]);
        assert!(VmEngine::new_interpreter().load_source_with_modules("import app/isolated fn caller() -> Int32 { 42 } impl Int32 { const CALLER: Int32 = 42 fn caller(self) -> Int32 { self } }", &modules).is_err(), "{body}");
    }
}

#[test]
fn module_depth_is_bounded_before_recursive_expansion() {
    let mut modules = ModuleSources::new();
    for index in 0..130 {
        modules
            .insert(
                format!("app/m{index}"),
                format!("import app/m{}", index + 1),
            )
            .unwrap();
    }
    let error = VmEngine::new_interpreter()
        .load_source_with_modules("import app/m0", &modules)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("128"));
    assert!(error
        .sources()
        .get(error.span().unwrap().source_id())
        .is_some());
}

#[test]
fn imported_generic_suspending_calls_work_in_both_vm_modes() {
    let modules = sources(&[("app/math", "fn identity<T>(value: T) @suspends -> T { value } pub fn value(n: Int32) @suspends -> Int32 { identity(n + 1) }")]);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(
            "import app/math fn answer() @suspends -> Int32 { math.value(41) }",
            &modules,
        )
        .unwrap();
        assert_eq!(vm.call_async("answer", &[]).unwrap().cps_result(), Some(42));
    }
}

#[test]
fn runtime_failures_keep_imported_source_identity_on_all_vm_paths() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for effect in ["", "@suspends"] {
            for (expression, argument) in [("42 / n", 0), ("42 << n", 32)] {
                let dependency =
                    format!("pub fn value(n: Int32) {effect} -> Int32 {{ {expression} }}");
                let modules = sources(&[("app/math", &dependency)]);
                let mut vm = VmEngine::with_mode(mode);
                vm.load_source_with_modules(&format!("import app/math fn answer() {effect} -> Int32 {{ math.value({argument}) }}"), &modules).unwrap();
                let error = if effect.is_empty() {
                    vm.call_int32_0("answer").unwrap_err()
                } else {
                    vm.call_async("answer", &[]).unwrap_err()
                };
                drop(vm);
                drop(modules);
                let span = error.span().unwrap();
                let file = error.sources().get(span.source_id()).unwrap();
                assert_eq!(file.name(), "app/math.jk");
                assert_eq!(file.text(), dependency);
                assert_eq!(span.slice(file.text()), Some(expression));
            }
        }
    }
}

#[test]
fn pending_host_calls_preserve_imported_error_sources_after_reload() {
    use joky::{
        runtime::{
            cps::{CpsFrame, DONE},
            suspension::ResumeError,
        },
        FunctionSignature, Type,
    };
    unsafe extern "C" fn wait(frame: *mut CpsFrame) -> i32 {
        let frame = unsafe { &mut *frame };
        if frame.state == 0 {
            frame.state = 1;
            1
        } else {
            frame.state = DONE;
            0
        }
    }
    let dependency =
        "extern fn wait() @suspends pub fn value() @suspends -> Int32 { wait(); 42 / 0 }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let modules = sources(&[("app/math", dependency)]);
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_suspending_fn("wait", FunctionSignature::new(vec![], Type::Unit), wait)
        }
        .unwrap();
        vm.load_source_with_modules(
            "import app/math fn answer() @suspends -> Int32 { math.value() }",
            &modules,
        )
        .unwrap();
        let mut task = vm.call_async("answer", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
        drop(vm);
        drop(modules);
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected structured failure");
        };
        let span = error.span().unwrap();
        let file = error.sources().get(span.source_id()).unwrap();
        assert_eq!(file.text(), dependency);
        assert_eq!(span.slice(file.text()), Some("42 / 0"));
    }
}

#[test]
fn suspended_imported_code_keeps_sources_after_reload_and_vm_drop() {
    use joky::runtime::suspension::ResumeError;
    use joky::vm::InterpreterLimits;
    let source = "pub fn divide(n: Int32) @suspends -> Int32 { 42 / n }";
    let modules = sources(&[("app/math", source)]);
    let mut vm = VmEngine::new_interpreter();
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    vm.load_source_with_modules(
        "import app/math fn answer() @suspends -> Int32 { math.divide(0) }",
        &modules,
    )
    .unwrap();
    let mut task = vm.call_async("answer", &[]).unwrap();
    task.resume().unwrap();
    assert!(!task.is_finished());
    vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
    drop(vm);
    drop(modules);
    for _ in 0..100 {
        if let Err(ResumeError::Failed(error)) = task.resume() {
            let span = error.span().unwrap();
            let file = error.sources().get(span.source_id()).unwrap();
            assert_eq!(file.name(), "app/math.jk");
            assert_eq!(file.text(), source);
            assert_eq!(span.slice(file.text()), Some("42 / n"));
            return;
        }
    }
    panic!("suspended call did not preserve its failure");
}

#[test]
fn user_module_declarations_keep_existing_name_checks() {
    for declaration in [
        "pub fn int32(n: Int32) -> Int32 { n }",
        "pub fn __joky_runtime_bad() -> Int32 { 0 }",
        "pub fn same() {} fn same() {}",
        "pub enum Same { One } struct Same {}",
        "pub enum Choice { Same } pub fn Same() {}",
    ] {
        let modules = sources(&[("app/bad", declaration)]);
        assert!(
            VmEngine::new_interpreter()
                .load_source_with_modules("import app/bad", &modules)
                .is_err(),
            "{declaration}"
        );
    }
    for source in [
        "pub",
        "pub import app/math",
        "pub impl Int32 {}",
        "pub extern fn value()",
        "pub pub fn value() {}",
    ] {
        assert!(joky::parser::parse(source).is_err(), "{source}");
    }
}

#[test]
fn user_modules_inherit_the_configured_prelude() {
    let modules = sources(&[(
        "app/math",
        "pub fn value() -> Option<UInt64> { (42).to_u64() }",
    )]);
    check("import app/math fn answer() -> Int32 { match math.value() { Some(n) => int32(n), None => 0 } }", &modules, 42);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        assert!(vm
            .load_source_with_modules("import app/math fn answer() -> Int32 { 0 }", &modules)
            .is_err());
        let modules = sources(&[(
            "app/math",
            "import joky/option.{type Option, Some} pub fn value() -> Option<Int32> { Some(42) }",
        )]);
        vm.load_source_with_modules("import app/math import joky/option.{type Option, Some, None} fn answer() -> Int32 { match math.value() { Some(n) => n, None => 0 } }", &modules).unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    }
}

#[test]
fn failed_loads_preserve_the_previous_module_and_diagnostics() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
        let version = vm.module_version();
        for dependency in [
            "pub fn bad() -> Int32 { 未定义() }",
            "pub fn bad() -> Missing { 0 }",
            "pub fn bad() -> Int32 { @ }",
        ] {
            let mut modules = ModuleSources::new();
            modules
                .insert_file("app/bad", SourceFile::new("real/bad.jk", dependency))
                .unwrap();
            let error = vm
                .load_source_with_modules("import app/bad fn answer() -> Int32 { 1 }", &modules)
                .unwrap_err();
            let span = error.span().unwrap();
            let file = error.sources().get(span.source_id()).unwrap();
            assert_eq!(file.name(), "real/bad.jk");
            assert_eq!(file.text(), dependency);
            assert!(span.slice(file.text()).is_some());
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        }
    }
}

#[test]
fn explicit_sources_report_complete_cycles_and_missing_dependencies() {
    let modules = sources(&[
        ("app/a", "import app/b"),
        ("app/b", "import app/c"),
        ("app/c", "import app/a"),
    ]);
    let mut vm = VmEngine::new_interpreter();
    let error = vm
        .load_source_with_modules("import app/a", &modules)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidImport);
    assert!(
        error.message().contains("app/a -> app/b -> app/c -> app/a"),
        "{error}"
    );
    assert_eq!(
        error
            .sources()
            .get(error.span().unwrap().source_id())
            .unwrap()
            .name(),
        "app/c.jk"
    );
    let error = vm
        .load_source_with_modules("import app/missing", &modules)
        .unwrap_err();
    assert!(error
        .message()
        .contains("No source supplied for module 'app/missing'"));
    for path in [
        "",
        "../secret",
        "/root",
        "app//math",
        "app/math.jk",
        "joky/option",
        "joky",
    ] {
        assert!(ModuleSources::new().insert(path, "").is_err(), "{path}");
    }
}

#[test]
fn interpreter_runtime_trace_retains_each_source_snapshot() {
    let dependency = "pub fn divide(n: Int32) -> Int32 { 42 / n }";
    let mut modules = sources(&[("app/math", dependency)]);
    let mut vm = VmEngine::new_interpreter();
    vm.load_source_with_modules(
        "import app/math fn answer() -> Int32 { math.divide(0) }",
        &modules,
    )
    .unwrap();
    let error = vm.call_int32_0("answer").unwrap_err();
    modules
        .insert("app/math", "pub fn divide(n: Int32) -> Int32 { n }")
        .unwrap();
    vm.load_source_with_modules(
        "import app/math fn answer() -> Int32 { math.divide(42) }",
        &modules,
    )
    .unwrap();
    assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    let trace = error.runtime_trace();
    assert_eq!(trace.len(), 2);
    let file = error.sources().get(trace[0].span.source_id()).unwrap();
    assert_eq!(file.name(), "app/math.jk");
    assert_eq!(file.text(), dependency);
    assert_eq!(trace[0].span.slice(file.text()), Some("42 / n"));
    assert_eq!(
        error
            .sources()
            .get(trace[1].span.source_id())
            .unwrap()
            .name(),
        "<entry>"
    );
}

#[test]
fn record_fields_default_private_and_require_pub_for_cross_module_access() {
    let modules = sources(&[
        (
            "counter",
            "pub struct Counter { value: Int32, pub step: Int32 } impl Counter { pub fn new() -> Self { Counter { value: 0, step: 1 } } pub fn bump(&mut self) { self.value = self.value + self.step } pub fn read(self) -> Int32 { self.value } } pub fn touch(c: &mut Counter) { c.value = c.value + 1 }",
        ),
    ]);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(
            "import counter.{type Counter, touch} fn main() -> Int32 { var c = Counter.new(); touch(&mut c); c.step = 2; c.bump(); c.bump(); c.read() }",
            &modules,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("main").unwrap(), 5);
    }
    for body in [
        "fn main() -> Int32 { let c = counter.Counter.new(); c.value }",
        "fn main() -> Int32 { var c = counter.Counter.new(); c.value = 1; 0 }",
        "fn main() -> Int32 { let c = counter.Counter.new(); let r = &c.value; 0 }",
        "fn main() -> Int32 { let c = counter.Counter { value: 1, step: 1 }; 0 }",
    ] {
        let error = VmEngine::with_mode(VmMode::Jit)
            .load_source_with_modules(&format!("import counter.{{type Counter}} {body}"), &modules)
            .unwrap_err();
        assert!(
            error.message().contains("private"),
            "{body}: {}",
            error.message()
        );
    }
}
