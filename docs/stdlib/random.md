# 系统安全随机数

`import joky/crypto/random`。调用者声明 `effects { random }`，或用本地 handler
处理随机数请求。模块提供一个 `@suspends` operation：

```joky
fn bytes(length: UInt64) -> Result(Bytes, String)
```

默认 provider 从操作系统的密码学随机源取得恰好 `length` 个字节，适合生成 nonce、
盐和密钥。它使用 `getrandom 0.4.3` 的系统后端，不维护用户态伪随机状态，不接受种子，
也不在系统调用失败时退回时间戳或其他弱随机源。SHA-256、HMAC、PBKDF2 和 PostgreSQL
协议仍由 Joky 代码实现；访问 OS 随机源属于 runtime 的职责。

## 输入与错误

- `length` 范围为 `0..1048576`，单次最多 1 MiB，以限制每个请求的分配与系统调用工作量。
- 零长度返回空 `Bytes`，不调用 OS 随机源。
- 超出上限时在分配数据缓冲区和调用随机源之前返回 `Err`。
- OS 随机源失败时返回带原因的 `Err`；部分填充的数据会丢弃，不返回短结果或补零结果。

输出没有文本编码，需要文本 nonce 时可使用 [Base64](base64.md)。
例如 24 个随机字节编码后恰好是 32 个 Base64 字符：

```joky
import joky/crypto/random
import joky/encoding/base64

fn main() -> Result(Unit, String) effects { random } {
    println(base64.encode(random.bytes(24)?))
    Ok(())
}
```

完整[示例](../../examples/basics/random.jk)可直接运行，每次输出不同的随机值。

## 挂起、取消与测试替身

OS 随机源可能在系统启动早期阻塞，因此调用放入 blocking pool；调度 worker
不会执行随机源 syscall。排队时只保存长度、continuation 和 scope，工作开始后才分配缓冲区。
取消会移除排队请求；已经开始的系统调用可能继续执行，但不会恢复已取消的 Joky 代码，
晚到结果会被释放。scope 关闭会等待在途请求清理完成，因此也可能等待 OS 调用返回。

与其他 effect 一样，本地 handler 优先于默认 provider，允许测试固定 nonce 或模拟失败：

```joky
import joky/crypto/random

fn request() -> Result(Bytes, String) effects { random } { random.bytes(4) }

fn main() {
    let result = do { request() } with {
        random.bytes(_) => Ok(b"test")
    }
    println(result!.to_string()!)
}
```

handler 完全替代默认实现，其返回值与随机性由 handler 负责。

## 平台与验证

`getrandom` 默认后端在 macOS 使用 `getentropy`，Linux 使用系统 `getrandom` 或
该库检查熵源就绪后的兼容路径，Windows 10+ 使用 `ProcessPrng`。本次运行验证为
ARM64 macOS；其他平台的 Joky 可执行文件支持边界见 [AOT 契约](../compiler/aot.md)。

新增 provider 后 runtime ABI 为 v27；旧 AOT runtime archive 需要重新构建：

```sh
cargo build --release --manifest-path crates/joky-runtime/Cargo.toml --target-dir target
JOKY_TEST_AOT=full cargo test --test cli crypto_random
cargo test --manifest-path crates/joky-runtime/Cargo.toml runtime::random
```

[CLI 测试](../../tests/cli/random.rs)覆盖 JIT、缓存 JIT、debug/release AOT、任务并发、
长度边界、effect 检查和 handler 替身，并检查运行后的资源回收。
[runtime 测试](../../crates/joky-runtime/src/runtime/random/tests.rs)通过注入确定性填充函数
验证二进制数据、部分失败、非法 ABI、取消先于准入及执行中取消。
随机性的来源由系统 API 保证；测试不使用“两个结果必须不同”之类的概率断言。

参考：[getrandom 官方文档](https://docs.rs/getrandom/0.4.3/getrandom/)。
