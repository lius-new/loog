//! Bulk codec for the public completion records. No server/protocol fields.
use super::{
    ByteRange, ErrorKind, HostError, ProviderCommand, TextEdit, WireCompletionItem, host_error,
};
use serde::Deserialize;

fn offset<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<u64, D::Error> {
    struct Offset;
    impl<'de> serde::de::Visitor<'de> for Offset {
        type Value = u64;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an unsigned byte offset or decimal string")
        }
        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<u64, E> {
            Ok(value)
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<u64, E> {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(E::custom("invalid byte offset"));
            }
            value.parse().map_err(E::custom)
        }
    }
    decoder.deserialize_any(Offset)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Range {
    #[serde(deserialize_with = "offset")]
    start: u64,
    #[serde(deserialize_with = "offset")]
    end: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    range: Range,
    text: String,
}
impl From<Edit> for TextEdit {
    fn from(edit: Edit) -> Self {
        Self {
            range: ByteRange {
                start: edit.range.start,
                end: edit.range.end,
            },
            text: edit.text,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    name: String,
    arguments: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Item {
    label: String,
    label_detail: Option<String>,
    label_description: Option<String>,
    deprecated: Option<bool>,
    preselect: Option<bool>,
    insert_text_mode: Option<u8>,
    detail: Option<String>,
    documentation: Option<String>,
    kind: Option<u32>,
    insert_text: Option<String>,
    text_edit: Option<Edit>,
    #[serde(default)]
    additional_text_edits: Vec<Edit>,
    insert_text_format: Option<u8>,
    #[serde(default)]
    commit_characters: Vec<String>,
    command: Option<Command>,
    sort_text: Option<String>,
    filter_text: Option<String>,
    data: Option<String>,
}
pub(super) fn decode(json: &str) -> Result<Vec<WireCompletionItem>, HostError> {
    if json.len() > 32 * 1024 * 1024 {
        return Err(host_error(
            ErrorKind::LimitExceeded,
            "completion batch exceeds 32 MiB",
        ));
    }
    struct Batch(Vec<Item>);
    impl<'de> Deserialize<'de> for Batch {
        fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Batch;
                fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    formatter.write_str("a bounded completion record list")
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut sequence: A,
                ) -> Result<Batch, A::Error> {
                    let mut items = Vec::new();
                    while let Some(item) = sequence.next_element::<Item>()? {
                        // More empty records already exceed the normal expanded
                        // answer budget. Enforce this before allocating them all.
                        if items.len() >= 131072 {
                            return Err(serde::de::Error::custom("too many completion records"));
                        }
                        items.push(item);
                    }
                    Ok(Batch(items))
                }
            }
            decoder.deserialize_seq(Visitor)
        }
    }
    let Batch(items) = serde_json::from_str(json)
        .map_err(|_| host_error(ErrorKind::InvalidEdit, "invalid completion batch records"))?;
    Ok(items
        .into_iter()
        .map(|item| WireCompletionItem {
            label: item.label,
            label_detail: item.label_detail,
            label_description: item.label_description,
            deprecated: item.deprecated,
            preselect: item.preselect,
            insert_text_mode: item.insert_text_mode,
            detail: item.detail,
            documentation: item.documentation,
            kind: item.kind,
            insert_text: item.insert_text,
            text_edit: item.text_edit.map(Into::into),
            additional_text_edits: item
                .additional_text_edits
                .into_iter()
                .map(Into::into)
                .collect(),
            insert_text_format: item.insert_text_format,
            commit_characters: item.commit_characters,
            command: item.command.map(|c| ProviderCommand {
                name: c.name,
                arguments: c.arguments,
            }),
            sort_text: item.sort_text,
            filter_text: item.filter_text,
            data: item.data,
        })
        .collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_records_preserve_edits_metadata_and_lossless_offsets() {
        let items = decode(r#"[{"label":"中😀","labelDetail":"()","labelDescription":"module","deprecated":true,"preselect":true,"insertTextMode":2,"insertTextFormat":2,"kind":3,"detail":"detail","documentation":"docs","insertText":"${1:value}","textEdit":{"range":{"start":"9007199254740993","end":"9007199254740994"},"text":"text"},"additionalTextEdits":[{"range":{"start":"0","end":"0"},"text":"import\n"}],"commitCharacters":["("],"command":{"name":"accept","arguments":"[42]"},"sortText":"1","filterText":"filter","data":"opaque"}]"#).unwrap();
        let item = &items[0];
        assert_eq!(item.label, "中😀");
        assert_eq!(
            item.text_edit.as_ref().unwrap().range.start,
            9_007_199_254_740_993
        );
        assert_eq!(item.additional_text_edits[0].text, "import\n");
        assert_eq!(item.command.as_ref().unwrap().arguments, "[42]");
        assert_eq!(item.insert_text_format, Some(2));
        assert_eq!(item.preselect, Some(true));
        assert_eq!(item.data.as_deref(), Some("opaque"));
    }
    #[test]
    fn unknown_protocol_fields_and_invalid_types_are_rejected_atomically() {
        for json in [
            r#"[{"label":"a","textEdit":{"newText":"LSP"}}]"#,
            r#"[{"label":"a","kind":-1}]"#,
            r#"[{"label":"a","textEdit":{"range":{"start":"18446744073709551616","end":"1"},"text":"x"}}]"#,
            r#"[{"label":"a"},{"label":"b","extra":true}]"#,
        ] {
            assert!(decode(json).is_err(), "{json}");
        }
    }
    #[test]
    fn compact_empty_records_cannot_expand_without_a_bound() {
        let mut json = "[{\"label\":\"\"}".to_owned();
        json.push_str(&",{\"label\":\"\"}".repeat(131072));
        json.push(']');
        assert!(decode(&json).is_err());
    }
}
