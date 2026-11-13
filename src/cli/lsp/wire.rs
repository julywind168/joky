use serde_json::Value;
use std::io::{self, BufRead, Read, Write};

const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_HEADER: usize = 8192;

pub(super) fn read(input: &mut impl BufRead) -> io::Result<Option<serde_json::Result<Value>>> {
    let mut header = Vec::new();
    let mut length = None;
    let mut read = 0;
    loop {
        header.clear();
        let bytes = input
            .take((MAX_HEADER - read) as u64)
            .read_until(b'\n', &mut header)?;
        if bytes == 0 && read == 0 {
            return Ok(None);
        }
        read += bytes;
        let line = header.strip_suffix(b"\r\n").ok_or_else(invalid_header)?;
        if line.is_empty() {
            break;
        }
        let line = std::str::from_utf8(line).map_err(|_| invalid_header())?;
        let (name, value) = line.split_once(':').ok_or_else(invalid_header)?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(invalid_header());
            }
            length = value
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|size| *size <= MAX_BODY);
            if length.is_none() {
                return Err(invalid_header());
            }
        }
    }
    let mut body = vec![0; length.ok_or_else(invalid_header)?];
    input.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)))
}

pub(super) fn write(output: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()
}

fn invalid_header() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid LSP message header or size",
    )
}
