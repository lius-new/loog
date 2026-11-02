use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-tests");
        let directory = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }

    fn write(&self, name: &str, content: &str) -> PathBuf {
        let file = self.0.join(name);
        fs::write(&file, content).unwrap();
        file
    }

    fn json_extension(&self) -> PathBuf {
        let source = bundled("loom.lang.json");
        let destination = self.0.join("loom.lang.json");
        copy_tree(&source, &destination);
        destination
    }

    fn samples(&self, grammar: &str, text: &str) -> PathBuf {
        let sample = self.write("sample.txt", text);
        self.write(
            "samples.json",
            &serde_json::json!([{"grammar": grammar, "sample": sample}]).to_string(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn bundled(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/extensions")
        .join(id)
}

fn validate(extension: &Path, samples: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loom-ext-validate"));
    command.arg(extension);
    if let Some(samples) = samples {
        command.arg("--samples").arg(samples);
    }
    command.output().unwrap()
}

fn succeeded(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn rejected(output: &Output, message: &str) {
    assert!(!output.status.success(), "invalid extension was accepted");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(message), "{error}");
}

#[test]
fn bundled_languages_load_and_samples_are_colored() {
    for (id, grammar, text) in [
        (
            "loom.lang.json",
            "json",
            r#"{"name": "Loom", "enabled": true}"#,
        ),
        (
            "loom.lang.markdown",
            "markdown",
            "# A heading\n\nA paragraph.\n",
        ),
        ("loom.lang.markdown", "markdown_inline", "A **bold** word."),
    ] {
        let fixture = Fixture::new();
        let samples = fixture.samples(grammar, text);
        let output = validate(&bundled(id), Some(&samples));
        succeeded(&output);
        let rows: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(rows.iter().any(|row| row["grammar"] == grammar
            && row["colored"].as_u64().is_some_and(|count| count > 0)));
    }
    succeeded(&validate(&bundled("loom.aura"), None));
}

#[test]
fn a_broken_module_and_an_invalid_query_are_rejected() {
    let fixture = Fixture::new();
    let extension = fixture.json_extension();
    fs::write(extension.join("tree-sitter-json.wasm"), b"not wasm").unwrap();
    rejected(&validate(&extension, None), "json:");

    let extension = fixture.json_extension();
    fs::write(
        extension.join("queries/highlights.scm"),
        "(nonexistent_node) @string",
    )
    .unwrap();
    rejected(&validate(&extension, None), "nonexistent_node");
}

#[test]
fn samples_with_parse_errors_or_unknown_grammars_are_rejected() {
    let fixture = Fixture::new();
    let sample = fixture.samples("json", "{broken");
    rejected(
        &validate(&bundled("loom.lang.json"), Some(&sample)),
        "parse errors",
    );
    let sample = fixture.samples("rust", "fn main() {}");
    rejected(
        &validate(&bundled("loom.lang.json"), Some(&sample)),
        "no grammar for",
    );
}

#[test]
fn every_supplied_sample_is_validated_and_needs_a_file() {
    let fixture = Fixture::new();
    let valid = fixture.write("valid.json", r#"{"name":"Loom"}"#);
    let invalid = fixture.write("invalid.json", "{broken");
    let samples = fixture.write(
        "samples.json",
        &serde_json::json!([
            {"grammar": "json", "sample": valid},
            {"grammar": "json", "sample": invalid}
        ])
        .to_string(),
    );
    rejected(
        &validate(&bundled("loom.lang.json"), Some(&samples)),
        "parse errors",
    );
    let samples = fixture.write("samples.json", r#"[{"grammar":"json"}]"#);
    rejected(
        &validate(&bundled("loom.lang.json"), Some(&samples)),
        "missing field",
    );
}

#[test]
fn missing_manifest_and_invalid_arguments_fail() {
    let fixture = Fixture::new();
    rejected(&validate(&fixture.0, None), "manifest.json");
    let output = Command::new(env!("CARGO_BIN_EXE_loom-ext-validate"))
        .output()
        .unwrap();
    rejected(&output, "usage:");
}

#[test]
fn language_identity_uses_the_host_loader_without_requiring_a_grammar() {
    let fixture = Fixture::new();
    let folder = fixture.0.join("loom.lsp.demo");
    fs::create_dir_all(folder.join("languages")).unwrap();
    fs::write(folder.join("manifest.json"), r#"{
        "schema":1, "id":"loom.lsp.demo", "name":"Demo", "version":"0.1.0",
        "contributes":{"languages":[{"id":"loom.lsp.demo.language","path":"languages/language.json"}]}
    }"#).unwrap();
    let language = folder.join("languages/language.json");
    fs::write(&language, r#"{"name":"demo","display_name":"Demo","badge":"D","variants":[{"id":"demo"}],"files":[{"extensions":["demo"],"variant":"demo"}]}"#).unwrap();
    succeeded(&validate(&folder, None));
    fs::write(
        &language,
        r#"{"name":"demo","variants":[{"id":"demo","grammar":"demo"}]}"#,
    )
    .unwrap();
    rejected(&validate(&folder, None), "parser");
    fs::write(&language, "{").unwrap();
    rejected(&validate(&folder, None), "loom.lsp.demo.language");
}
