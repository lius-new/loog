//! Version ranges in manifests: `>=0.1.8 <0.3`, `^22`, `~1.2`, `1.4.0`.
//!
//! A range is comparators separated by spaces, all of which must hold. A
//! version may leave out its minor and patch (`<0.3` means below `0.3.0`).
//! Pre-release and build parts of a version are ignored.
use std::cmp::Ordering;

/// `major.minor.patch`, compared field by field.
pub type Version = [u64; 3];

/// Parse `1`, `1.2` or `1.2.3` (missing fields are 0); how many were given is
/// returned too, since `^` and `~` depend on it.
fn parse_partial(text: &str) -> Option<(Version, usize)> {
    let core = text.split(['-', '+']).next()?;
    let mut version = [0; 3];
    let mut given = 0;
    for (index, part) in core.split('.').enumerate() {
        if index == 3 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        version[index] = part.parse().ok()?;
        given = index + 1;
    }
    Some((version, given))
}

/// A plain version such as `0.1.8`.
pub fn parse_version(text: &str) -> Option<Version> {
    parse_partial(text)
        .filter(|(_, given)| *given == 3)
        .map(|(v, _)| v)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
    Caret,
    Tilde,
}

/// A range such as `>=0.1.8 <0.3`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range(Vec<(Operator, Version, usize)>);

impl Range {
    pub fn parse(text: &str) -> Option<Self> {
        let mut comparators = Vec::new();
        for word in text.split_whitespace() {
            let (operator, rest) = [
                (">=", Operator::Ge),
                ("<=", Operator::Le),
                (">", Operator::Gt),
                ("<", Operator::Lt),
                ("=", Operator::Eq),
                ("^", Operator::Caret),
                ("~", Operator::Tilde),
            ]
            .into_iter()
            .find_map(|(prefix, operator)| word.strip_prefix(prefix).map(|rest| (operator, rest)))
            .unwrap_or((Operator::Eq, word));
            let (version, given) = parse_partial(rest)?;
            comparators.push((operator, version, given));
        }
        (!comparators.is_empty()).then_some(Self(comparators))
    }

    pub fn matches(&self, version: Version) -> bool {
        self.0.iter().all(|&(operator, bound, given)| {
            let order = version.cmp(&bound);
            match operator {
                Operator::Ge => order != Ordering::Less,
                Operator::Gt => {
                    // `>1.2` means 1.3.0 and up: everything below it is "1.2.x".
                    if given == 3 {
                        order == Ordering::Greater
                    } else {
                        version >= upper(bound, given)
                    }
                }
                Operator::Lt => order == Ordering::Less,
                Operator::Le => {
                    if given == 3 {
                        order != Ordering::Greater
                    } else {
                        version < upper(bound, given)
                    }
                }
                Operator::Eq => version >= bound && version < upper(bound, given),
                Operator::Tilde => version >= bound && version < upper(bound, given.min(2)),
                Operator::Caret => version >= bound && version < caret_end(bound, given),
            }
        })
    }
}

/// The first version after everything that starts with the first `given` fields.
fn upper(bound: Version, given: usize) -> Version {
    match given {
        1 => [bound[0] + 1, 0, 0],
        2 => [bound[0], bound[1] + 1, 0],
        _ => [bound[0], bound[1], bound[2] + 1],
    }
}

/// `^1.2.3` stops at 2.0.0, `^0.2.3` at 0.3.0, `^0.0.3` at 0.0.4.
fn caret_end(bound: Version, given: usize) -> Version {
    if bound[0] > 0 || given == 1 {
        [bound[0] + 1, 0, 0]
    } else if bound[1] > 0 || given == 2 {
        [0, bound[1] + 1, 0]
    } else {
        [0, 0, bound[2] + 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holds(range: &str, version: &str) -> bool {
        Range::parse(range)
            .unwrap()
            .matches(parse_version(version).unwrap())
    }

    #[test]
    fn bounds_and_partial_versions() {
        assert!(holds(">=0.1.8 <0.3", "0.1.8"));
        assert!(holds(">=0.1.8 <0.3", "0.2.9"));
        assert!(!holds(">=0.1.8 <0.3", "0.1.7"));
        assert!(!holds(">=0.1.8 <0.3", "0.3.0"));
        assert!(holds("1.4", "1.4.9"));
        assert!(!holds("1.4", "1.5.0"));
        assert!(holds(">1.2", "1.3.0"));
        assert!(!holds(">1.2", "1.2.9"));
    }

    #[test]
    fn caret_and_tilde() {
        assert!(holds("^22", "22.11.0"));
        assert!(!holds("^22", "23.0.0"));
        assert!(holds("^0.2.3", "0.2.9"));
        assert!(!holds("^0.2.3", "0.3.0"));
        assert!(holds("~1.2", "1.2.7"));
        assert!(!holds("~1.2", "1.3.0"));
    }

    #[test]
    fn nonsense_is_not_a_range() {
        for text in ["", "latest", ">=", "1.2.3.4", "^x", ">= 1"] {
            assert!(Range::parse(text).is_none(), "{text}");
        }
    }
}
