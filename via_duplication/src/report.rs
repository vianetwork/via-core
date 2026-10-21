use crate::scan::{Duplicate, Fragment};

// Own renderer: HyperDX's location listing idea, with Via verdicts and deterministic ordering.
// https://github.com/hyperdxio/hyperdx/blob/b9f61708eb7501d1fb2662b339008114853f7d88/scripts/ci/dupes.mjs
pub fn escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '`' | '|' | '[' | ']' | '*' | '_' | '\\' => format!("&#{};", c as u32),
            '\n' | '\r' => " ".into(),
            _ => c.to_string(),
        })
        .collect()
}
pub fn location(f: &Fragment) -> String {
    format!("`{}:{}–{}`", f.name.0, f.start, f.end)
}
pub fn finding(label: &str, d: &Duplicate) -> String {
    format!(
        "- **{label}**: {} ↔ {} ({} tokens, {} lines)\n",
        location(&d.first_file),
        location(&d.second_file),
        d.tokens,
        d.lines
    )
}
