use super::*;

#[test]
fn source_sum_methods_borrow_queries_and_move_payloads() {
    run_program(
        r#"
        class Box { let text: String }
        fn main() {
            let value = Some(Box("option"))
            if !value.is_some() || value.is_none() { panic("option query") }
            let box = value.unwrap_or(Box("fallback"))
            if box.text != "option" { panic("option payload") }
            let result: Result(Box, Box) = Ok(Box("result"))
            if !result.is_ok() || result.is_err() { panic("result query") }
            let box = result.unwrap_or(Box("fallback"))
            if box.text != "result" { panic("result payload") }
            let error: Result(Box, Box) = Err(Box("error"))
            if !error.is_err() || error.is_ok() { panic("error query") }
            let box = error.unwrap_or(Box("default"))
            if box.text != "default" { panic("error fallback") }
            let missing: Option(Box) = None
            if !missing.is_none() { panic("missing query") }
            let box = missing.unwrap_or(Box("none"))
            if box.text != "none" { panic("none fallback") }
        }
    "#,
    );
    for source in [
        "let value = Some(Box(1)); let box = value.unwrap_or(Box(2)); let again = value.is_some()",
        "let value: Result(Box, String) = Ok(Box(1)); let box = value.unwrap_or(Box(2)); let again = value.is_ok()",
    ] {
        assert!(try_run_program(&format!(
            "class Box {{ let value: Int32 }} fn main() {{ {source} }}"
        )).is_err());
    }
}

#[test]
fn source_sum_defaults_follow_ordinary_call_evaluation() {
    run_program(
        r#"
        class Counter {
            var count: Int32
            fn fallback(&self) -> Int32 {
                self.count = self.count + 1
                7
            }
        }
        fn main() {
            let counter = Counter(0)
            if Some(42).unwrap_or(counter.fallback()) != 42 { panic("some") }
            let ok: Result(Int32, String) = Ok(42)
            if ok.unwrap_or(counter.fallback()) != 42 { panic("ok") }
            if counter.count != 2 { panic("eager arguments") }
            if Some(42).unwrap_or_else(fn () -> Int32 { panic("unexpected fallback"); 0 }) != 42 { panic("lazy some") }
            let none: Option(Int32) = None
            if none.unwrap_or_else(fn () -> Int32 { 7 }) != 7 { panic("lazy none") }
            if ok.unwrap_or_else(fn (error: String) -> Int32 { panic(error); 0 }) != 42 { panic("lazy ok") }
            let err: Result(Int32, String) = Err("failure")
            if err.unwrap_or_else(fn (error: String) -> Int32 {
                if error != "failure" { panic("wrong error") }
                9
            }) != 9 { panic("lazy error") }
        }
    "#,
    );
}

#[test]
fn borrowed_matches_keep_owned_payloads_borrowed() {
    run_program(
        r#"
        class Box { let value: Int32 }
        fn is_some(value: &Option(Box)) -> Bool { panic("caller shadowed prelude"); false }
        fn inspect(value: &Option(Box)) -> Int32 {
            match value { Some(box) => box.value; None => 0 }
        }
        fn inspect_pair(value: &Option((Box, String))) -> String {
            match value { Some((_, text)) => text; None => "missing" }
        }
        fn main() {
            let value = Some(Box(42))
            if value.is_none() { panic("prelude helper resolution") }
            if inspect(value) != 42 { panic("borrowed match") }
            if inspect(value) != 42 { panic("second borrow") }
            let box = value!
            if box.value != 42 { panic("payload was moved") }
            let result: Result(String, Box) = Ok("shared" + " payload")
            if result.is_err() || result.is_err() { panic("shared query") }
            if result.unwrap_or("bad") != "shared payload" { panic("shared payload") }
            let pair = Some((Box(1), "nested" + " shared"))
            if inspect_pair(pair) != "nested shared" { panic("tuple borrow") }
            if inspect_pair(pair) != "nested shared" { panic("repeated tuple borrow") }
            let pair = pair!
            if pair.1 != "nested shared" { panic("tuple payload") }
        }
    "#,
    );
    assert!(try_run_program(
        r#"
        class Box { let value: Int32 }
        fn steal(value: &Option(Box)) -> Box {
            match value { Some(box) => box; None => Box(0) }
        }
        fn main() {}
    "#
    )
    .is_err());
}
