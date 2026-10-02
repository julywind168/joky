//! Built-in import and call resolution over the parsed AST.
//!
//! Host-module calls are rewritten after source-library expansion and type
//! normalization. Local scopes take precedence over host module names.
use crate::{
    parser::ast::{Expr, ExternFnDecl, ImportMemberKind, Item, Module, Param},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, Type,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// Name and signature information needed to inject an external binding.
#[derive(Clone, Debug)]
pub(crate) struct BindingSpec {
    pub(crate) name: String,
    pub(crate) signature: FunctionSignature,
    pub(crate) symbol: Option<String>,
    pub(crate) suspends: bool,
}

impl BindingSpec {
    pub(crate) fn with_symbol(
        name: impl Into<String>,
        signature: FunctionSignature,
        symbol: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            signature,
            symbol: Some(symbol.into()),
            suspends: false,
        }
    }
}

/// In-memory module-to-binding registry used by the resolver.
#[derive(Clone, Debug, Default)]
pub(crate) struct BindingRegistry {
    modules: HashMap<String, Vec<BindingSpec>>,
    names: HashMap<String, (String, usize)>,
}

impl BindingRegistry {
    pub(crate) fn register_module(
        &mut self,
        path: impl Into<String>,
        bindings: impl IntoIterator<Item = BindingSpec>,
    ) -> Result<(), Error> {
        let path = path.into();
        if super::imports::is_source_library(&path) || self.modules.contains_key(&path) {
            return Err(resolve_error_code(
                None,
                ErrorCode::DuplicateDefinition,
                format!("Module `{path}` is already registered"),
            ));
        }
        let bindings: Vec<_> = bindings.into_iter().collect();
        let mut names = HashSet::new();
        let mut exports = HashSet::new();
        let mut symbols: HashSet<_> = self
            .modules
            .values()
            .flatten()
            .filter_map(|b| b.symbol.as_deref())
            .collect();
        for binding in &bindings {
            if matches!(
                binding.name.as_str(),
                "alloc"
                    | "share"
                    | "memory.read"
                    | "memory.write"
                    | "memory.replace"
                    | "Ptr.null"
                    | "Ptr.is_null"
            ) {
                return Err(resolve_error_code(
                    None,
                    ErrorCode::ReservedName,
                    format!("Reserved memory intrinsic name '{}'", binding.name),
                ));
            }
            if crate::core::integer::IntegerType::conversion(&binding.name).is_some() {
                return Err(resolve_error_code(
                    None,
                    ErrorCode::ReservedName,
                    format!(
                        "Reserved integer conversion function name `{}`",
                        binding.name
                    ),
                ));
            }
            let short = binding.name.rsplit('.').next().unwrap();
            if !exports.insert(short) {
                return Err(resolve_error_code(
                    None,
                    ErrorCode::DuplicateDefinition,
                    format!("Module '{path}' exports duplicate value '{short}'"),
                ));
            }
            if self.names.contains_key(&binding.name) || !names.insert(binding.name.as_str()) {
                return Err(resolve_error_code(
                    None,
                    ErrorCode::DuplicateDefinition,
                    format!("Binding `{}` is already registered", binding.name),
                ));
            }
            if let Some(symbol) = &binding.symbol {
                if symbol.is_empty() || symbol.contains('\0') {
                    return Err(Error::new_with_code(
                        ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        format!("Invalid native symbol for `{}`", binding.name),
                    ));
                }
                if !symbols.insert(symbol.as_str()) {
                    return Err(Error::new_with_code(
                        ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        format!("Native symbol `{symbol}` is already registered"),
                    ));
                }
            }
        }
        for (index, binding) in bindings.iter().enumerate() {
            self.names
                .insert(binding.name.clone(), (path.clone(), index));
        }
        self.modules.insert(path, bindings);
        Ok(())
    }

