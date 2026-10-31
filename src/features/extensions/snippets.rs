//! Snippet files that an extension package contributes (design §6.6).
//!
//! A file names one language and lists snippets in the editor's own snippet
//! syntax (tab stops, placeholders, variables). They are offered in the
//! completion list like a language server's snippet items. A file is accepted
//! whole or rejected whole, with the reason, so a half-read file never offers
//! entries the author did not mean.
use serde_json::{Map, Value, json};

/// Most snippets one file may hold, and the longest prefix and body taken.
pub const MAX_SNIPPETS: usize = 500;
const MAX_PREFIX: usize = 64;
const MAX_BODY: usize = 8000;
const MAX_DESCRIPTION: usize = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub prefix: String,
    pub body: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub language: String,
    pub snippets: Vec<Entry>,
}

fn text<'a>(object: &'a Map<String, Value>, key: &str, what: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{what} needs text in \"{key}\""))
}

/// The body is one string or a list of lines, joined with line feeds.
fn body(value: &Value, what: &str) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Array(lines) => lines
            .iter()
            .map(|line| line.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map(|lines| lines.join("\n"))
            .ok_or_else(|| format!("{what}: \"body\" lines must be text")),
        _ => Err(format!("{what}: \"body\" must be text or a list of lines")),
    }
}

pub fn parse(source: &str) -> Result<File, String> {
    let value: Value =
        serde_json::from_str(source).map_err(|error| format!("invalid JSON: {error}"))?;
    let root = value.as_object().ok_or("the file must be an object")?;
    if let Some(key) = root
        .keys()
        .find(|key| !["language", "snippets"].contains(&key.as_str()))
    {
        return Err(format!("unknown field \"{key}\""));
    }
    let language = text(root, "language", "the file")?.to_owned();
    if language.is_empty() {
        return Err("\"language\" is empty".to_owned());
    }
    let list = root
        .get("snippets")
        .and_then(Value::as_array)
        .ok_or("the file needs a list in \"snippets\"")?;
    if list.len() > MAX_SNIPPETS {
        return Err(format!("more than {MAX_SNIPPETS} snippets"));
    }
    let mut snippets = Vec::with_capacity(list.len());
    for (at, entry) in list.iter().enumerate() {
        let what = format!("snippet {}", at + 1);
        let object = entry
            .as_object()
            .ok_or_else(|| format!("{what} must be an object"))?;
        if let Some(key) = object
            .keys()
            .find(|key| !["prefix", "body", "description"].contains(&key.as_str()))
        {
            return Err(format!("{what}: unknown field \"{key}\""));
        }
        let prefix = text(object, "prefix", &what)?;
        if prefix.trim().is_empty() || prefix.len() > MAX_PREFIX || prefix.contains('\n') {
            return Err(format!("{what}: \"prefix\" must be a short single line"));
        }
        let body = body(
            object
                .get("body")
                .ok_or_else(|| format!("{what} needs a \"body\""))?,
            &what,
        )?;
        if body.is_empty() || body.len() > MAX_BODY {
            return Err(format!("{what}: \"body\" must have 1 to {MAX_BODY} bytes"));
        }
        let description = match object.get("description") {
            None => None,
            Some(description) => Some(
                description
                    .as_str()
                    .filter(|text| text.len() <= MAX_DESCRIPTION)
                    .ok_or_else(|| format!("{what}: \"description\" must be short text"))?
                    .to_owned(),
            ),
        };
        snippets.push(Entry {
            prefix: prefix.to_owned(),
            body,
            description,
        });
    }
    Ok(File { language, snippets })
}

/// The completion items for `file`, in the shape of a language server's reply:
/// snippet-format items (LSP kind 15) that insert the body.
pub fn completion_items(file: &File) -> Vec<Value> {
    file.snippets
        .iter()
        .map(|entry| {
            let mut item = json!({
                "label": entry.prefix,
                "kind": 15,
                "insertText": entry.body,
                "insertTextFormat": 2,
                "detail": "snippet",
            });
            if let Some(description) = &entry.description {
                item["documentation"] = Value::from(description.clone());
            }
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_names_a_language_and_bodies_may_be_lines() {
        let file = parse(
            r##"{ "language": "rust", "snippets": [
                { "prefix": "tfn", "body": ["#[test]", "fn ${1:name}() {", "\t$0", "}"], "description": "A test" },
                { "prefix": "dbg", "body": "dbg!($0);" }
            ] }"##,
        )
        .unwrap();
        assert_eq!(file.language, "rust");
        assert_eq!(file.snippets[0].body, "#[test]\nfn ${1:name}() {\n\t$0\n}");
        assert_eq!(file.snippets[1].description, None);

        let items = completion_items(&file);
        assert_eq!(items[0]["insertTextFormat"], 2);
        assert_eq!(items[0]["documentation"], "A test");
        assert_eq!(items[1]["label"], "dbg");
        assert!(items[1].get("documentation").is_none());
    }

    #[test]
    fn a_mistake_rejects_the_whole_file_and_says_which_snippet() {
        for (source, expected) in [
            (r#"{ "snippets": [] }"#, "needs text in \"language\""),
            (r#"{ "language": "rust" }"#, "needs a list"),
            (
                r#"{ "language": "rust", "snippets": [], "x": 1 }"#,
                "unknown field \"x\"",
            ),
            (
                r#"{ "language": "rust", "snippets": [{ "prefix": "a" }] }"#,
                "snippet 1 needs a \"body\"",
            ),
            (
                r#"{ "language": "rust", "snippets": [{ "prefix": "a", "body": "b" }, { "prefix": " ", "body": "b" }] }"#,
                "snippet 2: \"prefix\"",
            ),
            (
                r#"{ "language": "rust", "snippets": [{ "prefix": "a", "body": 3 }] }"#,
                "text or a list",
            ),
            (
                r#"{ "language": "rust", "snippets": [{ "prefix": "a", "body": [1] }] }"#,
                "lines must be text",
            ),
            (
                r#"{ "language": "rust", "snippets": [{ "prefix": "a", "body": "" }] }"#,
                "1 to",
            ),
            (
                r#"{ "language": "rust", "snippets": [{ "prefix": "a", "body": "b", "extra": 1 }] }"#,
                "unknown field \"extra\"",
            ),
        ] {
            let error = parse(source)
                .err()
                .unwrap_or_else(|| panic!("{source} was accepted"));
            assert!(error.contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn a_file_is_bounded() {
        let many: Vec<Value> = (0..MAX_SNIPPETS + 1)
            .map(|n| json!({ "prefix": format!("p{n}"), "body": "x" }))
            .collect();
        let source = json!({ "language": "rust", "snippets": many }).to_string();
        assert!(parse(&source).unwrap_err().contains("more than"));
    }
}
