//! File icon theme support for the workspace tree.
//!
//! The bundled SVGs are a focused subset of Material Icon Theme. Matching is
//! intentionally kept in Loog so the framework's generic icon registry
//! stays independent from editor-specific file type policy.

use lgui::icons::SvgIconRegistry;

pub const DEFAULT_FILE_ICON: &str = "file-type-document";
pub const FOLDER_ICON: &str = "folder";
pub const FOLDER_OPEN_ICON: &str = "folder-open";

const RUST: &str = "file-type-rust";
const GO: &str = "file-type-go";
const JAVASCRIPT: &str = "file-type-javascript";
const TYPESCRIPT: &str = "file-type-typescript";
const REACT: &str = "file-type-react";
const HTML: &str = "file-type-html";
const CSS: &str = "file-type-css";
const SASS: &str = "file-type-sass";
const VUE: &str = "file-type-vue";
const SVELTE: &str = "file-type-svelte";
const JSON: &str = "file-type-json";
const YAML: &str = "file-type-yaml";
const TOML: &str = "file-type-toml";
const XML: &str = "file-type-xml";
const MARKDOWN: &str = "file-type-markdown";
const PYTHON: &str = "file-type-python";
const JAVA: &str = "file-type-java";
const C: &str = "file-type-c";
const CPP: &str = "file-type-cpp";
const CSHARP: &str = "file-type-csharp";
const PHP: &str = "file-type-php";
const RUBY: &str = "file-type-ruby";
const POWERSHELL: &str = "file-type-powershell";
const CONSOLE: &str = "file-type-console";
const DOCKER: &str = "file-type-docker";
const GIT: &str = "file-type-git";
const DATABASE: &str = "file-type-database";
const IMAGE: &str = "file-type-image";
const SVG: &str = "file-type-svg";
const AUDIO: &str = "file-type-audio";
const VIDEO: &str = "file-type-video";
const ARCHIVE: &str = "file-type-archive";
const PDF: &str = "file-type-pdf";
const WORD: &str = "file-type-word";
const FONT: &str = "file-type-font";
const BINARY: &str = "file-type-binary";
const LOCK: &str = "file-type-lock";
const LICENSE: &str = "file-type-license";
const NPM: &str = "file-type-npm";
const PNPM: &str = "file-type-pnpm";
const MAKEFILE: &str = "file-type-makefile";

/// Exact names have priority over suffixes. Entries are lowercase because the
/// requested name is normalized once before lookup.
const EXACT_FILE_ICONS: &[(&str, &str)] = &[
    ("cargo.toml", RUST),
    ("cargo.lock", RUST),
    ("rust-toolchain", RUST),
    ("rust-toolchain.toml", RUST),
    ("go.mod", GO),
    ("go.sum", GO),
    ("go.work", GO),
    ("package.json", NPM),
    ("package-lock.json", NPM),
    ("npm-shrinkwrap.json", NPM),
    ("pnpm-lock.yaml", PNPM),
    ("pnpm-workspace.yaml", PNPM),
    ("tsconfig.json", TYPESCRIPT),
    ("jsconfig.json", JAVASCRIPT),
    ("dockerfile", DOCKER),
    ("containerfile", DOCKER),
    (".dockerignore", DOCKER),
    ("docker-compose.yml", DOCKER),
    ("docker-compose.yaml", DOCKER),
    ("compose.yml", DOCKER),
    ("compose.yaml", DOCKER),
    (".gitignore", GIT),
    (".gitattributes", GIT),
    (".gitmodules", GIT),
    (".gitkeep", GIT),
    ("makefile", MAKEFILE),
    ("gnumakefile", MAKEFILE),
    ("pom.xml", JAVA),
    ("readme", MARKDOWN),
    ("license", LICENSE),
    ("license.md", LICENSE),
    ("license.txt", LICENSE),
    ("copying", LICENSE),
    ("copying.md", LICENSE),
    ("copying.txt", LICENSE),
];

