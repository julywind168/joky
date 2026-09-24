use super::super::*;

#[test]
fn invokes_capture_free_resumable_handler_mir_function() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int64) -> Int64 }

                    fn main() {
                        let value = do { Calc.adjust(value: 41) } with {
                            Calc.adjust(value) => resume(value + 1)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("capture-free handler body should execute through synthetic MIR");
}

#[test]
fn invokes_synthetic_handler_with_int32_payload() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }

                    fn main() {
                        let value = do { Calc.adjust(value: 41) } with {
                            Calc.adjust(value) => resume(value + 1)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("Int32 synthetic handler should execute");
}

#[test]
fn invokes_synthetic_handler_with_bool_payload() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Logic { @resumable fn invert(value: Bool) -> Bool }

                    fn main() {
                        let value = do { Logic.invert(value: true) } with {
                            Logic.invert(value) => resume(!value)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("Bool synthetic handler should execute");
}

#[test]
fn invokes_synthetic_handler_with_float64_payload() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Float64) -> Float64 }

                    fn main() {
                        let value = do { Calc.adjust(value: 1.5) } with {
                            Calc.adjust(value) => resume(value + 0.5)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("Float64 synthetic handler should execute");
}

#[test]
fn invokes_synthetic_handler_with_a_synchronous_function_call() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int64) -> Int64 }

                    fn increment(value: Int64) -> Int64 { value + 1 }

                    fn main() {
                        let value = do { Calc.adjust(value: 41) } with {
                            Calc.adjust(value) => resume(increment(value: value))
                        }
                        println(value)
                    }
                "#,
        )
        .expect("synthetic handler should call a synchronous function");
}

#[test]
fn invokes_synthetic_handler_with_an_immutable_capture() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int64) -> Int64 }

                    fn main() {
                        let offset: Int64 = 1
                        let value = do { Calc.adjust(value: 41) } with {
                            Calc.adjust(value) => resume(value + offset)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("synthetic handler should retain an immutable capture");
}

#[test]
fn invokes_synthetic_handler_with_multiple_immutable_captures() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int64) -> Int64 }

                    fn main() {
                        let offset: Int64 = 1
                        let scale: Int64 = 2
                        let bias: Int64 = 3
                        let value = do { Calc.adjust(value: 20) } with {
                            Calc.adjust(value) => resume(value * scale + offset + bias)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("synthetic handler should retain multiple immutable captures");
}

#[test]
fn invokes_synthetic_handler_with_mixed_scalar_captures() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int64) -> Int64 }

                    fn main() {
                        let enabled: Bool = true
                        let bump: Int32 = 2
                        let value = do { Calc.adjust(value: 20) } with {
                            Calc.adjust(value) => resume({
                                let copied = bump
                                if enabled { value + 1 } else { value }
                            })
                        }
                        println(value)
                    }
                "#,
        )
        .expect("synthetic handler should retain mixed scalar captures");
}
