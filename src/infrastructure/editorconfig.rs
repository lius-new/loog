//! EditorConfig is read during file preparation, away from input and paint.
use loog_editor_core::text::{FileOptions, LineEnding};
use std::{collections::BTreeMap, path::Path};

pub fn load(path: &Path) -> FileOptions {
    let mut sources = Vec::new();
    for dir in path
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .take(128)
    {
        let file = dir.join(".editorconfig");
        let Ok(meta) = std::fs::metadata(&file) else {
            continue;
        };
        if meta.len() > 64 * 1024 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let root = is_root(&text);
        sources.push((dir.to_path_buf(), text));
        if root {
            break;
        }
    }
    let mut values = BTreeMap::new();
    for (dir, text) in sources.into_iter().rev() {
        let relative = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        apply(&text, &relative, &mut values);
    }
    options(&values)
}

pub(crate) fn is_root(text: &str) -> bool {
    text.trim_start_matches('\u{feff}')
        .lines()
        .take_while(|line| !line.trim().starts_with('['))
        .any(|line| {
            line.split_once('=').is_some_and(|(key, value)| {
                key.trim().eq_ignore_ascii_case("root") && value.trim().eq_ignore_ascii_case("true")
            })
        })
}

/// Already-read sources in ancestor-to-descendant order. Relative names are
/// supplied by the resource provider; this function performs no filesystem IO.
pub(crate) fn from_sources(sources: impl IntoIterator<Item = (String, String)>) -> FileOptions {
    let mut values = BTreeMap::new();
    for (text, relative) in sources {
        apply(&text, &relative, &mut values);
    }
    options(&values)
}

fn apply(text: &str, relative: &str, values: &mut BTreeMap<String, String>) {
    let mut matched = false;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(pattern) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
            let pattern = if line.starts_with("[/") || pattern.contains('/') {
                pattern.to_owned()
            } else {
                format!("**/{pattern}")
            };
            matched = globset::GlobBuilder::new(&pattern)
                .literal_separator(true)
                .backslash_escape(true)
                .build()
                // Match the supplied text directly: Path-based matching on a
                // Windows client must not reinterpret a target backslash name.
                .is_ok_and(|glob| {
                    regex::bytes::Regex::new(glob.regex())
                        .is_ok_and(|pattern| pattern.is_match(relative.as_bytes()))
                });
        } else if matched && let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().to_ascii_lowercase();
            if value == "unset" {
                values.remove(&key);
            } else {
                values.insert(key, value);
            }
        }
    }
}
fn options(values: &BTreeMap<String, String>) -> FileOptions {
    let number = |key| {
        values
            .get(key)
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|v| (1..=16).contains(v))
    };
    let tab = number("tab_width");
    let indent = number("indent_size");
    FileOptions {
        indent_size_from_tab: values.get("indent_size").is_some_and(|s| s == "tab"),
        indent_size: if values.get("indent_size").is_some_and(|s| s == "tab") {
            tab
        } else {
            indent
        },
        tab_width: tab.or(indent),
        use_tabs: values.get("indent_style").and_then(|v| match v.as_str() {
            "space" => Some(false),
            "tab" => Some(true),
            _ => None,
        }),
        line_ending: values.get("end_of_line").and_then(|v| match v.as_str() {
            "lf" => Some(LineEnding::Lf),
            "crlf" => Some(LineEnding::Crlf),
            _ => None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_overrides_unset_and_tab_width_follow_directory_order() {
        let mut values = BTreeMap::new();
        apply(
            "root = true\n[*]\nindent_style=space\nindent_size=4\nend_of_line=crlf\n[*.{rs,go}]\nindent_style=tab\n",
            "src/test.rs",
            &mut values,
        );
        assert_eq!(options(&values).use_tabs, Some(true));
        assert_eq!(options(&values).tab_width, Some(4));
        apply(
            "[src/[a-z]*.rs]\nindent_size=tab\ntab_width=8\nend_of_line=unset\n",
            "src/test.rs",
            &mut values,
        );
        assert_eq!(options(&values).indent_size, Some(8));
        assert_eq!(options(&values).line_ending, None);
        apply("[/test.rs]\nindent_size=2", "src/test.rs", &mut values);
        assert_eq!(options(&values).indent_size, Some(8));
    }
}
