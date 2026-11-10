//! User and project file associations: ordered path patterns that choose a
//! language before the built-in recognition runs. Pure and deterministic; the
//! application feeds in the settings and the workspace folders.
//!
//! Pattern syntax is a small, fixed subset so that it means the same everywhere:
//!
//! * `*` matches any run of characters inside one path segment, `?` matches one;
//! * `**` as a whole segment matches any number of segments, including none;
//! * a pattern without `/` matches the file name at any depth (`*.custom`);
//! * a pattern with `/` is matched against the path below the workspace folder,
//!   from its start (`docs/**/*.txt`, `**/Jenkinsfile`);
//! * `[`, `]`, `{`, `}` and `\` are rejected rather than half-supported.
use crate::domain::language::{LanguageId, Registry, ResolvedLanguage};
use std::path::{Component, Path, PathBuf};

/// Whether file names compare ignoring case on this platform.
pub const PATH_CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    Literal(char),
    AnyChar,
    AnyRun,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    /// `**`
    Any,
    Name(Vec<Piece>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    segments: Vec<Segment>,
    /// The pattern named directories (it contains `/`), so it is matched
    /// against the whole relative path rather than the file name.
    anchored: bool,
    ignore_case: bool,
}

impl Glob {
    pub fn parse(pattern: &str, ignore_case: bool) -> Result<Self, String> {
        let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
        if pattern.trim().is_empty() {
            return Err("the pattern is empty".into());
        }
        if pattern.starts_with('/') || pattern.ends_with('/') {
            return Err(format!(
                "`{pattern}` must not start or end with `/`; patterns match files below a workspace folder"
            ));
        }
        if let Some(bad) = pattern
            .chars()
            .find(|c| matches!(c, '[' | ']' | '{' | '}' | '\\') || c.is_control())
        {
            return Err(format!("`{pattern}`: `{bad}` is not supported in patterns"));
        }
        let mut segments = Vec::new();
        for segment in pattern.split('/') {
            if segment.is_empty() {
                return Err(format!("`{pattern}` has an empty path segment"));
            }
            if segment == "**" {
                segments.push(Segment::Any);
                continue;
            }
            if segment.contains("**") {
                return Err(format!("`{pattern}`: `**` must be a whole path segment"));
            }
            segments.push(Segment::Name(
                segment
                    .chars()
                    .map(|c| match c {
                        '*' => Piece::AnyRun,
                        '?' => Piece::AnyChar,
                        other => Piece::Literal(other),
                    })
                    .collect(),
            ));
        }
        if segments.last() == Some(&Segment::Any) {
            return Err(format!("`{pattern}` must end with a file name pattern"));
        }
        Ok(Self {
            anchored: segments.len() > 1,
            segments,
            ignore_case,
        })
    }

    /// Whether the pattern could apply to a file that sits in no workspace
    /// folder: only plain file name patterns can.
    pub fn matches_without_folder(&self) -> bool {
        !self.anchored
    }

    /// `relative` is the file's path segments below its workspace folder, or just
    /// its name when it has none.
    pub fn matches(&self, relative: &[&str]) -> bool {
        match (self.anchored, relative.last()) {
            (_, None) => false,
            (false, Some(name)) => match &self.segments[0] {
                Segment::Name(pieces) => self.segment(pieces, name),
                Segment::Any => false,
            },
            (true, Some(_)) => self.path(&self.segments, relative),
        }
    }

    fn path(&self, pattern: &[Segment], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((Segment::Any, rest)) => {
                (0..=path.len()).any(|skip| self.path(rest, &path[skip..]))
            }
            Some((Segment::Name(pieces), rest)) => path
                .split_first()
                .is_some_and(|(first, more)| self.segment(pieces, first) && self.path(rest, more)),
        }
    }

    fn segment(&self, pieces: &[Piece], text: &str) -> bool {
        let text: Vec<char> = text.chars().collect();
        let same = |a: char, b: char| {
            a == b || (self.ignore_case && a.to_lowercase().eq(b.to_lowercase()))
        };
        // Classic iterative wildcard match with backtracking to the last `*`.
        let (mut p, mut t) = (0, 0);
        let mut restart: Option<(usize, usize)> = None;
        while t < text.len() {
            match pieces.get(p) {
                Some(Piece::AnyRun) => {
                    restart = Some((p, t));
                    p += 1;
                }
                Some(Piece::AnyChar) => {
                    p += 1;
                    t += 1;
                }
                Some(Piece::Literal(c)) if same(*c, text[t]) => {
                    p += 1;
                    t += 1;
                }
                _ => match restart {
                    Some((star, at)) => {
                        restart = Some((star, at + 1));
                        p = star + 1;
                        t = at + 1;
                    }
                    None => return false,
                },
            }
        }
        pieces[p..].iter().all(|piece| *piece == Piece::AnyRun)
    }
}

