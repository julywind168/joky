pub const LOOPS: &str = include_str!("../fixtures/text_host.jk");

// Include Unit placeholders, numeric slots above bit 63, and two owning inputs.
pub fn wide_source() -> String {
    let params = (0..67)
        .map(|index| match index {
            0 => "unit: Unit".into(),
            1 | 65 => format!("text{index}: String"),
            _ => format!("n{index}: Int32"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let args = (0..67)
        .map(|index| match index {
            0 => "{}".into(),
            1 => "\"left\"".into(),
            65 => "\"中🙂\\0tail\"".into(),
            _ => index.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("extern fn wide({params}) @suspends -> String fn run() @suspends -> String {{ wide({args}) }}")
}
