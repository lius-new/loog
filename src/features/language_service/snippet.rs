//! LSP snippets (`insertTextFormat: 2`): text with tab stops, placeholders,
//! choices and variables.
//!
//! [`parse`] turns the snippet source into the text to insert and, for each tab
//! stop, where it sits in that text. Tab stops with the same index are mirrors:
//! typing into the first is copied into the others. Stop `0` is where the caret
//! ends; a snippet without one ends at its end.
#![allow(clippy::single_range_in_vec_init)]
use std::ops::Range;
mod transform;
pub use self::transform::Transform;

/// Where one tab stop (and its mirrors) are in the inserted text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabStop {
    pub index: u32,
    /// The first range is the stop; the others mirror it. A placeholder's
    /// default text is the content of its range.
    pub ranges: Vec<Range<usize>>,
    /// The values offered by a choice (`${1|a,b|}`); empty for other stops.
    pub choices: Vec<String>,
    /// Parallel to ranges. These occurrences are transformed on leaving the
    /// stop, while ordinary mirrors continue to show the entered value.
    pub transforms: Vec<Option<Transform>>,
    /// Source order disambiguates several empty occurrences at one byte offset.
    pub orders: Vec<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snippet {
    /// The text to insert, with placeholders expanded to their defaults.
    pub text: String,
    /// In visiting order: 1, 2, 3 … and last 0 (added at the end if missing).
    pub stops: Vec<TabStop>,
}

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Stop { index: u32, children: Vec<Node> },
    Choice { index: u32, options: Vec<String> },
    Variable { name: String, children: Vec<Node> },
    Transform { index: u32, transform: Transform },
    VariableTransform { name: String, transform: Transform },
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    depth: usize,
    transforms: usize,
}

