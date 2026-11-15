//! Standard-library documentation uses the compiler's embedded source inventory.
use super::sources::{SourceDefinition, SOURCES};
use crate::{
    parser::{
        ast::{Import, ImportMemberKind},
        documentation::{extract_with_trait_docs, rename_entry, Documentation, Entry},
    },
    Error,
};
use std::collections::BTreeMap;

pub fn standard_library() -> Result<Vec<(&'static str, Documentation)>, Error> {
    // Resolve public facades as well as original declarations. A trait
    // implemented through an import such as joky/iter keeps its contract.
    let empty = BTreeMap::new();
    let traits = SOURCES
        .iter()
        .map(|source| {
            let mut docs = module_documentation(source, SOURCES, &empty)?;
            docs.entries.retain(|entry| entry.kind == "trait");
            Ok((source.path, docs.entries))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()?;
    let mut modules = Vec::new();
    for source in SOURCES {
        if source.path.contains("/internal/") {
            continue;
        }
        modules.push((source.path, module_documentation(source, SOURCES, &traits)?));
    }
    modules.sort_by_key(|(name, _)| *name);
    Ok(modules)
}

fn module_documentation(
    source: &SourceDefinition,
    sources: &[SourceDefinition],
    traits: &BTreeMap<&str, Vec<Entry>>,
) -> Result<Documentation, Error> {
    let lookup = |imports: &[Import], name: &str| imported_trait(traits, imports, name);
    let mut docs = extract_with_trait_docs(source.source, lookup)?;
    if !source.reexport_imports {
        return Ok(docs);
    }
    // Internal implementation types are documented at their public facade.
    // Only selected exports (and their impls) belong here, never sibling helpers.
    for import in crate::parser::parse(source.source)?.imports {
        if !import.path.contains("/internal/") || import.members.is_empty() {
            continue;
        }
        let Some(dependency) = sources.iter().find(|s| s.path == import.path) else {
            continue;
        };
        let renames: BTreeMap<_, _> = import
            .members
            .iter()
            .filter_map(|m| {
                m.alias
                    .as_ref()
                    .map(|alias| (m.name.as_str(), alias.as_str()))
            })
            .collect();
        let exported = extract_with_trait_docs(dependency.source, lookup)?;
        for mut entry in exported.entries {
            if !import.members.iter().any(|member| {
                member.name == entry.name
                    && match member.kind {
                        ImportMemberKind::Type => {
                            matches!(entry.kind, "struct" | "enum" | "type" | "trait" | "impl")
                        }
                        ImportMemberKind::Value => matches!(entry.kind, "fn" | "const"),
                    }
            }) {
                continue;
            }
            rename_entry(&mut entry, &renames)?;
            if !docs.entries.iter().any(|existing| {
                existing.kind == entry.kind && existing.signature == entry.signature
            }) {
                docs.entries.push(entry);
            }
        }
    }
    Ok(docs)
}

fn imported_trait<'a>(
    traits: &'a BTreeMap<&str, Vec<Entry>>,
    imports: &[Import],
    name: &str,
) -> Option<&'a [Entry]> {
    let (qualifier, name) = name
        .rsplit_once('.')
        .map_or((None, name), |(q, n)| (Some(q), n));
    imports.iter().find_map(|import| {
        let name = if let Some(qualifier) = qualifier {
            let alias = import
                .alias
                .as_deref()
                .or_else(|| import.path.rsplit('/').next())?;
            (alias == qualifier).then_some(name)?
        } else {
            import.members.iter().find_map(|member| {
                (member.kind == ImportMemberKind::Type
                    && member.alias.as_deref().unwrap_or(&member.name) == name)
                    .then_some(member.name.as_str())
            })?
        };
        traits
            .get(import.path.as_str())?
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.children.as_slice())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::imports::sources::{ImplicitImports, SourceKind};

    #[test]
    fn standard_library_public_declarations_have_documentation() {
        let modules = standard_library().unwrap();
        let mut missing = Vec::new();
        for (module, docs) in &modules {
            for entry in &docs.entries {
                if entry.kind != "impl" && entry.docs.trim().is_empty() {
                    missing.push(format!("{module}: {}", entry.signature));
                }
                for child in &entry.children {
                    if child.docs.trim().is_empty() {
                        missing.push(format!(
                            "{module}: {} / {}",
                            entry.signature, child.signature
                        ));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "{}", missing.join("\n"));
    }

    #[test]
    fn internal_type_reexports_document_only_selected_public_members() {
        let sources = [
            SourceDefinition {
                kind: SourceKind::InternalLibrary,
                path: "joky/internal/clock",
                source: r#"
                    /// An opaque point.
                    pub struct Point { ticks: UInt64 }
                    impl Point {
                        pub fn now() -> Point { Point { ticks: 0 } }
                        fn hidden(&self) -> UInt64 { self.ticks }
                    }
                    pub struct Other {}
                    pub fn sleep_until(point: Point) {}
                "#,
                reexport_imports: false,
                implicit: ImplicitImports::None,
            },
            SourceDefinition {
                kind: SourceKind::Library,
                path: "joky/clock",
                source: "//! Public clocks.\nimport joky/internal/clock.{type Point as Instant}",
                reexport_imports: true,
                implicit: ImplicitImports::None,
            },
        ];
        let docs = module_documentation(&sources[1], &sources, &BTreeMap::new()).unwrap();
        assert_eq!(docs.docs, "Public clocks.");
        assert!(docs.imports.is_empty());
        assert_eq!(docs.entries.len(), 2);
        let record = &docs.entries[0];
        assert_eq!(record.signature, "pub struct Instant");
        assert_eq!(record.docs, "An opaque point.");
        assert!(record.children.is_empty());
        let implementation = &docs.entries[1];
        assert_eq!(implementation.name, "Instant");
        assert_eq!(implementation.signature, "impl Instant");
        assert_eq!(implementation.children.len(), 1);
        assert_eq!(
            implementation.children[0].signature,
            "pub fn now() -> Instant"
        );
    }
}
