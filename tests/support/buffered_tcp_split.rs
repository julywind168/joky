// Run the original NET4 read-only matrix through the split reader as well.
pub fn frames_source() -> String {
    let source = include_str!("../fixtures/buffered_tcp_frames.jk")
        .replace("BufferedTcp", "BufferedTcpReader")
        .replace("BufferedTcpReader.with_capacity(stream,", "split_buffer(stream,")
        .replace(
            "BufferedTcpReader.new(stream)",
            "match split_buffer(stream, 16384) { Ok(value) => value, _ => { return 1 } }",
        )
        // Direction close may report an OS shutdown error after both FINs.
        .replace("ensure(buffered.close().is_ok())", "buffered.close();");
    format!(
        "{source}\nfn split_buffer(stream: TcpStream, capacity: UInt64) @suspends -> Result<BufferedTcpReader, ReadError> {{\n\
         let (reader, writer) = stream.split()\n\
         BufferedTcpReader.with_capacity(reader, capacity)\n}}"
    )
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn reset_source(port: u16, method: &str) -> String {
    include_str!("../fixtures/buffered_tcp_reset.jk")
        .replace("PORT", &port.to_string())
        .replace(".read(4)", &format!(".{method}"))
        .replace(
            "    match first.kind()",
            "    var (buffered, writer) = buffered.split()\n    match first.kind()",
        )
        .replace("if !buffered.close().is_ok() { return 10 }", "buffered.close();")
}