    pub(crate) fn module(&self, path: &str) -> Option<&[BindingSpec]> {
        self.modules.get(path).map(Vec::as_slice)
    }

    pub(crate) fn binding(&self, name: &str) -> Option<&BindingSpec> {
        let (path, index) = self.names.get(name)?;
        self.modules.get(path)?.get(*index)
    }

    /// Check explicit extern declarations as well as injected declarations.
    pub(crate) fn validate_declarations(&self, module: &Module) -> Result<(), Error> {
        for item in &module.items {
            let Item::ExternFn(f) = item else { continue };
            let Some(binding) = self.binding(&f.name) else {
                continue;
            };
            let signature = FunctionSignature::new(
                f.params.iter().map(|p| p.ty.clone()).collect(),
                f.ret_ty.clone(),
            );
            if f.c_abi || signature != binding.signature || f.suspends != binding.suspends {
                return Err(Error::new_with_code(
                    ErrorPhase::Abi,
                    ErrorCode::AbiError,
                    format!(
                        "Module binding `{}` has an incompatible signature or @suspends ABI",
                        f.name
                    ),
                )
                .with_span(f.span));
            }
        }
        Ok(())
    }
}

/// Resolve imported modules and rewrite their built-in call forms.
pub(crate) fn resolve(module: &Module, registry: &BindingRegistry) -> Result<Module, Error> {
    let mut resolved = module.clone();
    let mut scope = module.scope.as_deref().cloned().unwrap_or_default();
    let mut call_forms = HashMap::new();
    let local_values: HashSet<_> = module
        .items
        .iter()
        .map(|item| match item {
            Item::FnDef(f) => f.name.as_str(),
            Item::ExternFn(f) => f.name.as_str(),
        })
        .collect();
    let mut imported_paths = HashSet::new();
    let mut imported_bindings = Vec::new();
    for import in &module.imports {
        let Some(bindings) = registry.module(&import.path) else {
            return Err(resolve_error_code(
                Some(&import.span),
                ErrorCode::InvalidImport,
                format!("Unknown built-in module `{}`", import.path),
            ));
        };
        if !imported_paths.insert(import.path.as_str()) {
            return Err(resolve_error_code(
                Some(&import.span),
                ErrorCode::InvalidImport,
                format!("Duplicate import `{}`", import.path),
            ));
        }
        let root = import
            .alias
            .as_deref()
            .unwrap_or_else(|| import.path.rsplit('/').next().unwrap());
        for binding in bindings {
            let short = binding.name.rsplit('.').next().unwrap();
            scope
                .functions
                .insert(format!("{root}.{short}"), binding.name.clone());
            scope.imported_functions.insert(binding.name.clone());
            // Unqualified host registrations remain callable by their registered name.
            if import.alias.is_none() && !binding.name.contains('.') && import.members.is_empty() {
                if !scope.explicit_values.insert(binding.name.clone())
                    || scope.modules.contains_key(&binding.name)
                {
                    return Err(resolve_error_code(
                        Some(&import.span),
                        ErrorCode::DuplicateDefinition,
                        format!("Imported value '{}' is already defined", binding.name),
                    ));
                }
                scope.constructors.remove(&binding.name);
                scope
                    .functions
                    .insert(binding.name.clone(), binding.name.clone());
            }
        }
        if import.path == "joky/vm" {
            for form in ["call_int32", "register_int32"] {
                call_forms.insert(format!("{root}.{form}"), format!("vm.{form}"));
            }
        }
        for member in &import.members {
            if member.kind == ImportMemberKind::Type {
                continue;
            }
            let name = member.alias.as_deref().unwrap_or(&member.name);
            if !scope.explicit_values.insert(name.to_owned())
                || local_values.contains(name)
                || scope.modules.contains_key(name)
            {
                return Err(resolve_error_code(
                    Some(&member.span),
                    ErrorCode::DuplicateDefinition,
                    format!("Imported value '{name}' is already defined"),
                ));
            }
            super::integer::IntegerType::check_declaration(name, member.span)?;
            if import.path == "joky/vm"
                && matches!(member.name.as_str(), "call_int32" | "register_int32")
            {
                call_forms.insert(name.to_owned(), format!("vm.{}", member.name));
            } else {
                let binding = bindings
                    .iter()
                    .find(|b| b.name.rsplit('.').next() == Some(member.name.as_str()))
                    .ok_or_else(|| {
                        resolve_error_code(
                            Some(&member.span),
                            ErrorCode::InvalidImport,
                            format!(
                                "Module '{}' does not export value '{}'",
                                import.path, member.name
                            ),
                        )
                    })?;
                scope
                    .functions
                    .insert(name.to_owned(), binding.name.clone());
            }
            scope.constructors.remove(name);
        }
        imported_bindings.extend(bindings.iter().map(|binding| (binding, import.span)));
    }
    // These are the call forms of the VM module, not global language syntax.
    if imported_paths.contains("joky/vm") {
        let functions: HashMap<_, _> = resolved
            .items
            .iter()
            .filter_map(|item| match item {
                Item::FnDef(function) if function.type_params.is_empty() => Some((
                    function.name.clone(),
                    (
                        FunctionSignature::new(
                            function
                                .params
                                .iter()
                                .map(|param| param.ty.clone())
                                .collect(),
                            function.ret_ty.clone(),
                        ),
                        function.suspends,
                    ),
                )),
                _ => None,
            })
            .collect();
        let member_roots: HashSet<String> = resolved
            .enums
            .iter()
            .map(|d| d.name.as_str())
            .chain(
                resolved
                    .implementations
                    .iter()
                    .map(|d| d.target_name.as_str()),
            )
            .map(str::to_owned)
            .collect();
        for implementation in &mut resolved.implementations {
            for method in &mut implementation.methods {
                if method.scope.is_some() {
                    continue;
                }
                resolve_calls(
                    &mut method.body,
                    &functions,
                    &member_roots,
                    &call_forms,
                    &mut method.params.iter().map(|p| p.name.clone()).collect(),
                )?;
            }
        }
        for item in &mut resolved.items {
            if let Item::FnDef(function) = item {
                if function.scope.is_some() {
                    continue;
                }
                resolve_calls(
                    &mut function.body,
                    &functions,
                    &member_roots,
                    &call_forms,
                    &mut function.params.iter().map(|p| p.name.clone()).collect(),
                )?;
            }
        }
    }
    if !imported_bindings.is_empty() {
        let mut names: HashSet<_> = module
            .items
            .iter()
            .map(|item| match item {
                Item::FnDef(f) => f.name.as_str(),
                Item::ExternFn(f) => f.name.as_str(),
            })
            .collect();
        for (binding, import_span) in imported_bindings {
            if !names.insert(binding.name.as_str()) {
                // Separate source modules may import the same registered host.
                // Its canonical signature and native identity are shared.
                if module.items.iter().any(|item| {
                    matches!(item, Item::ExternFn(f)
                    if f.name == binding.name && !f.c_abi
                    && f.span.source_id() != import_span.source_id()
                    && f.ret_ty == binding.signature.ret
                    && f.suspends == binding.suspends
                    && f.params.iter().map(|p| &p.ty).eq(binding.signature.params.iter()))
                }) {
                    continue;
                }
                return Err(resolve_error_code(
                    Some(&import_span),
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate imported binding `{}`", binding.name),
                ));
            }
            resolved.items.push(Item::ExternFn(ExternFnDecl {
                c_abi: false,
                name: binding.name.clone(),
                params: binding
                    .signature
                    .params
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, ty)| Param {
                        name: format!("arg{index}"),
                        ty,
                        span: import_span,
                    })
                    .collect(),
                ret_ty: binding.signature.ret.clone(),
                suspends: binding.suspends,
                span: import_span,
            }));
        }
    }
    for binding in &scope.imported_functions {
        if binding.starts_with("vm.call_int32_") || binding.starts_with("vm.register_int32_") {
            scope
                .functions
                .insert(format!("joky/{binding}"), binding.clone());
        }
    }
    resolved.scope = Some(Arc::new(scope));
    resolved.imports.clear();

    Ok(resolved)
}

