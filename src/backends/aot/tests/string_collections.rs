//! Standalone execution of String container ownership and bulk text models.
use super::*;

#[test]
#[cfg(unix)]
fn string_collections_collections_match_models_in_standalone_aot() {
    let mut groups = crate::string_collection_model::ownership_groups();
    groups.push(crate::string_collection_model::bulk_group());
    run_groups(groups);
}

#[test]
#[cfg(unix)]
fn string_map_map_ownership_and_failures_in_aot() {
    run_groups(crate::string_map_model::groups());
}

#[cfg(unix)]
fn run_groups(groups: Vec<crate::string_collection_model::Group>) {
    for (index, group) in groups.into_iter().enumerate() {
        let fixture = ScratchDir::new(&format!("string-collections-{index}"));
        let object = fixture.path().join("collections.o");
        let host = fixture.path().join("host.c");
        let program = fixture.path().join("collections");
        let mut compiler = AotCompiler::new("string-collections").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&group.source).unwrap())
            .unwrap_or_else(|error| panic!("{error}"));
        std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
        compiler.write_object(&object).unwrap();
        let mut harness = String::from(
            r#"
            #include "text.h"
            #include "joky_runtime.h"
            #include <string.h>
            #include <stdio.h>
            static int created[100], dropped[100], invalid;
            void record_drop(int32_t value) {
                if (!value || value <= -100 || value >= 100) { invalid = 1; return; }
                if (value < 0) created[-value]++; else dropped[value]++;
            }
            static void reset(void) { memset(created, 0, sizeof created); memset(dropped, 0, sizeof dropped); invalid = 0; }
            static int verify(const int *ids, size_t length) {
                if (invalid) return 0;
                for (int id = 1; id < 100; id++) {
                    int expected = 0;
                    for (size_t i = 0; i < length; i++) if (ids[i] == id) expected++;
                    if (created[id] != expected || dropped[id] != expected) return 0;
                }
                return 1;
            }
        "#,
        );
        for entry in &group.entries {
            if entry.text.is_none() {
                harness.push_str(&format!("extern int32_t {}(void);\n", entry.name));
            }
        }
        harness.push_str("int main(void) {\n");
        for (index, entry) in group.entries.iter().enumerate() {
            harness.push_str("reset();\n");
            let name = &entry.name;
            if let Some(expected) = &entry.text {
                let length = expected.len();
                let mut bytes = expected
                    .bytes()
                    .map(|byte| byte.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                if bytes.is_empty() {
                    bytes.push('0');
                }
                harness.push_str(&format!("{{ const uint8_t expected[] = {{{bytes}}}; JokyOwnedTextV2 output = {{0}}; if (__joky_user_text_v2_{name}(&output) || joky_runtime_error_code() || output.byte_length != {length} || ({length} && memcmp(output.data, expected, {length}))) {{ fprintf(stderr, \"{name} failed\\n\"); return 1; }} joky_string_result_free_v2(&output); }}\n"));
            } else if entry.failure {
                harness.push_str(&format!("if ({name}() != 0 || !joky_runtime_error_code()) {{ fprintf(stderr, \"{name} failed\\n\"); return 2; }}; joky_runtime_error_clear();\n"));
            } else {
                harness.push_str(&format!("if ({name}() != 42 || joky_runtime_error_code()) {{ fprintf(stderr, \"{name} failed\\n\"); return 3; }}\n"));
            }
            let mut ids = entry
                .drops
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            if ids.is_empty() {
                ids.push('0');
            }
            harness.push_str(&format!("const int ids_{index}[] = {{{ids}}}; if (!verify(ids_{index}, {})) {{ fprintf(stderr, \"{name} cleanup failed\\n\"); return 4; }}\n", entry.drops.len()));
        }
        harness.push_str("return 0; }\n");
        std::fs::write(&host, harness).unwrap();
        link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
        assert!(
            Command::new(program).status().unwrap().success(),
            "group {index}"
        );
    }
}

#[test]
#[cfg(unix)]
fn string_collections_join_length_boundaries_execute_in_aot_without_allocating_payloads() {
    let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
    assert!(registry
        .types()
        .standard
        .set(crate::core::imports::string_test_library())
        .is_ok());
    let fixture = ScratchDir::new("string-length-boundaries");
    let object = fixture.path().join("length.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("length");
    let mut compiler =
        AotCompiler::with_symbol_prefix_and_registry("length", "", registry).unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(
                "import joky/string fn probe() -> Int32 { string.probe_join_length() }",
            )
            .unwrap(),
        )
        .unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, "#include <stdint.h>\nextern int32_t probe(void);\nint main(void) { return probe() == 42 ? 0 : 1; }\n").unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    assert!(Command::new(program).status().unwrap().success());
}
