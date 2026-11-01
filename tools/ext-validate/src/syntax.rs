// Use the production syntax engine to check extension resources.
include!("../../../src/features/syntax/mod.rs");

pub(crate) fn validate_producer_grammar(
    resources: &dyn Resources,
    name: &str,
    parser: &mut tree_sitter::Parser,
    text: &str,
) -> Result<usize, String> {
    use streaming_iterator::StreamingIterator;
    let query = queries::compile(resources, name)?;
    injections::compile(resources, name)?;
    let tree = parser.parse(text, None).ok_or("parse cancelled")?;
    if tree.root_node().has_error() {
        return Err(format!(
            "{name}: validation sample has parse errors: {}",
            tree.root_node().to_sexp()
        ));
    }
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut captures = cursor.captures(&query.query, tree.root_node(), text.as_bytes());
    let mut count = 0;
    while let Some((matched, index)) = captures.next() {
        if query.class_of(matched.captures()[*index].index).is_some() {
            count += 1;
        }
    }
    if count == 0 {
        return Err(format!(
            "{name}: validation sample has no highlight captures"
        ));
    }
    Ok(count)
}
