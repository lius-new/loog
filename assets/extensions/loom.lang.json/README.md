# JSON Language

Syntax highlighting for JSON, as a Tree-sitter grammar (WebAssembly) and a
highlight query. It ships with Loom.

You can change how JSON is coloured without touching this extension: put a
`highlights.scm` for the `json` grammar in your own queries folder. Start the file
with `; extends` to add rules to the ones here; without it, your file replaces them.
Use **Inspect Highlight at Cursor** to see which rule colours a place.
