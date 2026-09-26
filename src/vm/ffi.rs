// C API（阶段4）
// 为宿主程序提供 C ABI 兼容接口，使 AOT 编译的宿主代码可以调用 VM。
// 此处以 Rust 测试代替真正的 C FFI 层；完整 C 头文件见下方注释。

/*
  生成的 C 头文件（jk_vm.h）：

  typedef struct JkVmEngine JkVmEngine;

  JkVmEngine* jk_vm_create(void);
  void        jk_vm_destroy(JkVmEngine* vm);
  int         jk_vm_load(JkVmEngine* vm, const char* source);
  int         jk_vm_register_fn(JkVmEngine* vm, const char* name, void* fn_ptr);
  int32_t     jk_vm_call_int32_2(JkVmEngine* vm, const char* fn_name,
                                  int32_t a, int32_t b);
  int32_t     jk_vm_call_int32_1(JkVmEngine* vm, const char* fn_name,
                                  int32_t a);
*/
