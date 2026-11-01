# Markdown Language

Syntax highlighting for Markdown (block and inline grammars, WebAssembly) and
the injection query that colours fenced code blocks in their own language. It
ships with Loom.

To change the colours, put a `highlights.scm` for the `markdown` or
`markdown_inline` grammar in your own queries folder; start it with `; extends` to
add to the rules here. Use **Inspect Highlight at Cursor** to see which rule
colours a place.
