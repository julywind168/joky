//! Built-in import and call resolution over the parsed AST.
//!
//! Host-module calls are rewritten after source-library expansion and type
//! normalization. Local scopes take precedence over host module names.
use crate::{
    parser::ast::{Expr, ExternFnDecl, Item, Module, Param},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, Type,
};
use std::collections::{HashMap, HashSet};

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
        if path == "joky/integer" || self.modules.contains_key(&path) {
            return Err(resolve_error_code(
                None,
                ErrorCode::DuplicateDefinition,
                format!("Module `{path}` is already registered"),
            ));
        }
        let bindings: Vec<_> = bindings.into_iter().collect();
        let mut names = HashSet::new();
        let mut symbols: HashSet<_> = self
            .modules
            .values()
            .flatten()
            .filter_map(|b| b.symbol.as_deref())
            .collect();
        for binding in &bindings {
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
            if signature != binding.signature || f.suspends != binding.suspends {
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

/// Expand explicit source-library imports before resolving module-local types.
/// Standard declarations remain ordinary Joky source and keep one nominal
/// identity for the compilation that owns the expanded module.
pub(super) fn expand_sources(module: &Module) -> Result<std::borrow::Cow<'_, Module>, Error> {
    let source_paths = ["joky/integer", "joky/option", "joky/result"];
    if !module
        .imports
        .iter()
        .any(|import| source_paths.contains(&import.path.as_str()))
    {
        return Ok(std::borrow::Cow::Borrowed(module));
    }
    if module
        .imports
        .iter()
        .any(|import| import.path == "joky/integer")
        && module
            .imports
            .iter()
            .any(|import| import.path == "joky/option")
    {
        return Err(resolve_error_code(
            module
                .imports
                .iter()
                .find(|import| import.path == "joky/option")
                .map(|import| &import.span),
            ErrorCode::DuplicateDefinition,
            "`joky/integer` already provides the standard `Option` enum",
        ));
    }
    let mut result = module.clone();
    let mut seen = HashSet::new();
    for import in &module.imports {
        if !source_paths.contains(&import.path.as_str()) {
            continue;
        }
        if !seen.insert(import.path.as_str()) {
            return Err(crate::Error::new_with_code(
                crate::ErrorPhase::Resolve,
                crate::ErrorCode::InvalidImport,
                format!("Duplicate import `{}`", import.path),
            )
            .with_span(import.span));
        }
        let source = match import.path.as_str() {
            "joky/integer" => include_str!("../stdlib/integer.jk"),
            "joky/option" => include_str!("../stdlib/option.jk"),
            "joky/result" => include_str!("../stdlib/result.jk"),
            _ => unreachable!(),
        };
        let library = crate::parser::parse_import(source, import.span)?;
        validate_source_members(import, &library)?;
        for declaration in &library.enums {
            if module
                .enums
                .iter()
                .any(|item| item.name == declaration.name)
            {
                return Err(resolve_error_code(
                    Some(&import.span),
                    ErrorCode::DuplicateDefinition,
                    format!(
                        "Source module `{}` exports enum `{}` which conflicts with a local declaration",
                        import.path, declaration.name
                    ),
                ));
            }
        }
        result.enums.extend(library.enums);
        result.implementations.extend(library.implementations);
        // Integer methods return Option, so importing the integer library also
        // makes the standard Option declaration available to qualified users.
        if import.path == "joky/integer" {
            let option =
                crate::parser::parse_import(include_str!("../stdlib/option.jk"), import.span)?;
            if module.enums.iter().any(|item| item.name == "Option") {
                return Err(resolve_error_code(
                    Some(&import.span),
                    ErrorCode::DuplicateDefinition,
                    "Enum `Option` is already defined; `joky/integer` uses the standard declaration",
                ));
            }
            if result.enums.iter().all(|item| item.name != "Option") {
                result.enums.extend(option.enums);
            }
        }
    }
    result.source_imports.extend(
        result
            .imports
            .iter()
            .filter(|import| source_paths.contains(&import.path.as_str()))
            .cloned(),
    );
    result
        .imports
        .retain(|import| !source_paths.contains(&import.path.as_str()));
    Ok(std::borrow::Cow::Owned(result))
}

fn validate_source_members(
    import: &crate::parser::ast::Import,
    library: &Module,
) -> Result<(), Error> {
    if import.members.is_empty() {
        return Ok(());
    }
    let types: HashSet<_> = library
        .enums
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    let values: HashSet<_> = library
        .enums
        .iter()
        .flat_map(|item| item.variants.iter().map(|variant| variant.name.as_str()))
        .collect();
    for member in &import.members {
        let found = match member.kind {
            crate::parser::ast::ImportMemberKind::Type => types.contains(member.name.as_str()),
            crate::parser::ast::ImportMemberKind::Value => values.contains(member.name.as_str()),
        };
        if !found {
            return Err(resolve_error_code(
                Some(&member.span),
                ErrorCode::InvalidImport,
                format!("Module `{}` does not export `{}`", import.path, member.name),
            ));
        }
    }
    Ok(())
}

/// Resolve imported modules and rewrite their built-in call forms.
pub(crate) fn resolve(module: &Module, registry: &BindingRegistry) -> Result<Module, Error> {
    let mut resolved = module.clone();
    let constructor_aliases = source_constructor_aliases(module)?;
    for import in module.imports.iter().chain(&module.source_imports) {
        if import.alias.is_some() {
            return Err(resolve_error_code(
                Some(&import.span),
                ErrorCode::InvalidImport,
                "Module aliases are not supported by this compilation phase yet",
            ));
        }
    }
    let mut imported_paths = HashSet::new();
    let mut imported_bindings = Vec::new();
    for import in module.imports.iter().chain(&module.source_imports) {
        if is_source_library(&import.path) {
            continue;
        }
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
        imported_bindings.extend(bindings.iter().map(|binding| (binding, import.span)));
    }
    for implementation in &mut resolved.implementations {
        resolve_constructor_aliases(&mut implementation.methods, &constructor_aliases)?;
    }
    for item in &mut resolved.items {
        if let Item::FnDef(function) = item {
            resolve_constructor_aliases_in_expr(
                &mut function.body,
                &constructor_aliases,
                &mut function.params.iter().map(|p| p.name.clone()).collect(),
            )?;
        }
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
                resolve_calls(
                    &mut method.body,
                    &functions,
                    &member_roots,
                    &mut method.params.iter().map(|p| p.name.clone()).collect(),
                )?;
            }
        }
        for item in &mut resolved.items {
            if let Item::FnDef(function) = item {
                resolve_calls(
                    &mut function.body,
                    &functions,
                    &member_roots,
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
                return Err(resolve_error_code(
                    Some(&import_span),
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate imported binding `{}`", binding.name),
                ));
            }
            resolved.items.push(Item::ExternFn(ExternFnDecl {
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
    resolved.imports.clear();
    resolved.source_imports.clear();
    Ok(resolved)
}

fn is_source_library(path: &str) -> bool {
    matches!(path, "joky/integer" | "joky/option" | "joky/result")
}

fn source_constructor_aliases(module: &Module) -> Result<HashMap<String, String>, Error> {
    let mut aliases = HashMap::new();
    for import in module.imports.iter().chain(&module.source_imports) {
        let enum_name = match import.path.as_str() {
            "joky/option" => "Option",
            "joky/result" => "Result",
            _ => continue,
        };
        for member in &import.members {
            if member.kind != crate::parser::ast::ImportMemberKind::Value {
                continue;
            }
            let name = member.alias.as_deref().unwrap_or(&member.name);
            if aliases
                .insert(name.to_owned(), format!("{enum_name}.{}", member.name))
                .is_some()
            {
                return Err(resolve_error_code(
                    Some(&member.span),
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate imported constructor `{name}`"),
                ));
            }
        }
    }
    let top_level: HashSet<_> = module
        .items
        .iter()
        .map(|item| match item {
            Item::FnDef(function) => function.name.as_str(),
            Item::ExternFn(function) => function.name.as_str(),
        })
        .collect();
    if let Some(name) = aliases
        .keys()
        .find(|name| top_level.contains(name.as_str()))
    {
        return Err(resolve_error_code(
            None,
            ErrorCode::DuplicateDefinition,
            format!("Imported constructor `{name}` conflicts with a top-level declaration"),
        ));
    }
    Ok(aliases)
}

fn resolve_constructor_aliases(
    methods: &mut [crate::parser::ast::FnDef],
    aliases: &HashMap<String, String>,
) -> Result<(), Error> {
    for method in methods {
        let mut locals = method
            .params
            .iter()
            .map(|param| param.name.clone())
            .collect();
        resolve_constructor_aliases_in_expr(&mut method.body, aliases, &mut locals)?;
    }
    Ok(())
}

fn resolve_constructor_aliases_in_expr(
    expr: &mut Expr,
    aliases: &HashMap<String, String>,
    locals: &mut HashSet<String>,
) -> Result<(), Error> {
    match expr {
        Expr::MethodCall(call) => {
            resolve_constructor_aliases_in_expr(&mut call.receiver, aliases, locals)?;
            for arg in &mut call.args {
                resolve_constructor_aliases_in_expr(arg, aliases, locals)?;
            }
        }
        Expr::Match { value, arms, .. } => {
            resolve_constructor_aliases_in_expr(value, aliases, &mut locals.clone())?;
            for arm in arms {
                let mut branch = locals.clone();
                if let crate::parser::ast::Pattern::Variant {
                    binding: Some(name),
                    ..
                } = &arm.pattern
                {
                    branch.insert(name.clone());
                }
                resolve_constructor_aliases_in_expr(&mut arm.body, aliases, &mut branch)?;
            }
        }
        Expr::Record { fields, .. } => {
            for field in fields {
                resolve_constructor_aliases_in_expr(&mut field.value, aliases, locals)?;
            }
        }
        Expr::Field { base, .. } => {
            resolve_constructor_aliases_in_expr(base, aliases, locals)?;
        }
        Expr::FieldAssign { target, value, .. } => {
            resolve_constructor_aliases_in_expr(target, aliases, locals)?;
            resolve_constructor_aliases_in_expr(value, aliases, locals)?;
        }
        Expr::Call { callee, args, .. } => {
            if !callee.contains('.') && !locals.contains(callee) {
                if let Some(qualified) = aliases.get(callee) {
                    *callee = qualified.clone();
                }
            }
            for arg in args {
                resolve_constructor_aliases_in_expr(arg, aliases, locals)?;
            }
        }
        Expr::Ident { name, span } => {
            if !locals.contains(name) {
                if let Some(qualified) = aliases.get(name) {
                    let Some((root, variant)) = qualified.split_once('.') else {
                        unreachable!()
                    };
                    let original = *span;
                    *expr = Expr::Field {
                        base: Box::new(Expr::Ident {
                            name: root.to_owned(),
                            span: original,
                        }),
                        name: variant.to_owned(),
                        type_args: Vec::new(),
                        span: original,
                    };
                }
            }
        }
        Expr::Let { name, value, .. } | Expr::Var { name, value, .. } => {
            resolve_constructor_aliases_in_expr(value, aliases, locals)?;
            locals.insert(name.clone());
        }
        Expr::Destructure { pattern, value, .. } => {
            resolve_constructor_aliases_in_expr(value, aliases, locals)?;
            let mut pending = vec![pattern];
            while let Some(pattern) = pending.pop() {
                match pattern {
                    crate::parser::ast::BindingPattern::Name { name, .. } => {
                        locals.insert(name.clone());
                    }
                    crate::parser::ast::BindingPattern::Tuple { elements, .. } => {
                        pending.extend(elements)
                    }
                    crate::parser::ast::BindingPattern::Discard { .. } => {}
                }
            }
        }
        Expr::Assign { value, .. } | Expr::Unary { operand: value, .. } => {
            resolve_constructor_aliases_in_expr(value, aliases, locals)?;
        }
        Expr::While {
            condition, body, ..
        } => {
            resolve_constructor_aliases_in_expr(condition, aliases, &mut locals.clone())?;
            resolve_constructor_aliases_in_expr(body, aliases, &mut locals.clone())?;
        }
        Expr::Block { exprs, .. } => {
            let mut block = locals.clone();
            for expr in exprs {
                resolve_constructor_aliases_in_expr(expr, aliases, &mut block)?;
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
                resolve_constructor_aliases_in_expr(expr, aliases, locals)?;
            }
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            resolve_constructor_aliases_in_expr(condition, aliases, &mut locals.clone())?;
            resolve_constructor_aliases_in_expr(then_branch, aliases, &mut locals.clone())?;
            if let Some(branch) = else_branch {
                resolve_constructor_aliases_in_expr(branch, aliases, &mut locals.clone())?;
            }
        }
        Expr::Logical { lhs, rhs, .. } | Expr::BinOp { lhs, rhs, .. } => {
            resolve_constructor_aliases_in_expr(lhs, aliases, locals)?;
            resolve_constructor_aliases_in_expr(rhs, aliases, locals)?;
        }
        Expr::Return {
            value: Some(value), ..
        } => resolve_constructor_aliases_in_expr(value, aliases, locals)?,
        Expr::Literal { .. }
        | Expr::FunctionRef { .. }
        | Expr::Return { value: None, .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
    }
    Ok(())
}

fn resolve_calls(
    expr: &mut Expr,
    functions: &HashMap<String, (FunctionSignature, bool)>,
    member_roots: &HashSet<String>,
    locals: &mut HashSet<String>,
) -> Result<(), Error> {
    match expr {
        Expr::MethodCall(call) => {
            resolve_calls(&mut call.receiver, functions, member_roots, locals)?;
            for arg in &mut call.args {
                resolve_calls(arg, functions, member_roots, locals)?;
            }
        }
        Expr::Match { value, arms, .. } => {
            resolve_calls(value, functions, member_roots, &mut locals.clone())?;
            for arm in arms {
                let mut branch = locals.clone();
                if let crate::parser::ast::Pattern::Variant {
                    binding: Some(name),
                    ..
                } = &arm.pattern
                {
                    branch.insert(name.clone());
                }
                resolve_calls(&mut arm.body, functions, member_roots, &mut branch)?;
            }
        }

        Expr::Record { fields, .. } => {
            for field in fields {
                resolve_calls(&mut field.value, functions, member_roots, locals)?;
            }
        }
        Expr::Field { base, .. } => resolve_calls(base, functions, member_roots, locals)?,
        Expr::FieldAssign { target, value, .. } => {
            resolve_calls(target, functions, member_roots, locals)?;
            resolve_calls(value, functions, member_roots, locals)?;
        }

        Expr::Call {
            callee,
            type_args,
            args,
            span,
        } => {
            // Local values and nominal member roots precede module call forms.
            // Receiver arguments can still contain ordinary built-in calls.
            let member = callee
                .split_once('.')
                .is_some_and(|(root, _)| member_roots.contains(root) || locals.contains(root));
            if !member
                && !type_args.is_empty()
                && matches!(callee.as_str(), "vm.call_int32" | "vm.register_int32")
            {
                return Err(resolve_error(
                    Some(span),
                    "VM call forms do not accept type arguments",
                ));
            }
            if !member && callee == "vm.call_int32" {
                if !(2..=4).contains(&args.len()) {
                    return Err(resolve_error(
                        Some(span),
                        "vm.call_int32 expects a handle, function name and zero to two Int32 arguments",
                    ));
                }
                *callee = format!("vm.call_int32_{}", args.len() - 2);
            }
            if !member && callee == "vm.register_int32" {
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
                *callee = format!("vm.register_int32_{}", signature.params.len());
                args[2] = Expr::FunctionRef {
                    name: name.clone(),
                    span: *function_span,
                };
            }
            for arg in args {
                resolve_calls(arg, functions, member_roots, locals)?;
            }
        }
        Expr::Let { name, value, .. } | Expr::Var { name, value, .. } => {
            resolve_calls(value, functions, member_roots, locals)?;
            locals.insert(name.clone());
        }
        Expr::Destructure { pattern, value, .. } => {
            resolve_calls(value, functions, member_roots, locals)?;
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
            resolve_calls(value, functions, member_roots, locals)?;
        }
        Expr::While {
            condition, body, ..
        } => {
            resolve_calls(condition, functions, member_roots, &mut locals.clone())?;
            resolve_calls(body, functions, member_roots, &mut locals.clone())?;
        }
        Expr::Block { exprs, .. } => {
            let mut block = locals.clone();
            for expr in exprs {
                resolve_calls(expr, functions, member_roots, &mut block)?;
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
                resolve_calls(expr, functions, member_roots, locals)?;
            }
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            resolve_calls(condition, functions, member_roots, &mut locals.clone())?;
            resolve_calls(then_branch, functions, member_roots, &mut locals.clone())?;
            if let Some(branch) = else_branch {
                resolve_calls(branch, functions, member_roots, &mut locals.clone())?;
            }
        }
        Expr::Logical { lhs, rhs, .. } => {
            resolve_calls(lhs, functions, member_roots, locals)?;
            resolve_calls(rhs, functions, member_roots, &mut locals.clone())?;
        }
        Expr::BinOp { lhs, rhs, .. } => {
            resolve_calls(lhs, functions, member_roots, locals)?;
            resolve_calls(rhs, functions, member_roots, locals)?;
        }
        Expr::Return {
            value: Some(value), ..
        } => resolve_calls(value, functions, member_roots, locals)?,
        Expr::Literal { .. }
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
        assert_eq!(callee, "vm.call_int32_1");
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
