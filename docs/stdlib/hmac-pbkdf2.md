# HMAC-SHA-256 与 PBKDF2

两个模块均以纯 Joky 实现，基于 [SHA-256](sha256.md)，不调用 native 密码学库。
密钥、消息、密码、盐和结果均为原始 `Bytes`，支持空输入和包含 NUL 的二进制数据。

## HMAC-SHA-256

`import joky/crypto/hmac_sha256`。

| API | 返回值 | 语义 |
| --- | --- | --- |
| `hmac_sha256.digest(key: Bytes, data: Bytes)` | `Result(Bytes, String)` | 一次性计算 32 字节 MAC |
| `hmac_sha256.new(key: Bytes)` | `Result(HmacSha256, String)` | 预处理密钥，创建可复用状态 |
| `state.digest(&self, data: Bytes)` | `Result(Bytes, String)` | 使用预处理密钥计算独立消息的 MAC，不修改状态 |

超过 64 字节的密钥先做 SHA-256，短密钥补零到 64 字节；内外层 pad 分别使用
`0x36` 和 `0x5c`。状态保存处理完 pad 后的两个 SHA-256 前缀，每次计算复制前缀，
因此复用密钥时无需重新压缩 pad。通过 `new` 创建状态，字段和构造参数属于实现细节。

密钥最大长度为 `2^61 - 1` 字节，消息最大长度为 `2^61 - 65` 字节
（SHA-256 总长度包含 64 字节 pad）；超出 SHA-256 长度限制返回 `Err`。
输出始终为完整 32 字节，模块不提供 MAC 验证或恒定时间比较接口，也不保证密钥内存清零。

## PBKDF2-HMAC-SHA-256

`import joky/crypto/pbkdf2`。

```joky
pub fn hmac_sha256(password: Bytes, salt: Bytes, iterations: UInt32, length: UInt64) -> Result(Bytes, String)
```

返回恰好 `length` 字节的派生密钥。参数约束：

- `iterations` 必须大于零；API 支持至 `2^32 - 1` 次迭代。
- `length` 必须位于 `1..137438953440`，即最多 `(2^32 - 1) * 32` 字节。
- 盐最多 `2^61 - 69` 字节，为 HMAC pad 和四字节块编号留出空间。
- 密码遵循 HMAC 的密钥长度限制；模块不做字符串编码、Unicode 规范化或 SASLprep。

迭代次数、输出长度和盐长度在派生前检查，非法参数返回 `Err`。
块编号从 1 开始，以四字节大端序编码；每块对全部迭代结果做 XOR，最后一块按需要截取。
一次调用复用同一份 HMAC 密钥状态。结果存放在内存中，理论长度上限不保证分配成功；
运行时间与迭代次数及输出块数成正比，调用方应按协议和资源预算限制参数。
函数同步计算，不提供取消或 deadline。

## 示例与验证

```joky
import joky/crypto/hmac_sha256
import joky/crypto/pbkdf2

fn main() -> Result(Unit, String) {
    let key = hmac_sha256.new(b"example key")?
    println(key.digest(b"first message")?.debug())
    println(key.digest(b"second message")?.debug())
    let derived = pbkdf2.hmac_sha256(b"password", b"salt", 4096, 32)?
    println(derived.debug())
    Ok(())
}
```

完整可运行[示例](../../examples/basics/crypto.jk)同时展示 Base64、增量 SHA-256、
HMAC 和 PBKDF2。示例中的密码、盐及迭代次数仅用于演示，不构成密码存储参数建议。
随机盐可使用 [OS 安全随机数](random.md)；SCRAM 认证见 [PostgreSQL 待办](../plans/backlog.md#postgresql-驱动)。

[HMAC 测试](../../tests/fixtures/hmac_sha256.jk)覆盖 RFC 4231 的七组向量、空输入、
密钥块边界及重复使用密钥。[PBKDF2 测试](../../tests/fixtures/pbkdf2.jk)覆盖
RFC 7914 第 11 节首组向量、1/2/4096 次迭代、多块与部分块输出、二进制密码、
盐的填充边界、块编号 255/256 的进位和非法参数。额外固定期望值来自 Python 标准库 `hmac` / `hashlib`，
运行测试无需 Python 密码学支持。

[CLI 测试](../../tests/cli/crypto.rs)还使用 Rust `sha2` 独立验证 SHA-256 状态复制后的分支结果。

```bash
JOKY_TEST_AOT=full cargo test --test cli crypto
```

该命令覆盖 JIT、debug AOT、release AOT；HMAC、PBKDF2 向量与状态复制也覆盖缓存 JIT。
AOT runtime 准备方法见[开发指南](../../CONTRIBUTING.md#运行测试)。

算法依据：[RFC 2104](https://www.rfc-editor.org/rfc/rfc2104.html)、
[RFC 8018 §5.2](https://www.rfc-editor.org/rfc/rfc8018.html#section-5.2)；
标准向量：[RFC 4231](https://www.rfc-editor.org/rfc/rfc4231.html)、
[RFC 7914 §11](https://www.rfc-editor.org/rfc/rfc7914.html#section-11)。
