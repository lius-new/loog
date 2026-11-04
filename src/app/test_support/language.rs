//! Runs the extension-owned client against the deterministic server fixture.
//! The client source and builder live in loom-extensions, outside the Loom SDK.
use crate::app::{context::AppContext, runtime::AppRuntime};
use std::{
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

const COMPONENT: &[u8] =
    include_bytes!("../../../tests/fixtures/extensions/language-client/extension.wasm");
static NEXT: AtomicU64 = AtomicU64::new(1);

fn cache() -> &'static PathBuf {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE.get_or_init(|| {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-language-client-cache");
        loom_extension_host::Runtime::new()
            .unwrap()
            .compile_cached(COMPONENT, &path)
            .unwrap();
        path
    })
}

/// Install a real WASM client; every harness gets its own owner and process.
pub(crate) fn install(state: &AppContext, runtime: &Arc<AppRuntime>, arguments: &[String]) {
    let cache = cache().clone();
    let directory = std::env::temp_dir().join(format!(
        "loom-ui-client-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let mut arguments = arguments.to_vec();
    if !arguments.iter().any(|value| value == "--log") {
        arguments.extend([
            "--log".into(),
            directory
                .join("traffic.jsonl")
                .to_string_lossy()
                .into_owned(),
        ]);
    }
    let binary = if cfg!(windows) {
        "fake_lsp.exe"
    } else {
        "fake_lsp"
    };
    let executable = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(binary);
    std::fs::hard_link(&executable, directory.join(binary))
        .or_else(|_| std::fs::copy(&executable, directory.join(binary)).map(|_| ()))
        .unwrap();
    let component = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/extensions/language-client/extension.wasm");
    std::fs::hard_link(&component, directory.join("extension.wasm"))
        .or_else(|_| std::fs::copy(&component, directory.join("extension.wasm")).map(|_| ()))
        .unwrap();
    let providers = [
        "diagnostics",
        "completion",
        "hover",
        "definition",
        "document-symbols",
        "signature-help",
        "formatting",
        "inlay-hints",
        "code-actions",
        "references",
        "workspace-symbols",
        "document-highlights",
        "rename",
        "folding-ranges",
        "selection-ranges",
        "call-hierarchy",
        "type-hierarchy",
        "code-lens",
        "format-on-type",
        "execute-command",
        "highlights",
    ];
    let manifest = serde_json::json!({
        "schema":1, "id":"example.language-fixture", "version":"0.1.0", "api":"loom:extension@0.1.0", "component":"extension.wasm",
        "executables":[binary], "activation":[{"kind":"document", "selector":{"languages":["rust"]}}, {"kind":"event","name":"document.open"}, {"kind":"event","name":"document.change"}, {"kind":"event","name":"document.close"}, {"kind":"event","name":"document.save"}],
        "capabilities":["process.execute-local", "documents.read", "workspace.files.read", "workspace.files.write", "workspace.edit", "languages", "events", "settings"],
        "contributes":{
            "settings":[{"key":"example.language-fixture.options", "kind":"string", "default":serde_json::json!({"args":arguments}).to_string()}],
            "language_service":{"providers":providers.into_iter().map(|kind| serde_json::json!({"id":format!("example.language-fixture.{kind}"), "kind":kind, "selector":{"languages":["rust"]}})).collect::<Vec<_>>()}
        }
    });
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let root = std::env::temp_dir();
    runtime.trust().set_workspace(std::slice::from_ref(&root));
    runtime.trust().trust(&root).unwrap();
    state.update(|app| {
        app.explorer.model.add_root(
            &crate::infrastructure::explorer_filesystem::ExplorerFilesystem,
            root,
        );
    });
    let host = crate::app::extensions::host::ExtensionHost::start(
        state.clone(),
        runtime.application().unwrap(),
        cache,
        runtime.trust().clone(),
        None,
    )
    .unwrap();
    host.load_package(&directory).unwrap();
    runtime.install_extensions(host);
    state.update(|_| {});
}

pub(crate) fn saved(state: &AppContext, path: PathBuf, text: Arc<String>) {
    let app = state.get();
    let (file, document) = app
        .workspace
        .documents
        .iter()
        .find(|(_, document)| document.meta.path == path)
        .unwrap();
    let saved = crate::domain::document::SaveSnapshot {
        stamp: document.stamp(*file),
        path,
        text,
        disk_observation: 0,
        format: Default::default(),
    };
    drop(app);
    state
        .services()
        .unwrap()
        .language_services()
        .unwrap()
        .saved_document(saved);
}
