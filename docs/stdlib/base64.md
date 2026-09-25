# Base64

`import joky/encoding/base64`。纯 Joky 实现的 RFC 4648 标准 Base64，
使用 `A–Z a–z 0–9 + /` 字母表，无换行，末尾按需补 `=`。

| API | 返回值 | 语义 |
| --- | --- | --- |
| `base64.encode(data: Bytes)` | `String` | 将任意二进制数据编码为 ASCII 文本 |
| `base64.decode(text: String)` | `Result(Bytes, String)` | 严格解码，不要求解码结果为 UTF-8 |

```joky
import joky/encoding/base64

fn main() -> Result(Unit, String) {
    println(base64.encode(b"foobar")) // Zm9vYmFy
    println(base64.decode("Zm9vYmFy")?.to_string()!) // foobar
    Ok(())
}
```

空输入对应空输出。解码时输入的 UTF-8 字节长度必须是四的倍数，
只有最后一组可以包含一个或两个 `=`，而且未使用的填充位必须为零。
例如 `Zg==` 和 `Zm8=` 合法；`Zh==`、`Zm9=` 虽可能被宽松解码器接受，
在这里会返回 `Err`。

缺少或多余的 padding、内部 `=`、空格、换行、NUL、非 ASCII 字符、
URL-safe 的 `-` / `_` 等都会返回错误。当前没有无 padding、URL-safe 或 MIME 模式。
错误不会返回部分解码结果，错误类型为 `String`。

输入和输出均整块处理，内存用量随数据大小增长；实现使用 `MutBytes` 追加，
不逐字符拼接不可变字符串。调用者可在解码前按自身协议限制输入长度。

完整[示例](../../examples/basics/crypto.jk)演示与 SHA-256 组合。
[测试向量](../../tests/fixtures/base64.jk)包含 RFC 4648 第 10 节、全部 256 种字节、
UTF-8 文本、错误 padding、非零填充位和非法字符，覆盖 JIT、缓存 JIT 和 AOT。

规范：[RFC 4648](https://www.rfc-editor.org/rfc/rfc4648.html)。