/// Suffixes may contain multiple segments. The resolver selects the longest
/// matching suffix, so a future specialized entry such as `test.ts` wins over
/// the ordinary `ts` entry without depending on table order.
const SUFFIX_FILE_ICONS: &[(&str, &str)] = &[
    ("d.ts", TYPESCRIPT),
    ("d.mts", TYPESCRIPT),
    ("d.cts", TYPESCRIPT),
    ("tar.gz", ARCHIVE),
    ("tar.bz2", ARCHIVE),
    ("tar.xz", ARCHIVE),
    ("js", JAVASCRIPT),
    ("mjs", JAVASCRIPT),
    ("cjs", JAVASCRIPT),
    ("ts", TYPESCRIPT),
    ("mts", TYPESCRIPT),
    ("cts", TYPESCRIPT),
    ("jsx", REACT),
    ("tsx", REACT),
    ("html", HTML),
    ("htm", HTML),
    ("css", CSS),
    ("scss", SASS),
    ("sass", SASS),
    ("vue", VUE),
    ("svelte", SVELTE),
    ("json", JSON),
    ("jsonc", JSON),
    ("json5", JSON),
    ("jsonl", JSON),
    ("ndjson", JSON),
    ("geojson", JSON),
    ("yaml", YAML),
    ("yml", YAML),
    ("toml", TOML),
    ("xml", XML),
    ("xsd", XML),
    ("xsl", XML),
    ("xslt", XML),
    ("plist", XML),
    ("md", MARKDOWN),
    ("markdown", MARKDOWN),
    ("mdx", MARKDOWN),
    ("rst", MARKDOWN),
    ("rs", RUST),
    ("ron", RUST),
    ("go", GO),
    ("py", PYTHON),
    ("pyw", PYTHON),
    ("pyi", PYTHON),
    ("java", JAVA),
    ("jsp", JAVA),
    ("c", C),
    ("h", C),
    ("cpp", CPP),
    ("cc", CPP),
    ("cxx", CPP),
    ("hpp", CPP),
    ("hxx", CPP),
    ("cs", CSHARP),
    ("php", PHP),
    ("phtml", PHP),
    ("rb", RUBY),
    ("ps1", POWERSHELL),
    ("psm1", POWERSHELL),
    ("psd1", POWERSHELL),
    ("sh", CONSOLE),
    ("bash", CONSOLE),
    ("zsh", CONSOLE),
    ("fish", CONSOLE),
    ("bat", CONSOLE),
    ("cmd", CONSOLE),
    ("sql", DATABASE),
    ("sqlite", DATABASE),
    ("sqlite3", DATABASE),
    ("db", DATABASE),
    ("csv", DATABASE),
    ("tsv", DATABASE),
    ("png", IMAGE),
    ("jpg", IMAGE),
    ("jpeg", IMAGE),
    ("gif", IMAGE),
    ("bmp", IMAGE),
    ("webp", IMAGE),
    ("avif", IMAGE),
    ("ico", IMAGE),
    ("svg", SVG),
    ("mp3", AUDIO),
    ("wav", AUDIO),
    ("flac", AUDIO),
    ("ogg", AUDIO),
    ("m4a", AUDIO),
    ("mp4", VIDEO),
    ("mkv", VIDEO),
    ("mov", VIDEO),
    ("avi", VIDEO),
    ("webm", VIDEO),
    ("zip", ARCHIVE),
    ("7z", ARCHIVE),
    ("rar", ARCHIVE),
    ("tar", ARCHIVE),
    ("gz", ARCHIVE),
    ("bz2", ARCHIVE),
    ("xz", ARCHIVE),
    ("tgz", ARCHIVE),
    ("pdf", PDF),
    ("doc", WORD),
    ("docx", WORD),
    ("ttf", FONT),
    ("otf", FONT),
    ("woff", FONT),
    ("woff2", FONT),
    ("wasm", BINARY),
    ("bin", BINARY),
    ("exe", BINARY),
    ("dll", BINARY),
    ("so", BINARY),
    ("dylib", BINARY),
    ("lock", LOCK),
];

pub fn icon_for_file(name: &str) -> &'static str {
    let normalized = name.to_ascii_lowercase();

    if let Some((_, icon)) = EXACT_FILE_ICONS
        .iter()
        .find(|(file_name, _)| *file_name == normalized)
    {
        return icon;
    }

    SUFFIX_FILE_ICONS
        .iter()
        .filter(|(suffix, _)| matches_suffix(&normalized, suffix))
        .max_by_key(|(suffix, _)| suffix.len())
        .map_or(DEFAULT_FILE_ICON, |(_, icon)| icon)
}

