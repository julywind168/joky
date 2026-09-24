use super::*;

#[cfg(unix)]
#[test]
fn path_functions_are_pure_and_preserve_managed_results() {
    run_program(
        &include_str!("../../../tests/fixtures/path.jk")
            .replace(
                "import joky/path",
                include_str!("../../../std/joky/path.jk"),
            )
            .replace("path.", ""),
    );
}

#[test]
fn enum_valued_struct_fields_are_projections_not_constructors() {
    run_program(
        r#"
        enum Mode { Auto, Named(value: String) }
        struct Config { let mode: Mode; fn copy(&self) -> Mode { self.mode } }
        fn main() {
            let config = Config(mode: Mode.Named("test"))
            match config.copy() { Mode.Auto => panic("wrong variant"), Mode.Named(value) => if value != "test" { panic("payload") } }
        }
    "#,
    );
}

#[cfg(unix)]
#[test]
fn process_command_builders_capture_and_local_handlers() {
    let source = format!(
        r#"{}
        fn fixture(command: &Command) -> Result(Output, String) {{
            Ok(Output(status: ExitStatus.Exited(0), stdout: Bytes.from_string(command.program), stderr: Bytes()))
        }}
        fn call(command: &Command) -> Result(Output, String) effects {{ process }} {{ process.output(command, Bytes(), 10) }}
        fn main() effects {{ process }} {{
            let config = command("/bin/sh").with_args(List("-c", "printf out; printf err >&2; exit 7"))
            let result = process.output(config, Bytes(), 6)!
            if result.stdout.to_string()! != "out" {{ panic("stdout") }}
            if result.stderr.to_string()! != "err" {{ panic("stderr") }}
            let seven: UInt32 = 7
            if code(result.status)! != seven {{ panic("status code") }}
            if success(result.status) {{ panic("nonzero success") }}
            let mock = do {{ call(command("mock")) }} with {{ process.output(command, _, _) => fixture(command) }}
            if mock!.stdout.to_string()! != "mock" {{ panic("mock output") }}
        }}"#,
        include_str!("../../../std/joky/process.jk")
    );
    run_program(&source);
}
