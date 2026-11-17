//! signature help: the call the caret is in, its parameters, the one being
//! typed.

use serde_json::Value;

/// longest text kept from one reply.
const MAX_TEXT: usize = 8 * 1024;

/// a reply as markdown for the hover card: the signature in a code block,
/// the parameter being typed under it, then the docs. none when the caret
/// is in no call.
pub fn text(result: &Value) -> Option<String> {
    let sigs = result["signatures"].as_array()?;
    let which = result["activeSignature"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n < sigs.len())
        .unwrap_or(0);
    let sig = sigs.get(which)?;
    let label = sig["label"].as_str()?;
    if label.is_empty() {
        return None;
    }
    let mut out = format!("```\n{}\n```", label.replace("```", "'''"));
    if sigs.len() > 1 {
        out.push_str(&format!("\n\n{} / {}", which + 1, sigs.len()));
    }
    // the signature's own number wins over the reply's
    let active = sig["activeParameter"]
        .as_u64()
        .or_else(|| result["activeParameter"].as_u64())
        .and_then(|n| usize::try_from(n).ok());
    if let Some(param) = active.and_then(|n| sig["parameters"].as_array()?.get(n)) {
        let name = param_label(label, &param["label"]);
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            out.push_str(&format!("\n\n**{name}**"));
            let doc = docs(&param["documentation"]);
            if !doc.is_empty() {
                out.push_str("  ");
                out.push_str(&doc);
            }
        }
    }
    let doc = docs(&sig["documentation"]);
    if !doc.is_empty() {
        out.push_str("\n\n");
        out.push_str(&doc);
    }
    if out.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    Some(out)
}

/// a parameter's name: its own string, or a `[start, end]` span of the
/// signature in utf-16 units.
fn param_label(label: &str, value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_owned());
    }
    let span = value.as_array()?;
    let a = usize::try_from(span.first()?.as_u64()?).ok()?;
    let b = usize::try_from(span.get(1)?.as_u64()?).ok()?;
    if a > b {
        return None;
    }
    // walk utf-16 units to byte offsets; a span off the label is dropped
    let (mut from, mut to, mut units) = (None, None, 0);
    for (i, c) in label.char_indices() {
        if units == a {
            from = Some(i);
        }
        if units == b {
            to = Some(i);
        }
        units += c.len_utf16();
    }
    if units == a {
        from = Some(label.len());
    }
    if units == b {
        to = Some(label.len());
    }
    Some(label.get(from?..to?)?.to_owned())
}

/// documentation: a string or markup content.
fn docs(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shows_the_call_and_the_parameter_being_typed() {
        let reply = json!({
            "signatures": [{
                "label": "fn add(a: i32, b: i32) -> i32",
                "documentation": { "kind": "markdown", "value": "adds" },
                "parameters": [{ "label": [7, 13] }, { "label": [15, 21], "documentation": "the second" }],
            }],
            "activeParameter": 1,
        });
        let t = text(&reply).unwrap();
        assert!(
            t.starts_with("```\nfn add(a: i32, b: i32) -> i32\n```"),
            "{t}"
        );
        assert!(t.contains("**b: i32**  the second"), "{t}");
        assert!(t.ends_with("adds"));
        let named = json!({ "signatures": [{ "label": "f(x)", "parameters": [{ "label": "x" }], "activeParameter": 0 }] });
        assert!(text(&named).unwrap().contains("**x**"));
    }

    #[test]
    fn odd_replies_show_nothing_or_less() {
        assert_eq!(text(&json!(null)), None);
        assert_eq!(text(&json!({ "signatures": [] })), None);
        assert_eq!(text(&json!({ "signatures": [{ "label": "" }] })), None);
        // spans off the label, backwards, or mid char: no parameter line
        for span in [json!([3, 99]), json!([5, 2]), json!([2, 3])] {
            let reply = json!({ "signatures": [{ "label": "f(😀)", "parameters": [{ "label": span }] }], "activeParameter": 0 });
            let t = text(&reply).unwrap();
            assert!(!t.contains("**"), "{t}");
        }
        let utf16 = json!({ "signatures": [{ "label": "f(가, b)", "parameters": [{ "label": [2, 3] }] }], "activeParameter": 0 });
        assert!(text(&utf16).unwrap().contains("**가**"));
        let many =
            json!({ "signatures": [{ "label": "a()" }, { "label": "b()" }], "activeSignature": 7 });
        assert!(text(&many).unwrap().contains("1 / 2"));
    }
}
