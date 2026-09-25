# SASLprep 与 PostgreSQL 密码准备

本模块提供纯 Joky 的 UTF-8 scalar 编解码、固定 Unicode 3.2 NFKC、
RFC 4013 stored-string SASLprep，以及 PostgreSQL 密码字节兼容入口。
运行时不调用 Python、ICU、libpq 或 native Unicode 函数。Unicode 数据通过通用的
[编译期资源嵌入](../lang/values.md#编译期资源嵌入)进入 JIT / AOT；Bytes 构造使用 runtime ABI v28。
该模块只负责密码准备；[pgsql.connect](pgsql.md) 已将其接入 SCRAM 连接认证。

## 入口

| 模块与函数 | 返回值 | 用途 |
| --- | --- | --- |
| `joky/unicode/nfkc32.normalize(input: Bytes, limits: Limits)` | `Result(Bytes, utf8.Error)` | Unicode 3.2 NFKC；拒绝非法 UTF-8，但允许合法的未分配码点 |
| `joky/crypto/saslprep.prepare(input: Bytes, limits: nfkc32.Limits)` | `Result(Bytes, saslprep.Error)` | stored-string profile；未分配码点是错误 |
| `joky/pgsql/password.prepare(input: Bytes, limits: nfkc32.Limits)` | `Result(Bytes, password.Error)` | PostgreSQL SCRAM 密码准备，包含原字节回退 |

```joky
import joky/pgsql/password
import joky/unicode/nfkc32

fn prepared(raw: Bytes) -> Result(Bytes, password.Error) {
    password.prepare(raw, nfkc32.Limits())
}
```

准备好的 Bytes 可传入 [SCRAM 核心](scram-sha256.md)的
`first.respond(server_first, prepared_password)`。上层须同时满足这里的
`max_output_bytes` 和 SCRAM 的 `max_message`；两处限制独立，后者默认 16384。

低层 `utf8.decode(data, max_scalars)` 返回拥有型 `MutList(UInt32)`，
`utf8.encode(&scalars, max_bytes)` 返回 Bytes，
`utf8.append(&output, code, max_bytes)` 校验单个 scalar 后追加。
拒绝 overlong、孤立 continuation、截断、surrogate 和大于 U+10FFFF 的编码，
不插入替代字符；append 的单次失败不会部分写入该 scalar。
`nfkc32.normalize_scalars(&input, limits)` 可用于内部 profile 组合，
此入口仅应用 scalar/reordering 限制，字节限制由 decode/encode 调用方负责。
`tables32`、`nfkc32.read/in_ranges` 和 `saslprep.prepare_postgres` 是跨模块实现接口，
不作为稳定的应用 API；应用使用上列三个入口。

## 固定 Unicode 3.2

严格入口依次执行：UTF-8 校验、B.1 删除、C.1.2 非 ASCII 空格映射、
NFKC、禁用字符 / A.1 未分配码点检查、D.1 / D.2 双向文字检查。
禁止集合为 RFC 3454 C.1.2、C.2.1、C.2.2、C.3–C.9。
含 RandALCat 时不能含 LCat，且首尾都必须是 RandALCat。
大小写保持，不做 case folding。空输入和删除字符后变空都合法。
U+200B 同时在 B.1 与 C.1.2 中，本严格入口优先删除 B.1。

NFKC 包含递归兼容分解、稳定 canonical ordering、排除字符后的 canonical
composition 与 Hangul 的算法分解/组合。Unicode 3.2 之后的属性变化不会改变
结果；例如 emoji 在本 SASLprep profile 中可能仍是未分配码点。
这不是面向现代文本的通用最新版 Unicode 规范化。

`saslprep.Error` 为 `InvalidUtf8`、`Prohibited`、`Unassigned`、
`Bidirectional`、`LimitExceeded`；错误中不保存原始密码。
`utf8.Error` 为 `InvalidUtf8`、`InvalidScalar`、`LimitExceeded`。

## PostgreSQL 兼容规则

`password.prepare` 成功规范化时返回规范化字节；非法 UTF-8、禁用字符、
未分配码点或 bidi 不通过时返回**完整原始字节**。不能把严格入口的任意错误
都转换为回退：资源限制错误必须传播，且回退结果也检查输出长度。

实现对照 PostgreSQL 的 `pg_saslprep`，保留以下与严格入口不同的行为：

- ASCII 快速路径保持字节，包括控制字符；原输入为空也保持为空。
- C.1.2 映射先于 B.1，因此 U+200B 变成一个空格。
- 非空输入映射后为空，按失败回退；只有 soft hyphen 的密码不会变成空密码。
- 禁用、未分配和 bidi 检查作用于映射后、NFKC 前的序列，保持 PostgreSQL 的顺序。
- 兼容层单独应用 Unicode 4.0 的五个 normalization corrections：
  U+2F868、U+2F874、U+2F91F、U+2F95F、U+2F9BF；不改变 `nfkc32` 的 3.2 结果。

密码中嵌入 NUL 返回 `password.Error.EmbeddedNul`：PostgreSQL 的 C-string
接口无法表达完整值，本接口不会仿照截断或悄悄认证另一个密码。
资源超限返回 `password.Error.LimitExceeded`。这两个错误都不会回退。
空字节序列可作为准备结果，不代表 PostgreSQL 服务器允许空密码认证。

兼容证据为 PostgreSQL **14.17** 的本地 pg_saslprep 差分测试，以及
`REL_18_STABLE` 源码规则核对；不是对未来版本行为的保证。

## 资源限制

`nfkc32.Limits` 的所有字段均为 UInt64，可以显式收紧：

| 字段 | 默认值 | 限制 |
| --- | --- | --- |
| max_input_bytes | 16384 | 原始 UTF-8 / 密码字节长度 |
| max_output_bytes | 65536 | 规范化或 PostgreSQL 回退的最终字节长度 |
| max_scalars | 65536 | 解码和分解过程中 scalar 数量；包括中间展开 |
| max_reorderings | 1048576 | canonical ordering 移动次数，限制恶意组合字符的二次工作量 |

0 是合法预算，按实际消耗判断。按二分查找访问生成数据；每次操作保留一份
所需的数据：一次复制生成 67,503 字节的 Bytes，各属性区段通过偏移共享它，
不进行运行时解压。PostgreSQL 的 ASCII 快速路径无需分配这些数据。
系统内存分配失败沿用 runtime 的失败策略，不声称能够恢复 OOM；
密码 Bytes/String 不保证销毁时清零，调用方不得将其写入日志。

## 数据与复现

生成器 [generate-unicode32.py](../../scripts/generate-unicode32.py)只读取 Python
显式版本化的 `unicodedata.ucd_3_2_0` 和 `stringprep`，校验二者版本。
提交的 [tables32.jk](../../std/joky/unicode/tables32.jk) 每段附有内容 SHA-256；
[unicode32.bin](../../std/joky/unicode/unicode32.bin) 为紧凑大端记录，码点占 3 字节，
分解索引占 2 字节，重复分解序列去重。有效数据从 157,694 字节降为 67,503 字节
（减少 57.2%）；访问代码从 159,380 字节降为 2,371 字节。
生成器同时重建并校验二进制与访问代码，不联网。分解预先展开；算法本身仍在 Joky 运行。
Unicode 数据的许可见 [LICENSE-UNICODE.txt](../../std/joky/unicode/LICENSE-UNICODE.txt)。

```sh
python3 scripts/generate-unicode32.py --check
JOKY_TEST_AOT=full cargo test --test cli saslprep
python3 scripts/test-saslprep.py --aot full
```

[手写回归](../../tests/fixtures/saslprep.jk)包含 RFC 4013 向量、Hangul、
非法 UTF-8、bidi、兼容差异、NUL、各资源边界，以及准备后的 RFC 7677 proof。
[CLI 回归](../../tests/cli/saslprep.rs)验证冷/缓存 JIT、debug/release AOT，
并读取 [独立 corpus](../../tests/fixtures/unicode32_vectors.bin)：500 个 NFKC、
512 个严格 SASLprep、512 个 PostgreSQL 密码样例；普通测试不要求安装 Python 或 PostgreSQL。
该 corpus SHA-256 为 `a288947b51a5dc1765e39c10599b6395abb32a28d6706e4c3debfb3cfc14d34c`。

[差分脚本](../../scripts/test-saslprep.py)的 `--full` 增加全部 Hangul、
有分解/combining class 的码点、属性边界和固定种子的组合序列。
`--normalization-test` 接收官方 `NormalizationTest-3.2.0.txt`，校验 SHA-256
`c4513869bb7098d19838be4a1fd5d760843c5804bfe03bd6bbb20623ceb6e57d`；
使用其五列对 NFKC 的官方不变量，不用生成器反推预期结果。

安装 PostgreSQL 的静态开发库后，可以直接以其 C 实现作为测试 oracle：

```sh
python3 scripts/test-saslprep.py --full --aot full \
  --normalization-test /path/to/NormalizationTest-3.2.0.txt \
  --postgres-libdir "$(pg_config --libdir)" \
  --postgres-includedir "$(pg_config --includedir-server)"

# 复现提交的紧凑 corpus：Python 3.14.6，PostgreSQL 14.17
python3 scripts/test-saslprep.py --sample-count 512 \
  --postgres-libdir "$(pg_config --libdir)" \
  --postgres-includedir "$(pg_config --includedir-server)" \
  --write-fixture tests/fixtures/unicode32_vectors.bin
```

这是测试期的 C oracle，不是 Joky 标准库依赖。测试不启动数据库、不发送密码，
只报告样例索引；真实服务器认证留到下一步 pgsql 状态机集成验证。

## 依据

- [RFC 4013](https://www.rfc-editor.org/rfc/rfc4013)：SASLprep profile。
- [RFC 3454](https://www.rfc-editor.org/rfc/rfc3454)：固定属性集合与 bidi 规则。
- [Unicode 3.2 数据](https://www.unicode.org/Public/3.2-Update/)与
  [NormalizationTest](https://www.unicode.org/Public/3.2-Update/NormalizationTest-3.2.0.txt)。
- [PostgreSQL SASL 文档](https://www.postgresql.org/docs/18/sasl-authentication.html)与
  [pg_saslprep 源码](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/common/saslprep.c)。
