//! Command references and keybinding precedence (design §6.1).
use std::cmp::Reverse;

/// Outer command reference: built-in actions keep their own type-checked enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandRef<B> {
    Builtin(B),
    Extension(String),
}

/// Where a binding came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    User,
    Extension {
        id: String,
        /// User-configured priority; larger wins, unset counts as 0.
        priority: i64,
    },
    Builtin,
}

impl Source {
    fn rank(&self) -> u8 {
        match self {
            Self::User => 2,
            Self::Extension { .. } => 1,
            Self::Builtin => 0,
        }
    }
}

/// One binding that matches the current key sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate<C> {
    /// How specific the matching context is; deeper wins first.
    pub context_depth: u32,
    pub source: Source,
    /// Position within its own source; later declarations win.
    pub declared: usize,
    /// `None` is an explicit `null` binding that disables the key.
    pub command: Option<C>,
}

/// Pick the winning binding. Order: context depth, then User > Extension >
/// Builtin, then (extensions only) priority and the smaller ASCII id, then the
/// later declaration. Install or activation order never decides.
pub fn resolve<C: Clone>(candidates: &[Candidate<C>]) -> Option<C> {
    candidates
        .iter()
        .max_by_key(|candidate| {
            let (priority, id) = match &candidate.source {
                Source::Extension { id, priority } => (*priority, id.clone()),
                _ => (0, String::new()),
            };
            (
                candidate.context_depth,
                candidate.source.rank(),
                priority,
                Reverse(id),
                candidate.declared,
            )
        })
        .and_then(|winner| winner.command.clone())
}

/// Conflicts worth reporting: more than one distinct command at the winning level.
pub fn conflicts<C: Clone + PartialEq>(candidates: &[Candidate<C>]) -> bool {
    let Some(top) = candidates
        .iter()
        .map(|c| (c.context_depth, c.source.rank()))
        .max()
    else {
        return false;
    };
    let mut commands = candidates
        .iter()
        .filter(|c| (c.context_depth, c.source.rank()) == top)
        .map(|c| &c.command);
    let first = commands.next();
    commands.any(|other| Some(other) != first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(
        id: &str,
        priority: i64,
        declared: usize,
        command: &'static str,
    ) -> Candidate<&'static str> {
        Candidate {
            context_depth: 1,
            source: Source::Extension {
                id: id.into(),
                priority,
            },
            declared,
            command: Some(command),
        }
    }

    fn user(declared: usize, command: Option<&'static str>) -> Candidate<&'static str> {
        Candidate {
            context_depth: 1,
            source: Source::User,
            declared,
            command,
        }
    }

    fn builtin(declared: usize, command: &'static str) -> Candidate<&'static str> {
        Candidate {
            context_depth: 1,
            source: Source::Builtin,
            declared,
            command: Some(command),
        }
    }

    #[test]
    fn deeper_context_beats_a_higher_source() {
        let mut deep = builtin(0, "deep");
        deep.context_depth = 3;
        assert_eq!(resolve(&[user(0, Some("user")), deep]), Some("deep"));
    }

    #[test]
    fn source_order_is_user_then_extension_then_builtin() {
        assert_eq!(
            resolve(&[builtin(0, "b"), ext("a.x", 0, 0, "e")]),
            Some("e")
        );
        assert_eq!(
            resolve(&[ext("a.x", 0, 0, "e"), user(0, Some("u"))]),
            Some("u")
        );
    }

    #[test]
    fn extensions_compare_priority_then_ascii_id_then_declaration() {
        assert_eq!(
            resolve(&[ext("a.x", 0, 0, "low"), ext("b.y", 5, 0, "high")]),
            Some("high")
        );
        assert_eq!(
            resolve(&[ext("b.y", 0, 0, "b"), ext("a.x", 0, 0, "a")]),
            Some("a")
        );
        assert_eq!(
            resolve(&[ext("a.x", 0, 0, "first"), ext("a.x", 0, 1, "later")]),
            Some("later")
        );
    }

    #[test]
    fn result_does_not_depend_on_input_order() {
        let set = [
            ext("b.y", 0, 0, "b"),
            ext("a.x", 0, 0, "a"),
            builtin(0, "z"),
        ];
        let mut reversed = set.clone();
        reversed.reverse();
        assert_eq!(resolve(&set), resolve(&reversed));
    }

    #[test]
    fn user_null_disables_the_key_and_a_later_user_binding_revives_it() {
        assert_eq!(resolve(&[builtin(0, "b"), user(0, None)]), None);
        assert_eq!(
            resolve(&[user(0, None), user(1, Some("again"))]),
            Some("again")
        );
    }

    #[test]
    fn same_level_disagreement_is_a_conflict() {
        assert!(conflicts(&[ext("a.x", 0, 0, "a"), ext("b.y", 0, 0, "b")]));
        assert!(!conflicts(&[ext("a.x", 0, 0, "a"), builtin(0, "b")]));
    }
}