fn resolve_calls(
    expr: &mut Expr,
    functions: &HashMap<String, (FunctionSignature, bool)>,
    member_roots: &HashSet<String>,
    call_forms: &HashMap<String, String>,
    locals: &mut HashSet<String>,
) -> Result<(), Error> {
    match expr {
        Expr::Invoke { callee, args, .. } => {
            resolve_calls(callee, functions, member_roots, call_forms, locals)?;
            for arg in args {
                resolve_calls(arg, functions, member_roots, call_forms, locals)?;
            }
        }
        Expr::MethodCall(call) => {
            resolve_calls(
                &mut call.receiver,
                functions,
                member_roots,
                call_forms,
                locals,
            )?;
            for arg in &mut call.args {
                resolve_calls(arg, functions, member_roots, call_forms, locals)?;
            }
        }
        Expr::Match { value, arms, .. } => {
            resolve_calls(
                value,
                functions,
                member_roots,
                call_forms,
                &mut locals.clone(),
            )?;
            for arm in arms {
                let mut branch = locals.clone();
                if let crate::parser::ast::Pattern::Variant {
                    binding: Some(pattern),
                    ..
                } = &arm.pattern
                {
                    let mut names = Vec::new();
                    pattern.names(&mut names);
                    branch.extend(names);
                }
                resolve_calls(
                    &mut arm.body,
                    functions,
                    member_roots,
                    call_forms,
                    &mut branch,
                )?;
            }
        }

        Expr::Record { fields, .. } => {
            for field in fields {
                resolve_calls(
                    &mut field.value,
                    functions,
                    member_roots,
                    call_forms,
                    locals,
                )?;
            }
        }
        Expr::Field { base, .. } => {
            resolve_calls(base, functions, member_roots, call_forms, locals)?
        }
        Expr::FieldAssign { target, value, .. } => {
            resolve_calls(target, functions, member_roots, call_forms, locals)?;
            resolve_calls(value, functions, member_roots, call_forms, locals)?;
        }

        Expr::Call {
            callee,
            type_args,
            args,
            span,
        } => {
            // Local values and nominal member roots precede module call forms.
            // Receiver arguments can still contain ordinary built-in calls.
            let root = callee.split('.').next().unwrap();
            let member = member_roots.contains(root) || locals.contains(root);
            let form = (!member).then(|| call_forms.get(callee)).flatten();
            if !member && !type_args.is_empty() && form.is_some() {
                return Err(resolve_error(
                    Some(span),
                    "VM call forms do not accept type arguments",
                ));
            }
            if form.is_some_and(|f| f == "vm.call_int32") {
                if !(2..=4).contains(&args.len()) {
                    return Err(resolve_error(
                        Some(span),
                        "vm.call_int32 expects a handle, function name and zero to two Int32 arguments",
                    ));
                }
                *callee = format!("joky/vm.call_int32_{}", args.len() - 2);
            }
            if form.is_some_and(|f| f == "vm.register_int32") {
                if args.len() != 3 {
                    return Err(resolve_error(
                        Some(span),
                        "vm.register_int32 expects a handle, function name and function reference",
                    ));
                }
                let Expr::Ident {
                    name,
                    span: function_span,
                } = &args[2]
                else {
                    return Err(resolve_error(
                        Some(span),
                        "vm.register_int32 expects a function name as its third argument",
                    ));
                };
                let Some((signature, suspends)) = functions.get(name) else {
                    return Err(resolve_error(
                        Some(function_span),
                        format!(
                            "vm.register_int32 function `{name}` is not defined in this module"
                        ),
                    ));
                };
                if *suspends
                    || signature.ret != Type::Int32
                    || signature.params.len() > 2
                    || signature.params.iter().any(|ty| *ty != Type::Int32)
                {
                    return Err(resolve_error(
                        Some(function_span),
                        format!("vm.register_int32 function `{name}` must be a synchronous Int32 function with zero to two Int32 arguments"),
                    ));
                }
                *callee = format!("joky/vm.register_int32_{}", signature.params.len());
                args[2] = Expr::FunctionRef {
                    name: name.clone(),
                    span: *function_span,
                };
            }
            for arg in args {
                resolve_calls(arg, functions, member_roots, call_forms, locals)?;
            }
        }
        Expr::Let { name, value, .. } | Expr::Var { name, value, .. } => {
            resolve_calls(value, functions, member_roots, call_forms, locals)?;
            locals.insert(name.clone());
        }
        Expr::Destructure { pattern, value, .. } => {
            resolve_calls(value, functions, member_roots, call_forms, locals)?;
            let mut pending = vec![pattern];
            while let Some(pattern) = pending.pop() {
                match pattern {
                    crate::parser::ast::BindingPattern::Name { name, .. } => {
                        locals.insert(name.clone());
                    }
                    crate::parser::ast::BindingPattern::Tuple { elements, .. } => {
                        pending.extend(elements)
                    }
                    _ => {}
                }
            }
        }
        Expr::Assign { value, .. } | Expr::Unary { operand: value, .. } => {
            resolve_calls(value, functions, member_roots, call_forms, locals)?;
        }
        Expr::While {
            condition, body, ..
        } => {
            resolve_calls(
                condition,
                functions,
                member_roots,
                call_forms,
                &mut locals.clone(),
            )?;
            resolve_calls(
                body,
                functions,
                member_roots,
                call_forms,
                &mut locals.clone(),
            )?;
        }
        Expr::Unsafe { exprs, .. } | Expr::Block { exprs, .. } | Expr::Graph { exprs, .. } => {
            let mut block = locals.clone();
            for expr in exprs {
                resolve_calls(expr, functions, member_roots, call_forms, &mut block)?;
            }
        }
        Expr::Tuple {
            elements: exprs, ..
        }
        | Expr::Sequence { exprs, .. }
        | Expr::Scope {
            branches: exprs, ..
        } => {
            for expr in exprs {
                resolve_calls(expr, functions, member_roots, call_forms, locals)?;
            }
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            resolve_calls(
                condition,
                functions,
                member_roots,
                call_forms,
                &mut locals.clone(),
            )?;
            resolve_calls(
                then_branch,
                functions,
                member_roots,
                call_forms,
                &mut locals.clone(),
            )?;
            if let Some(branch) = else_branch {
                resolve_calls(
                    branch,
                    functions,
                    member_roots,
                    call_forms,
                    &mut locals.clone(),
                )?;
            }
        }
        Expr::Logical { lhs, rhs, .. } => {
            resolve_calls(
                lhs,
                functions,
                member_roots,
                call_forms,
                &mut locals.clone(),
            )?;
            resolve_calls(
                rhs,
                functions,
                member_roots,
                call_forms,
                &mut locals.clone(),
            )?;
        }
        Expr::BinOp { lhs, rhs, .. } => {
            resolve_calls(lhs, functions, member_roots, call_forms, locals)?;
            resolve_calls(rhs, functions, member_roots, call_forms, locals)?;
        }
        Expr::Return {
            value: Some(value), ..
        } => resolve_calls(value, functions, member_roots, call_forms, locals)?,
        Expr::GenericName(_)
        | Expr::Literal { .. }
        | Expr::Ident { .. }
        | Expr::FunctionRef { .. }
        | Expr::Return { value: None, .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
    }
    Ok(())
}

fn resolve_error(span: Option<&Span>, message: impl Into<String>) -> Error {
    let error = Error::new_with_code(ErrorPhase::Resolve, ErrorCode::InvalidCall, message);
    span.map_or(error.clone(), |span| error.with_span(*span))
}

fn resolve_error_code(span: Option<&Span>, code: ErrorCode, message: impl Into<String>) -> Error {
    let error = Error::new_with_code(ErrorPhase::Resolve, code, message);
    span.map_or(error.clone(), |span| error.with_span(*span))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_conversion_names_cannot_be_registered_as_module_bindings() {
        let mut registry = BindingRegistry::default();
        for name in ["int32", "int64", "uint32", "uint64"] {
            let error = registry
                .register_module(
                    "user/conversions",
                    [BindingSpec {
                        name: name.into(),
                        signature: FunctionSignature::new(vec![Type::Int32], Type::Int32),
                        symbol: None,
                        suspends: false,
                    }],
                )
                .unwrap_err();
            assert_eq!(error.code(), ErrorCode::ReservedName);
            assert!(registry.module("user/conversions").is_none());
            assert!(registry.binding(name).is_none());
        }
    }

    #[test]
    fn rewrites_builtin_calls_without_stdlib_ast_logic() {
        let module =
            crate::parser::parse("import joky/vm fn main() { vm.call_int32(1, \"value\", 42) }")
                .unwrap();
        let mut registry = BindingRegistry::default();
        registry
            .register_module(
                "joky/vm",
                [BindingSpec {
                    name: "vm.call_int32_1".into(),
                    signature: FunctionSignature::new(
                        vec![Type::Named("vm.Handle".into()), Type::String, Type::Int32],
                        Type::Int32,
                    ),
                    symbol: None,
                    suspends: false,
                }],
            )
            .unwrap();
        let resolved = resolve(&module, &registry).unwrap();
        assert!(resolved.imports.is_empty());
        let Item::FnDef(function) = &resolved.items[0] else {
            panic!("expected function definition");
        };
        let Expr::Block {
            exprs: expressions, ..
        } = &function.body
        else {
            panic!("expected function block");
        };
        let Expr::Call { callee, .. } = &expressions[0] else {
            panic!("expected call expression");
        };
        assert_eq!(callee, "joky/vm.call_int32_1");
        assert_eq!(resolved.items.len(), 2);
    }

    #[test]
    fn reports_builtin_call_errors_at_the_call_expression() {
        let source = "import joky/vm fn main() { vm.call_int32(1) }";
        let module = crate::parser::parse(source).unwrap();
        let mut registry = BindingRegistry::default();
        registry.register_module("joky/vm", []).unwrap();
        let error = resolve(&module, &registry).unwrap_err();
        let start = source.find("vm.call_int32").unwrap();
        let end = source[start..].find(')').unwrap() + start + 1;
        assert_eq!(error.phase(), ErrorPhase::Resolve);
        assert_eq!(error.code(), ErrorCode::InvalidCall);
        assert_eq!(error.span(), Some(Span::new(start, end)));
        assert_eq!(
            error.span().unwrap().slice(source),
            Some("vm.call_int32(1)")
        );
    }

    #[test]
    fn resolves_bindings_from_custom_registry() {
        let module = crate::parser::parse("import user/math fn main() { user.add(1, 2) }").unwrap();
        let mut registry = BindingRegistry::default();
        registry
            .register_module(
                "user/math",
                [BindingSpec {
                    name: "user.add".into(),
                    signature: FunctionSignature::new(vec![Type::Int32, Type::Int32], Type::Int32),
                    symbol: None,
                    suspends: false,
                }],
            )
            .unwrap();

        let resolved = resolve(&module, &registry).unwrap();
        assert!(resolved.imports.is_empty());
        assert!(resolved.items.iter().any(|item| {
            matches!(item, Item::ExternFn(function) if function.name == "user.add")
        }));
    }
}
