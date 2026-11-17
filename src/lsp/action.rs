//! code actions: quick fixes and refactors a server offers for a range.

use serde_json::{Value, json};

/// most actions kept from one reply.
const MAX_ACTIONS: usize = 200;
/// a diagnostic's own fields past this many bytes are not sent back.
const MAX_RAW: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub title: String,
    /// `quickfix`, `refactor.extract` and so on; empty when not given.
    pub kind: String,
    /// the one to pick when there is a clear best.
    pub preferred: bool,
    /// the workspace edit, when the reply had it.
    pub edit: Option<Value>,
    /// the action as sent, to ask the server for its edit.
    pub raw: Value,
}

/// the actions of a reply. bare commands are left out: running them is not
/// supported yet.
pub fn read(result: &Value) -> Vec<Action> {
    let Some(list) = result.as_array() else {
        return Vec::new();
    };
    let mut out: Vec<Action> = list
        .iter()
        .filter(|a| !a["command"].is_string())
        .filter_map(|a| {
            let title: String = a["title"].as_str()?.chars().take(200).collect();
            if title.is_empty() || a["disabled"].is_object() {
                return None;
            }
            let edit = a["edit"].is_object().then(|| a["edit"].clone());
            // nothing to do without an edit, unless the server fills it in
            if edit.is_none() && a["data"].is_null() {
                return None;
            }
            Some(Action {
                title,
                kind: a["kind"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(64)
                    .collect(),
                preferred: a["isPreferred"].as_bool() == Some(true),
                edit,
                raw: a.clone(),
            })
        })
        .take(MAX_ACTIONS)
        .collect();
    // preferred first, then fixes, then the rest, each in the server's order
    out.sort_by_key(|a| (!a.preferred, !a.kind.starts_with("quickfix")));
    out
}

/// the fields of a diagnostic a server may want back (`code`, `data` ...),
/// without the ones ligase keeps itself. none when there are none or they
/// are too big.
pub fn diag_extra(d: &Value) -> Option<Value> {
    let mut keep = serde_json::Map::new();
    for key in ["code", "source", "data", "codeDescription", "tags"] {
        if !d[key].is_null() {
            keep.insert(key.to_owned(), d[key].clone());
        }
    }
    if keep.is_empty() {
        return None;
    }
    let keep = Value::Object(keep);
    (keep.to_string().len() <= MAX_RAW).then_some(keep)
}

/// a diagnostic as a server sends it, from what ligase kept.
pub fn diag_json(range: Value, severity: i64, message: &str, extra: Option<&Value>) -> Value {
    let mut d = json!({ "range": range, "severity": severity, "message": message });
    if let (Some(d), Some(Value::Object(extra))) = (d.as_object_mut(), extra) {
        for (k, v) in extra {
            d.insert(k.clone(), v.clone());
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_actions_and_puts_the_best_first() {
        let reply = json!([
            { "title": "Extract", "kind": "refactor.extract", "data": 1 },
            { "title": "Run", "command": "x.run" },
            { "title": "Try this: exact h", "kind": "quickfix", "edit": { "changes": {} } },
            { "title": "Off", "edit": {}, "disabled": { "reason": "no" } },
            { "title": "Nothing", "kind": "quickfix" },
            { "title": "Best", "isPreferred": true, "edit": {} },
            { "kind": "quickfix", "edit": {} },
        ]);
        let titles: Vec<String> = read(&reply).into_iter().map(|a| a.title).collect();
        assert_eq!(titles, ["Best", "Try this: exact h", "Extract"]);
        assert!(read(&json!(null)).is_empty());
        assert!(read(&json!({ "title": 3 })).is_empty());
    }

    #[test]
    fn diagnostics_go_back_with_their_own_fields() {
        let d = json!({ "range": {}, "message": "m", "code": "E0308", "data": { "x": 1 } });
        let extra = diag_extra(&d).unwrap();
        assert_eq!(extra, json!({ "code": "E0308", "data": { "x": 1 } }));
        let back = diag_json(json!({ "start": 0 }), 1, "m", Some(&extra));
        assert_eq!(back["code"], "E0308");
        assert_eq!(back["message"], "m");
        assert!(diag_extra(&json!({ "message": "m" })).is_none());
        let big = json!({ "data": "x".repeat(MAX_RAW) });
        assert!(diag_extra(&big).is_none());
    }
}
