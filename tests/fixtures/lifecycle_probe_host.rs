// Host-only lifetime ledger. Release reads its own token, never Joky storage.
unsafe extern "C" fn create_probe(id: u64, output: *mut *mut std::ffi::c_void) -> i32 {
    *output = Box::into_raw(Box::new(id as i32)).cast();
    record(-(id as i32));
    0
}
unsafe extern "C" fn release_probe(token: *mut std::ffi::c_void) {
    let id = *Box::from_raw(token.cast::<i32>());
    record(id);
}
#[export_name = "__joky_resource_factory_v1_lifecycle_probe"]
pub static PROBE: joky::runtime::resource::ResourceFactoryV1 = joky::runtime::resource::ResourceFactoryV1 {
    abi_version: 1,
    create: Some(create_probe),
    release: Some(release_probe),
};
