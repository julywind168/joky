//! Evaluate imported type functions in an isolated defining-module checker.
use super::checker::Checker;
use super::{ModuleTypes, Type};
use crate::diagnostic::SemanticError;
use crate::module::{ModuleCompileContext, StableId};
use crate::syntax::{TypeAnnotation, TypeArgument, TypeExpr, Visibility};
use crate::Span;

impl Checker {
    pub(super) fn import_type_function(
        &mut self,
        module: StableId,
        name: &str,
        arguments: &[(Option<String>, Type, Span)],
        span: Span,
    ) -> Result<Type, SemanticError> {
        let invalid = |message: String| SemanticError::FunctionNotSupported {
            name: message,
            span,
        };
        if self.type_query_depth >= 64 {
            return Err(invalid(
                "imported type function evaluation depth exceeded".into(),
            ));
        }
        let table = self
            .dependency_types
            .get(&module)
            .cloned()
            .ok_or_else(|| invalid("type function module is missing".into()))?;
        let program = table
            .interface
            .type_program
            .as_ref()
            .ok_or_else(|| invalid(format!("unknown imported type function '{name}'")))?;
        if !program.functions.iter().any(|f| {
            f.name == name
                && f.visibility == Visibility::Public
                && f.return_type.as_ref().is_some_and(|ty| ty.is_name("type"))
        }) {
            return Err(invalid(format!("unknown imported type function '{name}'")));
        }
        let mut context = ModuleCompileContext {
            imports: table.interface.module_imports.clone(),
            dependency_exports: table.interface.module_exports.clone(),
            dependency_types: self.dependency_types.clone(),
            standard_modules: table.interface.standard_modules.clone(),
            definition_module: Some(module),
            type_query_depth: self.type_query_depth + 1,
            ..Default::default()
        };
        // Temporary identity is confined to this query. Nominal names in the
        // snapshot already retain their real defining module identities.
        let snapshot_id = StableId(0);
        let mut snapshot = ModuleTypes::snapshot(self);
        snapshot.qualify_nominal_types(self.definition_module.unwrap_or(snapshot_id));
        // A nested query adds its private caller snapshot without changing the
        // shared session registry or another checker's query bindings.
        std::sync::Arc::make_mut(&mut context.dependency_types)
            .insert(snapshot_id, snapshot.into());
        let query_span = Span::new(usize::MAX - 2, usize::MAX - 1);
        let mut type_arguments = Vec::new();
        for (index, (label, ty, _)) in arguments.iter().enumerate() {
            let binding = format!("@query{index}");
            context
                .type_bindings
                .insert(binding.clone(), (snapshot_id, *ty));
            type_arguments.push(TypeArgument {
                label: label.clone(),
                value: TypeAnnotation::named(&binding, query_span),
            });
        }
        context.type_query = Some(TypeAnnotation {
            span: query_span,
            kind: TypeExpr::Apply {
                callee: Box::new(TypeAnnotation::named(name, query_span)),
                arguments: type_arguments,
            },
        });
        let mut result = super::check_module_with_context(program, &context)
            .map_err(|error| invalid(error.to_string()))?;
        result.qualify_nominal_types(module);
        let ty = result
            .queried_type
            .ok_or_else(|| invalid("type query returned no type".into()))?;
        // Query-local indices can differ from the cached defining table. Do not
        // reuse its index memoization; nominal names still deduplicate layouts.
        let saved = std::mem::take(&mut self.imported_types);
        let imported = self.import_abi_type(module, &result, ty, span);
        self.imported_types = saved;
        imported
    }
}
