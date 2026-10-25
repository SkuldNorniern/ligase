//! text edits and locations from language server replies: rename,
//! formatting, references. untrusted like every reply: shapes checked,
//! counts capped.

use std::path::PathBuf;

use resolvase::Point;
use serde_json::Value;

use super::uri;

/// files one workspace edit may touch.
const MAX_FILES: usize = 1000;
/// edits per file.
const MAX_EDITS: usize = 20_000;
/// locations kept from one reply.
const MAX_LOCATIONS: usize = 5000;

/// one change a server asks for, positions in its encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: Point,
    pub end: Point,
    pub text: String,
}

fn point(value: &Value) -> Option<Point> {
    let line = usize::try_from(value["line"].as_u64()?).ok()?;
    let column = usize::try_from(value["character"].as_u64()?).ok()?;
    Some(Point::new(line, column))
}

/// a `TextEdit[]`.
pub fn text_edits(value: &Value) -> Vec<Edit> {
    value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take(MAX_EDITS)
        .filter_map(|e| {
            Some(Edit {
                start: point(&e["range"]["start"])?,
                end: point(&e["range"]["end"])?,
                text: e["newText"].as_str()?.to_owned(),
            })
        })
        .collect()
}

/// a `WorkspaceEdit`: `documentChanges` with text edits when it has them
/// (file create, rename and delete operations are skipped), else `changes`
/// by uri. never both: a server may send the same edits in each.
pub fn workspace_edit(value: &Value) -> Vec<(PathBuf, Vec<Edit>)> {
    let mut out: Vec<(PathBuf, Vec<Edit>)> = Vec::new();
    if value["documentChanges"].is_array() {
        // handled below
    } else if let Some(changes) = value["changes"].as_object() {
        for (u, edits) in changes.iter().take(MAX_FILES) {
            if let Some(path) = uri::to_path(u) {
                out.push((path, text_edits(edits)));
            }
        }
    }
    if let Some(list) = value["documentChanges"].as_array() {
        for change in list.iter().take(MAX_FILES) {
            let Some(u) = change["textDocument"]["uri"].as_str() else {
                continue;
            };
            if let Some(path) = uri::to_path(u) {
                out.push((path, text_edits(&change["edits"])));
            }
        }
    }
    out.retain(|(_, edits)| !edits.is_empty());
    out.truncate(MAX_FILES);
    out
}

/// `Location[]`, `LocationLink[]` or one of them, as files and places.
pub fn locations(value: &Value) -> Vec<(PathBuf, Point)> {
    let list = match value {
        Value::Array(list) => list.as_slice(),
        Value::Object(_) => std::slice::from_ref(value),
        _ => &[],
    };
    list.iter()
        .take(MAX_LOCATIONS)
        .filter_map(|l| {
            let u = l["uri"].as_str().or(l["targetUri"].as_str())?;
            let range = if l.get("targetSelectionRange").is_some() {
                &l["targetSelectionRange"]
            } else {
                &l["range"]
            };
            Some((uri::to_path(u)?, point(&range["start"])?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(line: usize, character: usize) -> Value {
        json!({ "line": line, "character": character })
    }

    #[test]
    fn workspace_edits_in_both_shapes() {
        let edit = json!({ "range": { "start": at(0, 4), "end": at(0, 7) }, "newText": "bar" });
        let changes = json!({ "changes": { "file:///D:/a.rs": [edit] } });
        let got = workspace_edit(&changes);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1[0].text, "bar");
        let docs = json!({ "documentChanges": [
            { "textDocument": { "uri": "file:///D:/b.rs", "version": 3 }, "edits": [edit] },
            { "kind": "create", "uri": "file:///D:/c.rs" },
        ]});
        assert_eq!(workspace_edit(&docs).len(), 1);
        assert!(workspace_edit(&json!(null)).is_empty());
        assert!(workspace_edit(&json!({ "changes": { "file:///x": "junk" } })).is_empty());
        let both = json!({
            "changes": { "file:///D:/b.rs": [edit] },
            "documentChanges": [{ "textDocument": { "uri": "file:///D:/b.rs" }, "edits": [edit] }],
        });
        assert_eq!(workspace_edit(&both).len(), 1, "one copy, not two");
    }

    #[test]
    fn locations_and_links() {
        let one =
            json!({ "uri": "file:///D:/a.rs", "range": { "start": at(2, 1), "end": at(2, 3) } });
        assert_eq!(locations(&one).len(), 1);
        let links = json!([{ "targetUri": "file:///D:/a.rs", "targetRange": {},
            "targetSelectionRange": { "start": at(5, 0), "end": at(5, 1) } }]);
        assert_eq!(locations(&links)[0].1, Point::new(5, 0));
        assert!(locations(&json!(7)).is_empty());
    }
}
