//! Pure URI/path and heading policies, shared by file and memory hosts.
use super::{
    model::*,
    reader::{LinkIntent, classify_link},
};
use std::path::{Path, PathBuf};
use unicode_segmentation::UnicodeSegmentation;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Web(String),
    Local {
        path: PathBuf,
        fragment: Option<String>,
    },
    Anchor(String),
}
pub fn decode(value: &str) -> Option<String> {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            out.push(u8::from_str_radix(value.get(at + 1..at + 3)?, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    let result = String::from_utf8(out).ok()?;
    (!result.contains(['\0', '\r', '\n'])).then_some(result)
}
pub fn resolve(base_document: Option<&Path>, destination: &str) -> Option<Address> {
    let value = destination.trim();
    match classify_link(value) {
        LinkIntent::Blocked => None,
        LinkIntent::Web => Some(Address::Web(value.to_owned())),
        LinkIntent::Fragment => Some(Address::Anchor(decode(value.strip_prefix('#')?)?)),
        LinkIntent::LocalDocument => {
            let (path, fragment) = value
                .split_once('#')
                .map_or((value, None), |(path, fragment)| (path, Some(fragment)));
            let path = decode(path.split_once('?').map_or(path, |(path, _)| path))?;
            let fragment = fragment.map(decode).transpose_option()?;
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                base_document?.parent()?.join(path)
            };
            let mut normalized = PathBuf::new();
            for part in path.components() {
                match part {
                    std::path::Component::ParentDir => {
                        normalized.pop();
                    }
                    std::path::Component::CurDir => {}
                    part => normalized.push(part.as_os_str()),
                }
            }
            Some(Address::Local {
                path: normalized,
                fragment,
            })
        }
    }
}
/// Decode link components without applying this computer's filesystem rules.
pub fn provider_relative(destination: &str) -> Option<(String, Option<String>)> {
    let value = destination.trim();
    if !matches!(classify_link(value), LinkIntent::LocalDocument) {
        return None;
    }
    let (path, fragment) = value
        .split_once('#')
        .map_or((value, None), |(path, fragment)| (path, Some(fragment)));
    let path = decode(path.split_once('?').map_or(path, |(path, _)| path))?;
    if path.is_empty() || path.len() > 32 * 1024 {
        return None;
    }
    Some((path, fragment.map(decode).transpose_option()?))
}
trait TransposeOption<T> {
    fn transpose_option(self) -> Option<Option<T>>;
}
impl<T> TransposeOption<T> for Option<Option<T>> {
    fn transpose_option(self) -> Option<Option<T>> {
        match self {
            None => Some(None),
            Some(value) => value.map(Some),
        }
    }
}
pub fn headings(document: &MarkdownDocument) -> Vec<(String, NodeId)> {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    let mut used = std::collections::HashSet::new();
    document
        .headings
        .iter()
        .map(|heading| {
            let title = document.text(heading.display.clone()).unwrap_or("");
            let mut slug = String::new();
            for grapheme in title.trim().graphemes(true) {
                if grapheme.chars().any(char::is_alphanumeric) || grapheme == "_" || grapheme == "-"
                {
                    slug.extend(grapheme.chars().flat_map(char::to_lowercase));
                } else if grapheme.chars().all(char::is_whitespace) {
                    slug.push('-');
                }
            }
            if slug.is_empty() {
                slug.push_str("section");
            }
            let count = seen.entry(slug.clone()).or_default();
            let mut value = slug.clone();
            while used.contains(&value) {
                *count += 1;
                value = format!("{slug}-{}", *count);
            }
            used.insert(value.clone());
            (value, heading.node)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_relative_links_do_not_use_the_client_path_rules() {
        assert_eq!(
            provider_relative("../中文%5Cimage.png#title"),
            Some(("../中文\\image.png".into(), Some("title".into())))
        );
        assert_eq!(
            provider_relative("/target/image.png"),
            Some(("/target/image.png".into(), None))
        );
        assert_eq!(provider_relative("https://example.test/image.png"), None);
        assert_eq!(provider_relative("bad%00.png"), None);
    }
    #[test]
    fn paths_and_fragment_are_decoded_separately() {
        let base = Path::new("C:/docs/README.md");
        let Some(Address::Local { path, fragment }) =
            resolve(Some(base), "子目录/a%20b%23c.md?raw=1#%E4%B8%AD%E6%96%87")
        else {
            panic!()
        };
        assert!(path.to_string_lossy().contains("a b#c.md"));
        assert_eq!(fragment.as_deref(), Some("中文"));
        assert_eq!(resolve(None, "relative.png"), None);
        assert_eq!(resolve(Some(base), "x%00.md"), None);
        assert_eq!(resolve(Some(base), "javascript:x"), None);
    }
}