/// One association as written in settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssociationSpec<'a> {
    pub pattern: &'a str,
    pub language: &'a str,
}

/// Check the saved rule without requiring its language extension to be active.
pub fn validate(spec: AssociationSpec<'_>) -> Result<(), String> {
    Glob::parse(spec.pattern, PATH_CASE_INSENSITIVE)?;
    if !crate::domain::language::descriptor::is_canonical_name(spec.language) {
        return Err(format!("invalid language name {}", spec.language));
    }
    Ok(())
}

fn language_of(registry: &Registry, name: &str) -> Result<LanguageId, String> {
    registry
        .language_by_name(name)
        .ok_or_else(|| format!("unknown language `{name}`"))
}

#[derive(Clone, Debug)]
struct Rule {
    glob: Glob,
    language: ResolvedLanguage,
}

/// The compiled associations of the user and of each project.
#[derive(Clone, Debug, Default)]
pub struct LanguageRules {
    roots: Vec<PathBuf>,
    projects: Vec<(PathBuf, Vec<Rule>)>,
    user: Vec<Rule>,
}

impl LanguageRules {
    /// `roots` are the workspace folders. A project's rules apply to files in its
    /// folder and come before the user's. A rule that cannot be compiled is left
    /// out and reported; the others still apply.
    pub fn compile(
        registry: &Registry,
        ignore_case: bool,
        roots: &[PathBuf],
        user: &[AssociationSpec<'_>],
        projects: &[(PathBuf, Vec<AssociationSpec<'_>>)],
    ) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut compile = |origin: &str, specs: &[AssociationSpec<'_>]| {
            let mut rules = Vec::new();
            for (index, spec) in specs.iter().enumerate() {
                let rule = Glob::parse(spec.pattern, ignore_case).and_then(|glob| {
                    let language = registry.default_resolved(language_of(registry, spec.language)?);
                    Ok(Rule { glob, language })
                });
                match rule {
                    Ok(rule) => rules.push(rule),
                    Err(error) => {
                        errors.push(format!("{origin} file_associations[{index}]: {error}"))
                    }
                }
            }
            rules
        };
        let user = compile("user", user);
        let projects = projects
            .iter()
            .map(|(root, specs)| (root.clone(), compile(&root.display().to_string(), specs)))
            .collect();
        (
            Self {
                roots: roots.to_vec(),
                projects,
                user,
            },
            errors,
        )
    }

    pub fn is_empty(&self) -> bool {
        self.user.is_empty() && self.projects.iter().all(|(_, rules)| rules.is_empty())
    }

    /// The innermost workspace folder holding `path`.
    fn root_of(&self, path: &Path) -> Option<&Path> {
        self.roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .map(PathBuf::as_path)
    }

    /// The first rule that matches `path`: its project's rules, then the user's.
    pub fn resolve(&self, path: &Path) -> Option<ResolvedLanguage> {
        let name = path.file_name()?.to_str()?;
        let root = self.root_of(path);
        let below_root: Vec<String> = match root.and_then(|root| path.strip_prefix(root).ok()) {
            Some(relative) => relative
                .components()
                .filter_map(|part| match part {
                    Component::Normal(part) => part.to_str().map(str::to_owned),
                    _ => None,
                })
                .collect(),
            None => vec![name.to_owned()],
        };
        let relative: Vec<&str> = below_root.iter().map(String::as_str).collect();
        let projects = root.into_iter().flat_map(|root| {
            self.projects
                .iter()
                .filter(move |(project, _)| project == root)
                .flat_map(|(_, rules)| rules)
        });
        projects
            .chain(&self.user)
            .filter(|rule| root.is_some() || rule.glob.matches_without_folder())
            .find(|rule| rule.glob.matches(&relative))
            .map(|rule| rule.language.clone())
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::language::Registry;
    use crate::domain::language::rules::*;
    use std::path::PathBuf;

    fn glob(pattern: &str) -> Glob {
        Glob::parse(pattern, false).unwrap()
    }
    fn hit(pattern: &str, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        glob(pattern).matches(&parts)
    }

    #[test]
    fn plain_patterns_match_the_file_name_at_any_depth() {
        assert!(hit("*.custom", "a.custom"));
        assert!(hit("*.custom", "deep/er/a.custom"));
        assert!(!hit("*.custom", "a.custom.bak"));
        assert!(hit("Jenkinsfile", "ci/Jenkinsfile"));
        assert!(hit("data-?.txt", "x/data-1.txt"));
        assert!(!hit("data-?.txt", "x/data-12.txt"));
        assert!(hit("*", "anything"));
        assert!(hit("a*b*c", "aXXbYYc") && hit("a*b*c", "abc") && !hit("a*b*c", "aXXbYY"));
        assert!(hit("*.rs", ".rs"), "`*` also matches nothing");
    }

    #[test]
    fn patterns_with_a_slash_are_anchored_below_the_folder() {
        assert!(hit("docs/*.txt", "docs/a.txt"));
        assert!(!hit("docs/*.txt", "other/docs/a.txt"));
        assert!(!hit("docs/*.txt", "docs/sub/a.txt"));
        assert!(hit("docs/**/*.txt", "docs/a.txt"), "** matches no segments");
        assert!(hit("docs/**/*.txt", "docs/a/b/c.txt"));
        assert!(hit("**/*.custom", "a.custom") && hit("**/*.custom", "x/y/a.custom"));
        assert!(hit("**/config/*.json", "a/b/config/x.json"));
        assert!(!hit("**/config/*.json", "a/config/b/x.json"));
        assert!(hit("./src/*.rs", "src/a.rs"), "a leading ./ is ignored");
        assert!(hit("a/**/b/**/c", "a/b/c") && hit("a/**/b/**/c", "a/x/b/y/z/c"));
    }

    #[test]
    fn case_follows_the_platform_policy() {
        let sensitive = Glob::parse("*.Custom", false).unwrap();
        let insensitive = Glob::parse("*.Custom", true).unwrap();
        assert!(!sensitive.matches(&["a.custom"]));
        assert!(insensitive.matches(&["a.custom"]) && insensitive.matches(&["A.CUSTOM"]));
        assert!(
            Glob::parse("Docs/*.TXT", true)
                .unwrap()
                .matches(&["docs", "a.txt"])
        );
        assert!(
            !Glob::parse("Docs/*.TXT", false)
                .unwrap()
                .matches(&["docs", "a.txt"])
        );
    }

    #[test]
    fn invalid_patterns_are_rejected_with_a_reason() {
        for (pattern, expected) in [
            ("", "empty"),
            ("   ", "empty"),
            ("/abs/*.x", "must not start or end"),
            ("dir/", "must not start or end"),
            ("a//b", "empty path segment"),
            ("a**b", "whole path segment"),
            ("**", "end with a file name"),
            ("dir/**", "end with a file name"),
            ("[ab].txt", "not supported"),
            ("{a,b}.txt", "not supported"),
            ("a\\b", "not supported"),
        ] {
            let error = Glob::parse(pattern, false).unwrap_err();
            assert!(error.contains(expected), "{pattern:?}: {error}");
        }
    }

    fn spec<'a>(pattern: &'a str, language: &'a str) -> AssociationSpec<'a> {
        AssociationSpec { pattern, language }
    }
    fn name(rules: &LanguageRules, path: &str) -> Option<String> {
        rules
            .resolve(std::path::Path::new(path))
            .map(|resolved| resolved.definition().name.to_string())
    }
    fn roots() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/work/app"),
            PathBuf::from("/work/app/vendor"),
            PathBuf::from("/other"),
        ]
    }

    #[test]
    fn project_rules_come_first_then_user_rules_and_the_first_match_wins() {
        let registry = Registry::builtin();
        let (rules, errors) = LanguageRules::compile(
            &registry,
            false,
            &roots(),
            &[
                spec("*.cfg", "rust"),
                spec("*.cfg", "csharp"),
                spec("*.mine", "javascript"),
            ],
            &[(
                PathBuf::from("/work/app"),
                vec![spec("*.cfg", "typescript"), spec("gen/**/*.out", "csharp")],
            )],
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            name(&rules, "/work/app/a.cfg").as_deref(),
            Some("typescript"),
            "project beats user"
        );
        assert_eq!(
            name(&rules, "/other/a.cfg").as_deref(),
            Some("rust"),
            "user rules elsewhere; first wins"
        );
        assert_eq!(
            name(&rules, "/work/app/gen/x/y.out").as_deref(),
            Some("csharp")
        );
        assert_eq!(
            name(&rules, "/work/app/y.out"),
            None,
            "anchored below the folder"
        );
        assert_eq!(
            name(&rules, "/other/gen/y.out"),
            None,
            "project rules stay in their folder"
        );
        assert_eq!(
            name(&rules, "/work/app/x.mine").as_deref(),
            Some("javascript")
        );
        assert_eq!(
            name(&rules, "/work/app/x.rs"),
            None,
            "builtin recognition is the caller's fallback"
        );
    }

    #[test]
    fn the_innermost_folder_owns_a_file_for_project_rules() {
        let registry = Registry::builtin();
        let (rules, errors) = LanguageRules::compile(
            &registry,
            false,
            &roots(),
            &[],
            &[
                (PathBuf::from("/work/app"), vec![spec("*.x", "rust")]),
                (
                    PathBuf::from("/work/app/vendor"),
                    vec![spec("*.x", "csharp")],
                ),
            ],
        );
        assert!(errors.is_empty());
        assert_eq!(
            name(&rules, "/work/app/vendor/a.x").as_deref(),
            Some("csharp")
        );
        assert_eq!(name(&rules, "/work/app/src/a.x").as_deref(), Some("rust"));
    }

    #[test]
    fn files_outside_every_folder_only_match_plain_name_patterns() {
        let registry = Registry::builtin();
        let (rules, _) = LanguageRules::compile(
            &registry,
            false,
            &roots(),
            &[
                spec("*.loose", "rust"),
                spec("**/*.deep", "csharp"),
                spec("src/*.x", "javascript"),
            ],
            &[],
        );
        assert_eq!(name(&rules, "/nowhere/a.loose").as_deref(), Some("rust"));
        assert_eq!(
            name(&rules, "/nowhere/a.deep"),
            None,
            "needs a folder to anchor to"
        );
        assert_eq!(name(&rules, "/nowhere/src/a.x"), None);
        assert_eq!(
            name(&rules, "/work/app/src/a.x").as_deref(),
            Some("javascript")
        );
        assert_eq!(
            name(&rules, "/work/app/q/a.deep").as_deref(),
            Some("csharp")
        );
    }

    #[test]
    fn bad_rules_are_reported_and_the_rest_still_apply() {
        let registry = Registry::builtin();
        let (rules, errors) = LanguageRules::compile(
            &registry,
            false,
            &roots(),
            &[
                spec("[x].a", "rust"),
                spec("*.b", "klingon"),
                spec("*.c", "ts"),
                spec("", "rust"),
            ],
            &[(PathBuf::from("/work/app"), vec![spec("*.d", "nope")])],
        );
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(
            errors[0].contains("user file_associations[0]") && errors[0].contains("not supported")
        );
        assert!(errors[1].contains("[1]") && errors[1].contains("unknown language `klingon`"));
        assert!(errors[3].contains("/work/app") || errors[3].contains("work"));
        assert_eq!(
            name(&rules, "/other/a.c").as_deref(),
            Some("typescript"),
            "aliases work"
        );
        assert!(!rules.is_empty());
        assert!(LanguageRules::default().is_empty());
    }

    #[test]
    fn a_well_formed_rule_can_name_an_unavailable_language() {
        assert!(validate(spec("**/*.custom", "rust")).is_ok());
        assert!(validate(spec("**/*.custom", "cs")).is_ok());
        assert!(validate(spec("**/*.custom", "not_installed")).is_ok());
        assert!(validate(spec("**/*.custom", "not valid")).is_err());
        assert!(validate(spec("{a}", "rust")).is_err());
    }
}
