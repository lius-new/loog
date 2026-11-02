// Metadata-only fixtures for domain/protocol tests. No parser or query payload.
pub(crate) static LANGUAGE_FILES: &[(&str,&str)] = &[
    ("go", r#"{"name":"go","display_name":"Go","badge":"GO","aliases":["golang"],"comments":{"line":"//","block":["/*","*/"]},"variants":[{"id":"go"}],"files":[{"extensions":["go"],"variant":"go"}]}"#),
    ("csharp", r#"{"name":"csharp","display_name":"C#","badge":"C#","aliases":["cs"],"comments":{"line":"//","block":["/*","*/"]},"variants":[{"id":"csharp","grammar":"c_sharp"}],"files":[{"extensions":["cs"],"variant":"csharp"}]}"#),
    ("css", r#"{"name":"css","display_name":"CSS","badge":"CSS","comments":{"block":["/*","*/"]},"variants":[{"id":"css","grammar":"css"}],"files":[{"extensions":["css"],"variant":"css"}]}"#),
    ("html", r#"{"name":"html","display_name":"HTML","badge":"HTML","aliases":["htm"],"comments":{"block":["<!--","-->"]},"variants":[{"id":"html","grammar":"html"}],"files":[{"extensions":["html","htm","xhtml"],"variant":"html"}]}"#),
    ("javascript", r#"{"name":"javascript","display_name":"JavaScript","badge":"JS","aliases":["js","jsx"],"comments":{"line":"//","block":["/*","*/"]},"variants":[{"id":"javascript","grammar":"javascript"}],"files":[{"extensions":["js","jsx","mjs","cjs"],"variant":"javascript"}]}"#),
    ("json", r#"{"name":"json","display_name":"JSON","badge":"JSON","aliases":["jsonc"],"comments":{"line":"//","block":["/*","*/"]},"variants":[{"id":"json","grammar":"json"}],"files":[{"extensions":["json","jsonc","geojson","webmanifest"],"variant":"json"}]}"#),
    ("markdown", r#"{"name":"markdown","display_name":"Markdown","badge":"MD","aliases":["md"],"comments":{"block":["<!--","-->"]},"variants":[{"id":"markdown","grammar":"markdown"}],"files":[{"extensions":["md","markdown","mdown","mkd"],"variant":"markdown"}]}"#),
    ("plaintext", r#"{"name":"plaintext","display_name":"Plain Text","badge":"TXT","variants":[{"id":"plaintext"}]}"#),
    ("rust", r#"{"name":"rust","display_name":"Rust","badge":"RS","aliases":["rs"],"comments":{"line":"//","block":["/*","*/"]},"variants":[{"id":"rust","grammar":"rust"}],"files":[{"extensions":["rs"],"variant":"rust"}]}"#),
    ("typescript", r#"{"name":"typescript","display_name":"TypeScript","badge":"TS","aliases":["ts"],"comments":{"line":"//","block":["/*","*/"]},"variants":[{"id":"typescript","grammar":"typescript"},{"id":"tsx","grammar":"tsx"}],"files":[{"extensions":["ts","mts","cts"],"variant":"typescript"},{"extensions":["tsx"],"variant":"tsx"}]}"#),
];
pub(crate) static GRAMMAR_NAMES: &[&str] = &["c_sharp","css","html","javascript","json","markdown","markdown_inline","rust","tsx","typescript"];