impl Parser<'_> {
    fn transform(&mut self) -> Option<Transform> {
        self.transforms += 1;
        if self.transforms > 128 {
            return None;
        }
        Transform::parse(&mut self.chars)
    }
    fn children(&mut self) -> Vec<Node> {
        if self.depth >= 64 {
            let mut text = String::new();
            let mut braces = 0usize;
            while let Some(&c) = self.chars.peek() {
                if c == '}' && braces == 0 {
                    break;
                }
                self.chars.next();
                if c == '{' {
                    braces += 1;
                } else if c == '}' {
                    braces = braces.saturating_sub(1);
                }
                text.push(c);
            }
            return vec![Node::Text(text)];
        }
        self.depth += 1;
        let nodes = self.nodes(Some('}'));
        self.depth -= 1;
        nodes
    }
    /// Nodes until `end` (a closing brace of the enclosing construct) or the end
    /// of the input. `end` is not consumed.
    fn nodes(&mut self, end: Option<char>) -> Vec<Node> {
        let mut nodes = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, nodes: &mut Vec<Node>| {
            if !text.is_empty() {
                nodes.push(Node::Text(std::mem::take(text)));
            }
        };
        while let Some(&c) = self.chars.peek() {
            if Some(c) == end {
                break;
            }
            self.chars.next();
            match c {
                '\\' => match self.chars.peek() {
                    Some(&next @ ('$' | '}' | '\\')) => {
                        self.chars.next();
                        text.push(next);
                    }
                    _ => text.push('\\'),
                },
                '$' => {
                    let saved = self.chars.clone();
                    match self.dollar() {
                        Some(node) => {
                            flush(&mut text, &mut nodes);
                            nodes.push(node);
                        }
                        None => {
                            self.chars = saved;
                            text.push('$');
                            // Treat a failed braced construct as one literal;
                            // its nested capture formats must not become stops.
                            if self.chars.peek() == Some(&'{') {
                                let mut depth = 0usize;
                                while let Some(c) = self.chars.next() {
                                    text.push(c);
                                    if c == '\\' {
                                        if let Some(next) = self.chars.next() {
                                            text.push(next);
                                        }
                                    } else if c == '{' {
                                        depth += 1;
                                    } else if c == '}' {
                                        depth = depth.saturating_sub(1);
                                        if depth == 0 {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                other => text.push(other),
            }
        }
        flush(&mut text, &mut nodes);
        nodes
    }

    /// What follows a `$`, or `None` when it is a plain dollar sign.
    fn dollar(&mut self) -> Option<Node> {
        match *self.chars.peek()? {
            c if c.is_ascii_digit() => Some(Node::Stop {
                index: self.number()?,
                children: Vec::new(),
            }),
            c if c.is_ascii_alphabetic() || c == '_' => Some(Node::Variable {
                name: self.name(),
                children: Vec::new(),
            }),
            '{' => {
                self.chars.next();
                self.braced()
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Option<u32> {
        let mut digits = String::new();
        while let Some(&c) = self.chars.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            digits.push(c);
            self.chars.next();
        }
        digits.parse().ok()
    }

    fn name(&mut self) -> String {
        let mut name = String::new();
        while let Some(&c) = self.chars.peek() {
            if !(c.is_ascii_alphanumeric() || c == '_') {
                break;
            }
            name.push(c);
            self.chars.next();
        }
        name
    }

    /// After `${`.
    fn braced(&mut self) -> Option<Node> {
        let first = *self.chars.peek()?;
        if first.is_ascii_digit() {
            let index = self.number()?;
            return match self.chars.next()? {
                '}' => Some(Node::Stop {
                    index,
                    children: Vec::new(),
                }),
                ':' => {
                    let children = self.children();
                    self.chars.next(); // the closing brace, if the input had one
                    Some(Node::Stop { index, children })
                }
                '/' => Some(Node::Transform {
                    index,
                    transform: self.transform()?,
                }),
                '|' => {
                    let mut options = vec![String::new()];
                    loop {
                        match self.chars.next()? {
                            '\\' => {
                                if let Some(&escaped) = self.chars.peek()
                                    && ",|\\".contains(escaped)
                                {
                                    self.chars.next();
                                    options.last_mut()?.push(escaped);
                                } else {
                                    options.last_mut()?.push('\\');
                                }
                            }
                            ',' => options.push(String::new()),
                            '|' => break,
                            other => options.last_mut()?.push(other),
                        }
                    }
                    if self.chars.next()? != '}' {
                        return None;
                    }
                    Some(Node::Choice { index, options })
                }
                _ => None,
            };
        }
        if first.is_ascii_alphabetic() || first == '_' {
            let name = self.name();
            return match self.chars.peek() {
                Some('}') => {
                    self.chars.next();
                    Some(Node::Variable {
                        name,
                        children: Vec::new(),
                    })
                }
                Some(':') => {
                    self.chars.next();
                    let children = self.children();
                    self.chars.next();
                    Some(Node::Variable { name, children })
                }
                Some('/') => {
                    self.chars.next();
                    Some(Node::VariableTransform {
                        name,
                        transform: self.transform()?,
                    })
                }
                _ => None,
            };
        }
        None
    }
}

struct Builder<'a> {
    text: String,
    stops: Vec<TabStop>,
    defaults: std::collections::HashMap<u32, String>,
    resolve: &'a dyn Fn(&str) -> Option<String>,
    unknown: std::collections::BTreeMap<String, u32>,
    used: std::collections::BTreeSet<u32>,
    next_unknown: u32,
    order: usize,
}

impl Builder<'_> {
    fn emit(&mut self, nodes: &[Node]) {
        for node in nodes {
            match node {
                Node::Text(text) => self.text.push_str(text),
                Node::Stop { index, children } => {
                    let start = self.text.len();
                    if children.is_empty() {
                        // A mirror, or a bare stop: show the first placeholder's text.
                        if let Some(default) = self.defaults.get(index) {
                            self.text.push_str(&default.clone());
                        }
                    } else {
                        self.emit(children);
                    }
                    self.stop(*index, start..self.text.len(), Vec::new(), None);
                }
                Node::Choice { index, options } => {
                    let start = self.text.len();
                    let first = options.first().cloned().unwrap_or_default();
                    self.text.push_str(&first);
                    self.stop(*index, start..self.text.len(), options.clone(), None);
                }
                Node::Variable { name, children } => match (self.resolve)(name) {
                    Some(value) => self.text.push_str(&value),
                    None if children.is_empty() && !known_variable(name) => {
                        self.unknown(name, None)
                    }
                    None => self.emit(children),
                },
                Node::Transform { index, transform } => {
                    let start = self.text.len();
                    if let Some(value) = self.defaults.get(index) {
                        self.text.push_str(value);
                    }
                    self.stop(
                        *index,
                        start..self.text.len(),
                        Vec::new(),
                        Some(transform.clone()),
                    );
                }
                Node::VariableTransform { name, transform } => match (self.resolve)(name) {
                    Some(value) => self.text.push_str(&transform.apply(&value)),
                    None if known_variable(name) => self.text.push_str(&transform.apply("")),
                    None => self.unknown(name, Some(transform.clone())),
                },
            }
        }
    }

    fn unknown(&mut self, name: &str, transform: Option<Transform>) {
        let index = if let Some(index) = self.unknown.get(name) {
            *index
        } else {
            while self.next_unknown == 0 || self.used.contains(&self.next_unknown) {
                self.next_unknown = self.next_unknown.checked_add(1).unwrap_or(1);
            }
            let index = self.next_unknown;
            self.used.insert(index);
            self.unknown.insert(name.to_owned(), index);
            index
        };
        let start = self.text.len();
        self.text.push_str(name);
        self.stop(index, start..self.text.len(), Vec::new(), transform);
    }

    fn stop(
        &mut self,
        index: u32,
        range: Range<usize>,
        choices: Vec<String>,
        transform: Option<Transform>,
    ) {
        let order = self.order;
        self.order += 1;
        match self.stops.iter_mut().find(|stop| stop.index == index) {
            Some(stop) => {
                let at = if transform.is_none() && stop.transforms[0].is_some() {
                    0
                } else {
                    stop.ranges.len()
                };
                stop.ranges.insert(at, range);
                stop.transforms.insert(at, transform);
                stop.orders.insert(at, order);
                if stop.choices.is_empty() {
                    stop.choices = choices;
                }
            }
            None => self.stops.push(TabStop {
                index,
                ranges: vec![range],
                choices,
                transforms: vec![transform],
                orders: vec![order],
            }),
        }
    }
}

/// The first placeholder text of each tab stop, so a bare `$1` elsewhere can
/// mirror it, wherever it appears.
fn collect_defaults(
    nodes: &[Node],
    defaults: &mut std::collections::HashMap<u32, String>,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> bool {
    let mut definitions = std::collections::HashMap::new();
    fn gather(nodes: &[Node], out: &mut std::collections::HashMap<u32, Node>) {
        for node in nodes {
            match node {
                node @ (Node::Stop { index, .. } | Node::Transform { index, .. }) => {
                    let children = match node {
                        Node::Stop { children, .. } => children.as_slice(),
                        _ => &[],
                    };
                    if !children.is_empty() {
                        out.entry(*index).or_insert_with(|| node.clone());
                    }
                    gather(children, out);
                }
                Node::Choice { index, .. } => {
                    out.entry(*index).or_insert_with(|| node.clone());
                }
                Node::Variable { children, .. } => gather(children, out),
                _ => {}
            }
        }
    }
    fn expand(
        nodes: &[Node],
        definitions: &std::collections::HashMap<u32, Node>,
        resolve: &dyn Fn(&str) -> Option<String>,
        seen: &mut std::collections::HashSet<u32>,
        out: &mut String,
        budget: &mut usize,
    ) {
        for node in nodes {
            if *budget == 0 || out.len() > 64 * 1024 {
                return;
            }
            *budget -= 1;
            match node {
                Node::Text(t) => out.push_str(t),
                node @ (Node::Stop { index, .. } | Node::Transform { index, .. }) => {
                    let children = match node {
                        Node::Stop { children, .. } => children.as_slice(),
                        _ => &[],
                    };
                    if seen.len() >= 64 || !seen.insert(*index) {
                        continue;
                    }
                    if !children.is_empty() {
                        expand(children, definitions, resolve, seen, out, budget);
                    } else if let Some(node) = definitions.get(index) {
                        match node {
                            Node::Stop { children, .. } => {
                                expand(children, definitions, resolve, seen, out, budget)
                            }
                            Node::Choice { options, .. } => {
                                out.push_str(options.first().map_or("", String::as_str))
                            }
                            _ => {}
                        }
                    }
                    seen.remove(index);
                }
                Node::Choice { options, .. } => {
                    out.push_str(options.first().map_or("", String::as_str))
                }
                Node::Variable { name, children } => {
                    if let Some(value) = resolve(name) {
                        out.push_str(&value);
                    } else if children.is_empty() && !known_variable(name) {
                        out.push_str(name);
                    } else {
                        expand(children, definitions, resolve, seen, out, budget);
                    }
                }
                Node::VariableTransform { name, transform } => {
                    if let Some(value) = resolve(name) {
                        out.push_str(&transform.apply(&value));
                    } else if known_variable(name) {
                        out.push_str(&transform.apply(""));
                    } else {
                        out.push_str(name);
                    }
                }
            }
        }
    }
    gather(nodes, &mut definitions);
    let mut budget = 65_536;
    let mut bytes = 0;
    for (index, node) in &definitions {
        let mut text = String::new();
        expand(
            std::slice::from_ref(node),
            &definitions,
            resolve,
            &mut Default::default(),
            &mut text,
            &mut budget,
        );
        bytes += text.len();
        if budget == 0 || text.len() > 64 * 1024 || bytes > 2 * 1024 * 1024 {
            return false;
        }
        defaults.insert(*index, text);
    }
    true
}

fn known_variable(name: &str) -> bool {
    matches!(
        name,
        "TM_SELECTED_TEXT"
            | "TM_CURRENT_LINE"
            | "TM_CURRENT_WORD"
            | "TM_LINE_INDEX"
            | "TM_LINE_NUMBER"
            | "TM_FILENAME"
            | "TM_FILENAME_BASE"
            | "TM_DIRECTORY"
            | "TM_FILEPATH"
            | "RELATIVE_FILEPATH"
            | "CLIPBOARD"
            | "WORKSPACE_NAME"
            | "WORKSPACE_FOLDER"
            | "CURSOR_INDEX"
            | "CURSOR_NUMBER"
            | "CURRENT_YEAR"
            | "CURRENT_YEAR_SHORT"
            | "CURRENT_MONTH"
            | "CURRENT_MONTH_NAME"
            | "CURRENT_MONTH_NAME_SHORT"
            | "CURRENT_DATE"
            | "CURRENT_DAY_NAME"
            | "CURRENT_DAY_NAME_SHORT"
            | "CURRENT_HOUR"
            | "CURRENT_MINUTE"
            | "CURRENT_SECOND"
            | "CURRENT_SECONDS_UNIX"
            | "CURRENT_TIMEZONE_OFFSET"
            | "RANDOM"
            | "RANDOM_HEX"
            | "UUID"
            | "BLOCK_COMMENT_START"
            | "BLOCK_COMMENT_END"
            | "LINE_COMMENT"
    )
}

fn tab_indices(nodes: &[Node], used: &mut std::collections::BTreeSet<u32>) {
    for node in nodes {
        match node {
            Node::Stop { index, children } => {
                used.insert(*index);
                tab_indices(children, used);
            }
            Node::Transform { index, .. } | Node::Choice { index, .. } => {
                used.insert(*index);
            }
            Node::Variable { children, .. } => tab_indices(children, used),
            _ => {}
        }
    }
}

/// Parses `source`. Unset known variables use their default or the empty string;
/// unknown bare variables become editable placeholders after explicit stops.
pub fn parse(source: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Snippet {
    let mut parser = Parser {
        chars: source.chars().peekable(),
        depth: 0,
        transforms: 0,
    };
    let nodes = parser.nodes(None);
    let mut defaults = std::collections::HashMap::new();
    if parser.transforms > 128 || !collect_defaults(&nodes, &mut defaults, resolve) {
        // Degenerate mirror graphs must not expand exponentially on the UI
        // thread. Preserve the server's source literally if expansion exceeds
        // its work or memory budget.
        return Snippet {
            text: source.to_owned(),
            stops: vec![TabStop {
                index: 0,
                ranges: vec![source.len()..source.len()],
                choices: Vec::new(),
                transforms: vec![None],
                orders: vec![usize::MAX],
            }],
        };
    }
    let mut used = std::collections::BTreeSet::from([0]);
    tab_indices(&nodes, &mut used);
    let next_unknown = used
        .last()
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .unwrap_or(1);
    let mut builder = Builder {
        text: String::new(),
        stops: Vec::new(),
        defaults,
        resolve,
        unknown: Default::default(),
        used,
        next_unknown,
        order: 0,
    };
    builder.emit(&nodes);
    let Builder {
        text, mut stops, ..
    } = builder;
    // Mirrors that came before their stop's text was known are filled in by the
    // defaults; ranges are otherwise exact. Visiting order puts 0 last.
    stops.sort_by_key(|stop| (stop.index == 0, stop.index));
    if stops.last().is_none_or(|stop| stop.index != 0) {
        stops.push(TabStop {
            index: 0,
            ranges: vec![text.len()..text.len()],
            choices: Vec::new(),
            transforms: vec![None],
            orders: vec![usize::MAX],
        });
    }
    Snippet { text, stops }
}

/// Indents every line after the first by `indent`, so a snippet inserted in an
/// indented place lines up; the stops move with the text.
pub fn indent_lines(snippet: &mut Snippet, indent: &str) {
    if indent.is_empty() {
        return;
    }
    let shift = |at: usize, text: &str| at + indent.len() * text[..at].matches('\n').count();
    let old = snippet.text.clone();
    for stop in &mut snippet.stops {
        for range in &mut stop.ranges {
            // A stop that starts right after a break starts after the indent too.
            let start = shift(range.start, &old);
            let end = shift(range.end, &old);
            *range = start..end.max(start);
        }
        for choice in &mut stop.choices {
            *choice = choice.replace('\n', &format!("\n{indent}"));
        }
        for transform in stop.transforms.iter_mut().flatten() {
            transform.indent = format!("{indent}{}", transform.indent);
        }
    }
    snippet.text = old.replace('\n', &format!("\n{indent}"));
}

/// The variables every editor offers that Loog can answer for a document.
pub fn document_variables(
    path: &std::path::Path,
    selected: &str,
) -> impl Fn(&str) -> Option<String> + use<> {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let stem = path
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned());
    let directory = path.parent().map(|dir| dir.to_string_lossy().into_owned());
    let full = path.to_string_lossy().into_owned();
    let selected = selected.to_owned();
    move |name| match name {
        "TM_FILENAME" => file.clone(),
        "TM_FILENAME_BASE" => stem.clone(),
        "TM_DIRECTORY" => directory.clone(),
        "TM_FILEPATH" => Some(full.clone()),
        "TM_SELECTED_TEXT" => Some(selected.clone()),
        "TM_CURRENT_LINE" | "TM_LINE_INDEX" | "TM_LINE_NUMBER" => None,
        "CLIPBOARD" => None,
        _ => None,
    }
}

/// Document variables evaluated at the insertion cursor, before any completion
/// edits change the line or selected text.
pub fn insertion_variables<'a>(
    path: &std::path::Path,
    selected: &str,
    text: &'a str,
    cursor: usize,
) -> impl Fn(&str) -> Option<String> + 'a {
    let document = document_variables(path, selected);
    insertion_context(document, text, cursor)
}

/// Opaque resources supply a display name, never a native path. Undefined path
/// variables use the snippet's own fallback until provider path metadata exists.
pub fn resource_insertion_variables<'a>(
    resource: &loog_workspace_api::ResourceRef,
    name: &str,
    selected: &str,
    text: &'a str,
    cursor: usize,
) -> impl Fn(&str) -> Option<String> + 'a {
    let local = resource
        .local_path()
        .map(|path| document_variables(path, selected));
    let name = name.to_owned();
    let selected = selected.to_owned();
    insertion_context(
        move |variable| {
            if let Some(local) = &local {
                return local(variable);
            }
            match variable {
                "TM_FILENAME" => Some(name.clone()),
                "TM_SELECTED_TEXT" => Some(selected.clone()),
                _ => None,
            }
        },
        text,
        cursor,
    )
}
fn insertion_context<'a>(
    document: impl Fn(&str) -> Option<String> + 'a,
    text: &'a str,
    cursor: usize,
) -> impl Fn(&str) -> Option<String> + 'a {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let start = text[..cursor].rfind('\n').map_or(0, |at| at + 1);
    let end = text[cursor..]
        .find('\n')
        .map_or(text.len(), |at| cursor + at);
    let line = text[..start].bytes().filter(|c| *c == b'\n').count();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let word_start = text[start..cursor]
        .char_indices()
        .rev()
        .find(|(_, c)| !word(*c))
        .map_or(start, |(at, c)| start + at + c.len_utf8());
    let word_end = text[cursor..end]
        .char_indices()
        .find(|(_, c)| !word(*c))
        .map_or(end, |(at, _)| cursor + at);
    move |name| match name {
        "TM_CURRENT_LINE" => Some(text[start..end].trim_end_matches('\r').to_owned()),
        "TM_CURRENT_WORD" => Some(text[word_start..word_end].to_owned()),
        "TM_LINE_INDEX" => Some(line.to_string()),
        "TM_LINE_NUMBER" => Some((line + 1).to_string()),
        _ => document(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provided_snippets_keep_text_variables_without_guessing_native_path_parts() {
        let resource = loog_workspace_api::ResourceRef::provided(
            loog_workspace_api::EnvironmentKey::new("provider", "target").unwrap(),
            "opaque:back\\slash",
        )
        .unwrap();
        let variables =
            resource_insertion_variables(&resource, "a\\中.rs", "选中", "let 中 = 1;", 6);
        assert_eq!(variables("TM_FILENAME"), Some("a\\中.rs".into()));
        assert_eq!(variables("TM_CURRENT_WORD"), Some("中".into()));
        assert_eq!(variables("TM_SELECTED_TEXT"), Some("选中".into()));
        assert_eq!(variables("TM_DIRECTORY"), None);
        assert_eq!(variables("TM_FILEPATH"), None);
    }

    fn none(_: &str) -> Option<String> {
        None
    }

    fn snippet(source: &str) -> Snippet {
        parse(source, &none)
    }

    fn ranges(snippet: &Snippet) -> Vec<(u32, Vec<Range<usize>>)> {
        snippet
            .stops
            .iter()
            .map(|s| (s.index, s.ranges.clone()))
            .collect()
    }

    #[test]
    fn plain_text_has_only_the_final_stop() {
        let s = snippet("hello");
        assert_eq!(s.text, "hello");
        assert_eq!(ranges(&s), [(0, vec![5..5])]);
    }

    #[test]
    fn tab_stops_are_visited_in_order_and_zero_comes_last() {
        let s = snippet("fn $1($2) {\n\t$0\n}");
        assert_eq!(s.text, "fn () {\n\t\n}");
        assert_eq!(
            ranges(&s),
            [(1, vec![3..3]), (2, vec![4..4]), (0, vec![9..9])]
        );
    }

    #[test]
    fn a_placeholder_inserts_its_default_and_selects_it() {
        let s = snippet("let ${1:name} = ${2:value};$0");
        assert_eq!(s.text, "let name = value;");
        assert_eq!(&s.text[s.stops[0].ranges[0].clone()], "name");
        assert_eq!(&s.text[s.stops[1].ranges[0].clone()], "value");
    }

    #[test]
    fn stops_with_the_same_index_mirror_each_other() {
        let s = snippet("${1:x} + $1 = ${1}");
        assert_eq!(s.text, "x + x = x");
        assert_eq!(s.stops[0].ranges, [0..1, 4..5, 8..9]);
        // The mirror may come first.
        let s = snippet("$1 and ${1:y}");
        assert_eq!(s.text, "y and y");
        assert_eq!(s.stops[0].ranges, [0..1, 6..7]);
    }

    #[test]
    fn placeholders_nest() {
        let s = snippet("${1:a ${2:b} c}$0");
        assert_eq!(s.text, "a b c");
        assert_eq!(s.stops[0].ranges, [0..5]);
        assert_eq!(s.stops[1].ranges, [2..3]);
    }

    #[test]
    fn nested_defaults_resolve_forward_mirrors_and_cycles() {
        let s = snippet("$1 ${1:$2} ${2:名字} $1");
        assert_eq!(s.text, "名字 名字 名字 名字");
        for stop in &s.stops[..2] {
            for range in &stop.ranges {
                assert_eq!(&s.text[range.clone()], "名字");
            }
        }
        let s = snippet("${1:$2} ${2:$1} $1");
        assert_eq!(s.text, "  ");
    }

    #[test]
    fn excessive_placeholder_nesting_is_bounded_and_retains_literal_text() {
        let source = format!("{}end{}", "${1:".repeat(2048), "}".repeat(2048));
        let s = snippet(&source);
        assert!(s.text.contains("end"));
        assert!(s.text.contains("${1:"));
        assert!(s.text.len() <= source.len());
        assert!(
            s.stops
                .iter()
                .flat_map(|s| &s.ranges)
                .all(|r| r.end <= s.text.len())
        );
    }

    #[test]
    fn exponential_forward_mirrors_fall_back_to_literal_source_with_bounded_work() {
        for leaf in ["x", ""] {
            let mut source = String::new();
            for n in 1..50 {
                source.push_str(&format!("${{{n}:${}${}}}", n + 1, n + 1));
            }
            source.push_str(&format!("${{50:{leaf}}}"));
            let parsed = snippet(&source);
            assert_eq!(parsed.text, source);
            assert_eq!(parsed.stops.len(), 1);
            assert_eq!(parsed.stops[0].index, 0);
        }
    }

    #[test]
    fn a_choice_offers_its_options_and_starts_on_the_first() {
        let s = snippet("${1|one,two,three|} end");
        assert_eq!(s.text, "one end");
        assert_eq!(s.stops[0].choices, ["one", "two", "three"]);
        let escaped = snippet(r"${1|a\,b,c|}");
        assert_eq!(escaped.stops[0].choices, ["a,b", "c"]);
        let forward = snippet("$1 ${1|one,two|}");
        assert_eq!(
            forward.stops[0].choices,
            ["one", "two"],
            "a preceding mirror retains its definition's choices"
        );
    }

    #[test]
    fn variables_resolve_or_fall_back_to_their_default_or_name() {
        let known = |name: &str| (name == "TM_FILENAME").then(|| "main.rs".to_owned());
        let s = parse(
            "// $TM_FILENAME ${TM_FILENAME} ${UNKNOWN:fallback} $OTHER ${X}",
            &known,
        );
        assert_eq!(s.text, "// main.rs main.rs fallback OTHER X");
        // Variable transforms are evaluated at insertion time.
        let s = parse("${TM_FILENAME/(.*)\\.rs/$1/}", &known);
        assert_eq!(s.text, "main");
    }

    #[test]
    fn variable_transforms_evaluate_inside_forward_and_nested_defaults() {
        let known = |name: &str| (name == "TM_FILENAME").then(|| "my_file.rs".to_owned());
        let s = parse("$1 ${1:${TM_FILENAME/(.*)\\.rs/${1:/pascalcase}/}}", &known);
        assert_eq!(s.text, "MyFile MyFile");
        assert_eq!(parse("${CLIPBOARD/^.+$/${0:-empty}/}", &none).text, "empty");
    }

    #[test]
    fn transforms_before_the_plain_placeholder_keep_the_plain_occurrence_primary() {
        let s = snippet("${1/(.*)/${1:/upcase}/} ${1:é}");
        assert_eq!(s.text, "é é");
        assert_eq!(s.stops[0].ranges, [3..5, 0..2]);
        assert!(s.stops[0].transforms[0].is_none());
        assert!(s.stops[0].transforms[1].is_some());
        assert!(s.stops[0].orders[0] > s.stops[0].orders[1]);
    }

    #[test]
    fn unknown_variables_are_distinct_editable_stops_with_same_name_mirrors() {
        let s = snippet("${2:value} $MISSING/$MISSING $OTHER ${CLIPBOARD:fallback} $CLIPBOARD");
        assert_eq!(s.text, "value MISSING/MISSING OTHER fallback ");
        assert_eq!(
            s.stops.iter().map(|s| s.index).collect::<Vec<_>>(),
            [2, 3, 4, 0]
        );
        assert_eq!(s.stops[1].ranges.len(), 2);
        let s = snippet("${4294967295:high} $UNKNOWN");
        assert_eq!(
            s.stops.iter().map(|s| s.index).collect::<Vec<_>>(),
            [1, u32::MAX, 0]
        );
        let s = snippet("${MISSING/(.*)/${1:/downcase}/} $MISSING");
        assert_eq!(s.stops[0].ranges, [8..15, 0..7]);
        assert!(s.stops[0].transforms[0].is_none());
    }

    #[test]
    fn malformed_constructs_and_choice_escapes_do_not_consume_literal_text() {
        for source in [
            "${1/a/b",
            "${X/a/b}",
            "${4294967296}",
            "${1?x}",
            "$变量",
            "${NAME/a/${1:/upcase}/",
            "${1|a|tail",
        ] {
            assert_eq!(snippet(source).text, source);
        }
        let s = snippet(r"${1|a\q,a\,b,c\|d,\\|}");
        assert_eq!(s.stops[0].choices, [r"a\q", "a,b", "c|d", r"\"]);
    }

    #[test]
    fn too_many_compiled_transforms_preserve_the_original_source() {
        let source = "${TM_FILENAME/(.*)/$1/}".repeat(129);
        let parsed = parse(&source, &|_| Some("name".into()));
        assert_eq!(parsed.text, source);
        assert_eq!(parsed.stops.len(), 1);
        assert_eq!(parsed.stops[0].index, 0);
    }

    #[test]
    fn escapes_and_stray_dollars_are_literal() {
        assert_eq!(snippet(r"cost: \$5 \\ \}").text, r"cost: $5 \ }");
        assert_eq!(snippet("a $ b $").text, "a $ b $");
        assert_eq!(snippet(r"\n stays").text, r"\n stays");
    }

    #[test]
    fn unterminated_constructs_do_not_panic() {
        for source in [
            "${", "${1", "${1:abc", "${1|a,b", "${A:", "$", "${1:${2:", "}",
        ] {
            let _ = snippet(source);
        }
        assert_eq!(snippet("${1:abc").text, "abc");
    }

    #[test]
    fn unicode_text_keeps_byte_ranges_correct() {
        let s = snippet("变量 ${1:名字} é");
        assert_eq!(&s.text[s.stops[0].ranges[0].clone()], "名字");
    }

    #[test]
    fn insertion_variables_use_original_unicode_line_word_and_selection() {
        let text = "first\r\nlet 名字 = 1;\r\n";
        let resolve = insertion_variables(std::path::Path::new("main.rs"), "选择", text, 13);
        let s = parse(
            "$TM_FILENAME:$TM_LINE_INDEX:$TM_LINE_NUMBER:$TM_CURRENT_WORD:$TM_CURRENT_LINE:$TM_SELECTED_TEXT",
            &resolve,
        );
        assert_eq!(s.text, "main.rs:1:2:名字:let 名字 = 1;:选择");
    }

    #[test]
    fn indenting_moves_the_stops_with_their_text() {
        let mut s = snippet("if $1 {\n\t$0\n}");
        indent_lines(&mut s, "  ");
        assert_eq!(s.text, "if  {\n  \t\n  }");
        assert_eq!(s.stops[0].ranges[0], 3..3);
        // The final stop sits after the new indent and the tab.
        assert_eq!(s.stops[1].ranges[0], 9..9);
        assert_eq!(&s.text[..9], "if  {\n  \t");
        let mut flat = snippet("one");
        indent_lines(&mut flat, "    ");
        assert_eq!(flat.text, "one");
        let mut choice = snippet("a\n${1|b,c\nd|}\n$0");
        indent_lines(&mut choice, "  ");
        assert_eq!(choice.text, "a\n  b\n  ");
        assert_eq!(choice.stops[0].ranges[0], 4..5);
        assert_eq!(choice.stops[0].choices, ["b", "c\n  d"]);
        assert_eq!(choice.stops[1].ranges[0], 8..8);
        let mut initially_flat = snippet("${1|a,b\nc|}");
        indent_lines(&mut initially_flat, "  ");
        assert_eq!(initially_flat.stops[0].choices, ["a", "b\n  c"]);
    }

    #[test]
    fn document_variables_come_from_the_path() {
        let resolve = document_variables(std::path::Path::new("/w/src/lib.rs"), "sel");
        assert_eq!(resolve("TM_FILENAME").as_deref(), Some("lib.rs"));
        assert_eq!(resolve("TM_FILENAME_BASE").as_deref(), Some("lib"));
        assert_eq!(resolve("TM_SELECTED_TEXT").as_deref(), Some("sel"));
        assert_eq!(resolve("NOPE"), None);
    }
}

/// Normalize expanded text and remap tab stops after indentation and variables.
pub fn normalize_endings(snippet: &mut Snippet, ending: &str) {
    let text = &snippet.text;
    if !text.contains(['\r', '\n']) {
        return;
    }
    let mapped = |at: usize| loog_editor_core::text::normalize_endings(&text[..at], ending).len();
    for stop in &mut snippet.stops {
        for range in &mut stop.ranges {
            *range = mapped(range.start)..mapped(range.end);
        }
        for choice in &mut stop.choices {
            *choice = loog_editor_core::text::normalize_endings(choice, ending);
        }
    }
    snippet.text = loog_editor_core::text::normalize_endings(text, ending);
}

#[cfg(test)]
mod ending_tests {
    use super::*;
    #[test]
    fn normalized_snippet_stops_track_multiline_defaults_and_choices() {
        let mut snippet = parse("fn { \n  \u{24}{1:中\n尾}\n}\u{24}0", &|_| None);
        normalize_endings(&mut snippet, "\r\n");
        assert!(snippet.text.contains("中\r\n尾"));
        let first = snippet.stops.iter().find(|s| s.index == 1).unwrap();
        assert_eq!(&snippet.text[first.ranges[0].clone()], "中\r\n尾");
        assert_eq!(
            snippet.stops.last().unwrap().ranges[0].end,
            snippet.text.len()
        );
    }
}
