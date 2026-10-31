//! Stacks query files for one grammar: its own, then those extensions add, then
//! the user's.
//!
//! A layer that starts with a `; extends` comment is appended to what is below
//! it, so it can add a few patterns to a grammar it does not own. Any other layer
//! replaces everything below it. Appended patterns come later in the text, and
//! the highlighter lets the later pattern win for the same range, so an added
//! pattern overrides the one it competes with.
//!
//! Each layer is checked on its own against what is below it. A layer that does
//! not compile is left out and reported, so a typo in a custom query costs only
//! that layer, never the language's highlighting.
//!
//! The result remembers which file each part of the text came from, so a pattern
//! can be traced back to a file and line (see `inspect`).
use std::ops::Range;

/// One query file above the grammar's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layer {
    /// Where it came from, for messages.
    pub source: String,
    pub text: String,
}

/// A layer that was left out, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rejected {
    pub source: String,
    pub error: String,
}

/// The part of the stacked text that one file supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Origin {
    pub range: Range<usize>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Stacked {
    pub text: String,
    pub origins: Vec<Origin>,
    pub rejected: Vec<Rejected>,
}

/// Whether the first non-blank line is a comment that says `extends`.
pub(crate) fn extends(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with(';') && line.contains("extends"))
}

/// `base` (the grammar's own files, as `(source, text)`) with `layers` stacked on
/// it. `check` compiles a candidate text; a layer whose result it refuses is
/// skipped. The base itself is not checked here, so its own failure is still
/// reported where it always was.
pub(crate) fn stack(
    base: Vec<(String, String)>,
    layers: &[Layer],
    mut check: impl FnMut(&str) -> Result<(), String>,
) -> Stacked {
    let mut stacked = Stacked::default();
    for (source, text) in base {
        let start = stacked.text.len();
        stacked.text.push_str(&text);
        stacked.origins.push(Origin {
            range: start..stacked.text.len(),
            source,
        });
        stacked.text.push('\n');
    }
    for layer in layers {
        let appends = extends(&layer.text);
        let (candidate, range) = if appends {
            let start = stacked.text.len() + 1;
            (
                format!("{}\n{}\n", stacked.text, layer.text),
                start..start + layer.text.len(),
            )
        } else {
            (layer.text.clone(), 0..layer.text.len())
        };
        match check(&candidate) {
            Ok(()) => {
                stacked.text = candidate;
                if !appends {
                    stacked.origins.clear();
                }
                stacked.origins.push(Origin {
                    range,
                    source: layer.source.clone(),
                });
            }
            Err(error) => stacked.rejected.push(Rejected {
                source: layer.source.clone(),
                error,
            }),
        }
    }
    stacked
}

/// The file and 1-based line that supplied byte `at` of `text`.
pub(crate) fn locate(text: &str, origins: &[Origin], at: usize) -> Option<(String, usize)> {
    let origin = origins.iter().find(|origin| origin.range.contains(&at))?;
    let line = text.get(origin.range.start..at)?.matches('\n').count() + 1;
    Some((origin.source.clone(), line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(source: &str, text: &str) -> Layer {
        Layer {
            source: source.into(),
            text: text.into(),
        }
    }

    fn base(text: &str) -> Vec<(String, String)> {
        vec![("grammar".into(), text.into())]
    }

    #[test]
    fn extends_is_the_first_non_blank_comment_line() {
        assert!(extends("; extends\n(a) @b"));
        assert!(extends("\n\n  ;; extends the base\n(a) @b"));
        assert!(!extends("(a) @b\n; extends"));
        assert!(!extends("; just a note\n(a) @b"));
        assert!(!extends(""));
    }

    #[test]
    fn extending_layers_append_and_the_others_replace() {
        let layers = [
            layer("ext", "; extends\n(x) @keyword"),
            layer("user", "; extends\n(y) @string"),
        ];
        let stacked = stack(base("(base) @type"), &layers, |_| Ok(()));
        assert!(stacked.rejected.is_empty());
        let order: Vec<_> = ["(base)", "(x)", "(y)"]
            .iter()
            .map(|part| stacked.text.find(part).unwrap())
            .collect();
        assert!(
            order.windows(2).all(|pair| pair[0] < pair[1]),
            "{}",
            stacked.text
        );

        let replaced = stack(
            base("(base) @type"),
            &[layer("user", "(only) @string")],
            |_| Ok(()),
        );
        assert_eq!(replaced.text, "(only) @string");
        assert_eq!(replaced.origins.len(), 1);
    }

    #[test]
    fn a_layer_that_fails_the_check_is_left_out_and_reported() {
        let layers = [
            layer("ext", "; extends\n(bad"),
            layer("user", "; extends\n(good) @string"),
        ];
        let stacked = stack(base("(base) @type"), &layers, |candidate| {
            if candidate.contains("(bad") {
                Err("unbalanced".into())
            } else {
                Ok(())
            }
        });
        assert!(stacked.text.contains("(good)") && !stacked.text.contains("(bad"));
        assert_eq!(
            stacked.rejected,
            [Rejected {
                source: "ext".into(),
                error: "unbalanced".into()
            }]
        );
    }

    #[test]
    fn a_replacing_layer_that_fails_leaves_the_layers_below_it() {
        let layers = [
            layer("ext", "; extends\n(x) @keyword"),
            layer("user", "(broken"),
        ];
        let stacked = stack(base("(base) @type"), &layers, |candidate| {
            if candidate == "(broken" {
                Err("syntax".into())
            } else {
                Ok(())
            }
        });
        assert!(stacked.text.contains("(base)") && stacked.text.contains("(x)"));
        assert_eq!(stacked.rejected.len(), 1);
    }

    #[test]
    fn a_byte_is_traced_to_its_file_and_line() {
        let layers = [layer("ext.scm", "; extends\n(x) @keyword\n(y) @string")];
        let stacked = stack(
            vec![
                ("a.scm".into(), "(one) @type\n(two) @type".into()),
                ("b.scm".into(), "(three) @type".into()),
            ],
            &layers,
            |_| Ok(()),
        );
        let at = |needle: &str| {
            locate(
                &stacked.text,
                &stacked.origins,
                stacked.text.find(needle).unwrap(),
            )
        };
        assert_eq!(at("(one)"), Some(("a.scm".into(), 1)));
        assert_eq!(at("(two)"), Some(("a.scm".into(), 2)));
        assert_eq!(at("(three)"), Some(("b.scm".into(), 1)));
        assert_eq!(at("(x)"), Some(("ext.scm".into(), 2)));
        assert_eq!(at("(y)"), Some(("ext.scm".into(), 3)));
        let replaced = stack(base("(a)"), &[layer("user", "\n(b) @x")], |_| Ok(()));
        assert_eq!(
            locate(
                &replaced.text,
                &replaced.origins,
                replaced.text.find("(b)").unwrap()
            ),
            Some(("user".into(), 2))
        );
    }
}
