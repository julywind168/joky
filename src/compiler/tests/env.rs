use super::*;

const ENV_API: &str = include_str!("../../../std/joky/env.jk");

#[test]
fn env_arguments_are_isolated_between_runs_of_one_compiler() {
    let mut compiler = Compiler::new().unwrap();
    for expected in ["first", "second"] {
        let source = format!(
            r#"{ENV_API}
            fn main() effects {{ env }} {{
                let args = env.args()!
                if args != List("{expected}", "", "参数") {{ panic("args") }} else {{}}
                let variables = env.vars()!
                for key in variables.keys() {{
                    if env.get(key)! != variables.get(key) {{ panic("snapshot") }} else {{}}
                }}
                match env.get("") {{ Err(_) => {{}}, Ok(_) => panic("invalid name") }}
                if env.current_dir()!.is_empty() {{ panic("cwd") }} else {{}}
                if env.temp_dir()!.is_empty() {{ panic("temp") }} else {{}}
            }}"#
        );
        compiler
            .run_program_with_args(&source, vec![expected.into(), "".into(), "参数".into()])
            .unwrap();
    }
    compiler
        .run_program(&format!(
            r#"{ENV_API}
        fn main() effects {{ env }} {{
            if !env.args()!.is_empty() {{ panic("inherited host arguments") }} else {{}}
        }}"#
        ))
        .unwrap();
}

#[test]
fn env_get_can_be_replaced_by_a_local_handler() {
    let source = format!(
        r#"{ENV_API}
        fn read_setting() -> Result(Option(String), String) effects {{ env }} {{ env.get("APP_MODE") }}
        fn fixture(name: String) -> Result(Option(String), String) {{
            if name == "APP_MODE" {{ Ok(Some("test")) }} else {{ Ok(None) }}
        }}
        fn main() {{
            let value = do {{ read_setting() }} with {{
                env.get(name) => fixture(name)
            }}
            if value! != Some("test") {{ panic("handler") }} else {{}}
        }}"#
    );
    run_program(&source);
}

#[test]
fn env_requires_a_declared_or_handled_effect() {
    let error = try_run_program(&format!(
        r#"{ENV_API}
        fn unhandled() -> Result(Option(String), String) {{ env.get("APP_MODE") }}
        fn main() {{ let _ = unhandled(); () }}"#
    ))
    .unwrap_err();
    assert!(error.to_string().contains("env"), "{error}");
}

#[test]
fn env_handlers_transfer_shared_results_and_override_no_argument_operations() {
    let source = format!(
        r#"{ENV_API}
        fn identity(name: String) -> Result(Option(String), String) {{ Ok(Some(name)) }}
        fn fixture_args() -> Result(List(String), String) {{ Ok(List("mock")) }}
        fn fixture_vars() -> Result(Map(String, String), String) {{ Ok(Map#{{"mode" => "mock"}}) }}
        fn inspect() effects {{ env }} {{
            if env.get("echo")! != Some("echo") {{ panic("shared argument") }} else {{}}
            if env.args()! != List("mock") {{ panic("mock args") }} else {{}}
            if env.vars()!.get("mode") != Some("mock") {{ panic("mock vars") }} else {{}}
            if env.current_dir()! != "/mock/cwd" {{ panic("mock cwd") }} else {{}}
            match env.temp_dir() {{ Err(message) => if message != "mock temp" {{ panic("mock error") }} else {{}}, Ok(_) => panic("expected error") }}
        }}
        fn main() {{
            do {{ inspect() }} with {{
                env.get(name) => identity(name)
                env.args() => fixture_args()
                env.vars() => fixture_vars()
                env.current_dir() => Ok("/mock/cwd")
                env.temp_dir() => Err("mock temp")
            }}
        }}"#
    );
    run_program(&source);
}
