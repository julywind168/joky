//! Local manifest graph. Package identity is the canonical manifest directory.
use super::*;
use joky::{PackageSources, SourcePackage};

pub(super) fn parse(value: Option<toml::Value>) -> Result<BTreeMap<String, String>, Error> {
    let table = match value {
        None => return Ok(BTreeMap::new()),
        Some(toml::Value::Table(table)) => table,
        _ => {
            return Err(cli_argument_error(
                "Dependencies must be a table of path dependencies",
            ))
        }
    };
    let mut result = BTreeMap::new();
    for (alias, dependency) in table {
        let mut chars = alias.chars();
        if alias == "joky"
            || !chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
            || !chars.all(|c| c == '_' || c.is_alphanumeric())
        {
            return Err(cli_argument_error(format!(
                "Invalid or reserved dependency alias: {alias}"
            )));
        }
        let toml::Value::Table(mut dependency) = dependency else {
            return Err(cli_argument_error(format!(
                "Dependency '{alias}' must declare a path"
            )));
        };
        let path = string(&mut dependency, "path", None)?;
        unknown(&dependency)?;
        if Path::new(&path).is_absolute() {
            return Err(cli_argument_error(
                "Dependency paths must be relative to their declaring manifest",
            ));
        }
        result.insert(alias, path);
    }
    Ok(result)
}

pub(super) fn load(root: &mut Project) -> Result<(), Error> {
    struct Graph {
        packages: Vec<SourcePackage>,
        paths: BTreeMap<PathBuf, usize>,
        names: BTreeMap<String, PathBuf>,
        active: Vec<PathBuf>,
    }
    impl Graph {
        fn visit(&mut self, project: &Project) -> Result<usize, Error> {
            if self.active.contains(&project.root) {
                let mut cycle: Vec<_> = self
                    .active
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                cycle.push(project.root.display().to_string());
                return Err(cli_argument_error(format!(
                    "Cyclic package dependency: {}",
                    cycle.join(" -> ")
                )));
            }
            if let Some(index) = self.paths.get(&project.root) {
                return Ok(*index);
            }
            if self.packages.len() >= 128 {
                return Err(cli_argument_error("Package graph exceeds 128 packages"));
            }
            if let Some(previous) = self.names.get(&project.name) {
                if previous != &project.root {
                    return Err(cli_argument_error(format!("Conflicting sources for package '{}': '{}' and '{}'; multiple versions are not supported", project.name, previous.display(), project.root.display())));
                }
            }
            self.names
                .insert(project.name.clone(), project.root.clone());
            let index = self.packages.len();
            self.paths.insert(project.root.clone(), index);
            self.packages.push(SourcePackage {
                name: project.name.clone(),
                source_root: project.source_root.clone(),
                dependencies: BTreeMap::new(),
            });
            self.active.push(project.root.clone());
            for (alias, relative) in &project.dependencies {
                let directory = project.root.join(relative);
                let child = Project::read(&directory.join(MANIFEST))?;
                if child.entry.is_some() {
                    return Err(cli_argument_error(format!(
                        "Dependency '{alias}' must declare [package], not [project]"
                    )));
                }
                let target = self.visit(&child)?;
                self.packages[index]
                    .dependencies
                    .insert(alias.clone(), target);
            }
            self.active.pop();
            Ok(index)
        }
    }
    let mut graph = Graph {
        packages: Vec::new(),
        paths: BTreeMap::new(),
        names: BTreeMap::new(),
        active: Vec::new(),
    };
    graph.visit(root)?;
    root.dependency_roots = graph
        .paths
        .keys()
        .filter(|path| **path != root.root)
        .cloned()
        .collect();
    root.packages = Some(PackageSources::new(graph.packages)?);
    Ok(())
}