fn matches_suffix(name: &str, suffix: &str) -> bool {
    name == suffix
        || name
            .strip_suffix(suffix)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// Registers the Loog file theme in the application-wide SVG registry.
pub fn register(registry: SvgIconRegistry) -> SvgIconRegistry {
    registry
        .with_icon_bytes(FOLDER_ICON, icon("folder.svg"))
        .with_icon_bytes(FOLDER_OPEN_ICON, icon("folder-open.svg"))
        .with_icon_bytes(DEFAULT_FILE_ICON, icon("document.svg"))
        .with_icon_bytes(RUST, icon("rust.svg"))
        .with_icon_bytes(GO, icon("go.svg"))
        .with_icon_bytes(JAVASCRIPT, icon("javascript.svg"))
        .with_icon_bytes(TYPESCRIPT, icon("typescript.svg"))
        .with_icon_bytes(REACT, icon("react.svg"))
        .with_icon_bytes(HTML, icon("html.svg"))
        .with_icon_bytes(CSS, icon("css.svg"))
        .with_icon_bytes(SASS, icon("sass.svg"))
        .with_icon_bytes(VUE, icon("vue.svg"))
        .with_icon_bytes(SVELTE, icon("svelte.svg"))
        .with_icon_bytes(JSON, icon("json.svg"))
        .with_icon_bytes(YAML, icon("yaml.svg"))
        .with_icon_bytes(TOML, icon("toml.svg"))
        .with_icon_bytes(XML, icon("xml.svg"))
        .with_icon_bytes(MARKDOWN, icon("markdown.svg"))
        .with_icon_bytes(PYTHON, icon("python.svg"))
        .with_icon_bytes(JAVA, icon("java.svg"))
        .with_icon_bytes(C, icon("c.svg"))
        .with_icon_bytes(CPP, icon("cpp.svg"))
        .with_icon_bytes(CSHARP, icon("csharp.svg"))
        .with_icon_bytes(PHP, icon("php.svg"))
        .with_icon_bytes(RUBY, icon("ruby.svg"))
        .with_icon_bytes(POWERSHELL, icon("powershell.svg"))
        .with_icon_bytes(CONSOLE, icon("console.svg"))
        .with_icon_bytes(DOCKER, icon("docker.svg"))
        .with_icon_bytes(GIT, icon("git.svg"))
        .with_icon_bytes(DATABASE, icon("database.svg"))
        .with_icon_bytes(IMAGE, icon("image.svg"))
        .with_icon_bytes(SVG, icon("svg.svg"))
        .with_icon_bytes(AUDIO, icon("audio.svg"))
        .with_icon_bytes(VIDEO, icon("video.svg"))
        .with_icon_bytes(ARCHIVE, icon("zip.svg"))
        .with_icon_bytes(PDF, icon("pdf.svg"))
        .with_icon_bytes(WORD, icon("word.svg"))
        .with_icon_bytes(FONT, icon("font.svg"))
        .with_icon_bytes(BINARY, icon("hex.svg"))
        .with_icon_bytes(LOCK, icon("lock.svg"))
        .with_icon_bytes(LICENSE, icon("license.svg"))
        .with_icon_bytes(NPM, icon("npm.svg"))
        .with_icon_bytes(PNPM, icon("pnpm.svg"))
        .with_icon_bytes(MAKEFILE, icon("makefile.svg"))
}

fn icon(name: &str) -> &'static [u8] {
    match name {
        "folder.svg" => include_bytes!("../../assets/icon-themes/material/icons/folder.svg"),
        "folder-open.svg" => {
            include_bytes!("../../assets/icon-themes/material/icons/folder-open.svg")
        }
        "document.svg" => include_bytes!("../../assets/icon-themes/material/icons/document.svg"),
        "rust.svg" => include_bytes!("../../assets/icon-themes/material/icons/rust.svg"),
        "go.svg" => include_bytes!("../../assets/icon-themes/material/icons/go.svg"),
        "javascript.svg" => {
            include_bytes!("../../assets/icon-themes/material/icons/javascript.svg")
        }
        "typescript.svg" => {
            include_bytes!("../../assets/icon-themes/material/icons/typescript.svg")
        }
        "react.svg" => include_bytes!("../../assets/icon-themes/material/icons/react.svg"),
        "html.svg" => include_bytes!("../../assets/icon-themes/material/icons/html.svg"),
        "css.svg" => include_bytes!("../../assets/icon-themes/material/icons/css.svg"),
        "sass.svg" => include_bytes!("../../assets/icon-themes/material/icons/sass.svg"),
        "vue.svg" => include_bytes!("../../assets/icon-themes/material/icons/vue.svg"),
        "svelte.svg" => include_bytes!("../../assets/icon-themes/material/icons/svelte.svg"),
        "json.svg" => include_bytes!("../../assets/icon-themes/material/icons/json.svg"),
        "yaml.svg" => include_bytes!("../../assets/icon-themes/material/icons/yaml.svg"),
        "toml.svg" => include_bytes!("../../assets/icon-themes/material/icons/toml.svg"),
        "xml.svg" => include_bytes!("../../assets/icon-themes/material/icons/xml.svg"),
        "markdown.svg" => include_bytes!("../../assets/icon-themes/material/icons/markdown.svg"),
        "python.svg" => include_bytes!("../../assets/icon-themes/material/icons/python.svg"),
        "java.svg" => include_bytes!("../../assets/icon-themes/material/icons/java.svg"),
        "c.svg" => include_bytes!("../../assets/icon-themes/material/icons/c.svg"),
        "cpp.svg" => include_bytes!("../../assets/icon-themes/material/icons/cpp.svg"),
        "csharp.svg" => include_bytes!("../../assets/icon-themes/material/icons/csharp.svg"),
        "php.svg" => include_bytes!("../../assets/icon-themes/material/icons/php.svg"),
        "ruby.svg" => include_bytes!("../../assets/icon-themes/material/icons/ruby.svg"),
        "powershell.svg" => {
            include_bytes!("../../assets/icon-themes/material/icons/powershell.svg")
        }
        "console.svg" => include_bytes!("../../assets/icon-themes/material/icons/console.svg"),
        "docker.svg" => include_bytes!("../../assets/icon-themes/material/icons/docker.svg"),
        "git.svg" => include_bytes!("../../assets/icon-themes/material/icons/git.svg"),
        "database.svg" => include_bytes!("../../assets/icon-themes/material/icons/database.svg"),
        "image.svg" => include_bytes!("../../assets/icon-themes/material/icons/image.svg"),
        "svg.svg" => include_bytes!("../../assets/icon-themes/material/icons/svg.svg"),
        "audio.svg" => include_bytes!("../../assets/icon-themes/material/icons/audio.svg"),
        "video.svg" => include_bytes!("../../assets/icon-themes/material/icons/video.svg"),
        "zip.svg" => include_bytes!("../../assets/icon-themes/material/icons/zip.svg"),
        "pdf.svg" => include_bytes!("../../assets/icon-themes/material/icons/pdf.svg"),
        "word.svg" => include_bytes!("../../assets/icon-themes/material/icons/word.svg"),
        "font.svg" => include_bytes!("../../assets/icon-themes/material/icons/font.svg"),
        "hex.svg" => include_bytes!("../../assets/icon-themes/material/icons/hex.svg"),
        "lock.svg" => include_bytes!("../../assets/icon-themes/material/icons/lock.svg"),
        "license.svg" => include_bytes!("../../assets/icon-themes/material/icons/license.svg"),
        "npm.svg" => include_bytes!("../../assets/icon-themes/material/icons/npm.svg"),
        "pnpm.svg" => include_bytes!("../../assets/icon-themes/material/icons/pnpm.svg"),
        "makefile.svg" => include_bytes!("../../assets/icon-themes/material/icons/makefile.svg"),
        _ => unreachable!("all file theme assets are registered explicitly"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_name_has_priority_over_extension() {
        assert_eq!(icon_for_file("Cargo.toml"), RUST);
        assert_eq!(icon_for_file("package.json"), NPM);
        assert_eq!(icon_for_file("Dockerfile"), DOCKER);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(icon_for_file("PHOTO.JPEG"), IMAGE);
        assert_eq!(icon_for_file("Component.TSX"), REACT);
        assert_eq!(icon_for_file("LICENSE.MD"), LICENSE);
    }

    #[test]
    fn suffix_matching_respects_segments_and_compound_extensions() {
        assert_eq!(icon_for_file("archive.tar.gz"), ARCHIVE);
        assert_eq!(icon_for_file("types.d.ts"), TYPESCRIPT);
        assert_eq!(icon_for_file("notjavascript"), DEFAULT_FILE_ICON);
    }

    #[test]
    fn unknown_files_use_the_document_fallback() {
        assert_eq!(icon_for_file("AUTHORS"), DEFAULT_FILE_ICON);
        assert_eq!(icon_for_file("sample.unknown"), DEFAULT_FILE_ICON);
    }

    #[test]
    fn registry_contains_every_tree_icon_key() {
        let registry = register(SvgIconRegistry::new());
        let keys = std::iter::once(FOLDER_ICON)
            .chain(std::iter::once(FOLDER_OPEN_ICON))
            .chain(std::iter::once(DEFAULT_FILE_ICON))
            .chain(EXACT_FILE_ICONS.iter().map(|(_, icon)| *icon))
            .chain(SUFFIX_FILE_ICONS.iter().map(|(_, icon)| *icon));

        for key in keys {
            let svg = registry
                .resolve(key)
                .unwrap_or_else(|| panic!("missing registered tree icon: {key}"));
            assert!(svg.contains("<svg"), "invalid registered SVG for {key}");
        }
    }
}
