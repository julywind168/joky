use super::*;

impl CraneliftBackend {
    pub(crate) fn new() -> Result<Self, CodegenError> {
        let mut flag_builder = settings::builder();
        flag_builder
            .set("use_colocated_libcalls", "false")
            .map_err(codegen_error)?;
        flag_builder.set("is_pic", "false").map_err(codegen_error)?;
        // Joky-to-Joky calls flatten aggregate results, including Result(Output,
        // String). Keep this private convention in sync with the object backend.
        // Cranelift deprecates implicit sret; replace it with explicit result
        // storage before upgrading to a backend version that removes this flag.
        flag_builder
            .set("enable_multi_ret_implicit_sret", "true")
            .map_err(codegen_error)?;

        let isa_builder = cranelift_native::builder().map_err(codegen_error)?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(codegen_error)?;
        let mut jit_builder = JITBuilder::with_isa(isa, default_libcall_names());
        joky_runtime::host::visit_jit_symbols(|name, address| {
            jit_builder.symbol(name, address);
        });

        Ok(Self {
            debug_info: None,
            module: JITModule::new(jit_builder),
            function_context: FunctionBuilderContext::new(),
            next_function: 0,
            string_literals: Vec::new(),
            foreign_libraries: HashMap::new(),
            continuation_entry_keys: HashMap::new(),
            map_key_adapters: HashMap::new(),
            aot_machine_entries: Vec::new(),
            registered_machine_scopes: Vec::new(),
            pending_machine_functions: Vec::new(),
        })
    }
}
