//! Isolated allocation/timing benchmark for module compilation, without codegen.
use super::*;
use crate::test_metrics;
use std::time::Instant;

struct Package(std::path::PathBuf);

impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "isolated compiler allocation benchmark; run alone with --ignored --nocapture"]
fn module_compilation_scaling() {
    for count in [8, 32, 128] {
        let root = Package(
            std::env::temp_dir().join(format!("joky-module-bench-{}-{count}", std::process::id())),
        );
        std::fs::create_dir_all(root.0.join("src")).unwrap();
        std::fs::write(
            root.0.join("src/generic.jk"),
            "pub fn identity(T: type, value: T) -> T { value }",
        )
        .unwrap();
        let mut main = String::new();
        for index in 0..count {
            main.push_str(&format!("import m{index}\n"));
            let mut source = String::from("import generic\nstruct Record { let value: Int32 }\n");
            for function in 0..8 {
                source.push_str(&format!(
                    "pub fn f{function}(value: Int32) -> Int32 {{\n\
                     let record = generic.identity(Record(value: value))\n\
                     record.value + {function}\n}}\n"
                ));
            }
            std::fs::write(root.0.join(format!("src/m{index}.jk")), source).unwrap();
        }
        main.push_str("fn main() {\n");
        for index in 0..count {
            main.push_str(&format!("let value{index} = m{index}.f0({index})\n"));
        }
        main.push_str("}\n");
        std::fs::write(root.0.join("src/main.jk"), main).unwrap();
        let graph = ModuleGraph::load(&root.0.join("src/main.jk"), &root.0).unwrap();
        let units = graph
            .source_units(
                env!("JOKY_BUILD_ID"),
                env!("JOKY_TARGET"),
                usize::BITS as u8,
                MODULE_MIR_FEATURES,
            )
            .unwrap();
        let metadata = graph.metadata_map();
        let mut compiler = Compiler::new().unwrap();
        for sample in 0..3 {
            let cache_path = root.0.join(format!("cache-{sample}"));
            let cache = ModuleCache::new(&cache_path);
            for mode in ["cold", "warm"] {
                let (_, before) = test_metrics::allocations();
                let started = Instant::now();
                let artifacts = compiler
                    .frontend
                    .compile_units(&units, &graph, &metadata, &cache)
                    .unwrap();
                let elapsed = started.elapsed();
                let (_, after) = test_metrics::allocations();
                assert_eq!(artifacts.len(), 2 * count + 2);
                if mode == "warm" {
                    assert_eq!(compiler.frontend.module_compilations, 0);
                    assert_eq!(compiler.frontend.module_cache_hits, artifacts.len());
                }
                let cache_bytes: u64 = std::fs::read_dir(&cache_path)
                    .unwrap()
                    .map(|entry| entry.unwrap())
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jmir"))
                    .map(|entry| entry.metadata().unwrap().len())
                    .sum();
                eprintln!(
                    "module-bench modules={} instances={count} mode={mode} sample={sample} ms={:.3} allocated_bytes={} artifacts={} cache_bytes={cache_bytes}",
                    units.len(), elapsed.as_secs_f64() * 1000.0, after - before, artifacts.len()
                );
                std::hint::black_box(artifacts);
            }
        }
    }
}
