//! Development acceptance against the three actual Rust client components.
//! LOOG_MOTION_PACKAGES=<staged root> cargo test --release -p loog-extension-host
//! --test language_client_motion -- --ignored --nocapture
use loog_extension_host::{
    DocumentRef, DocumentView, Inbound, Limits, MotionProperty, OutputChannel, ProcessControl,
    ProcessExit, ProcessOutput, RootRef, Runtime,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

#[test]
#[ignore = "requires locally staged Rust language client components"]
fn actual_language_clients_animate_status_without_per_frame_guest_calls() {
    let root =
        PathBuf::from(std::env::var_os("LOOG_MOTION_PACKAGES").expect("set LOOG_MOTION_PACKAGES"));
    let installed: Value =
        serde_json::from_slice(&std::fs::read(root.join("extensions.json")).unwrap()).unwrap();
    let runtime = Runtime::new().unwrap();
    for (id, language, filename) in [
        ("loog.lsp.gopls", "go", "main.go"),
        ("loog.lsp.rust-analyzer", "rust", "main.rs"),
        (
            "loog.lsp.typescript-language-server",
            "typescript",
            "main.ts",
        ),
    ] {
        let directory = root
            .join(id)
            .join(installed["extensions"][id]["version"].as_str().unwrap());
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap())
                .unwrap();
        let _ = manifest["name"].as_str().unwrap();
        let component = runtime
            .compile(&std::fs::read(directory.join("extension.wasm")).unwrap())
            .unwrap();
        let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
        let reference = DocumentRef {
            document: 1,
            lifecycle: 1,
            path: format!("C:/motion-preview/{filename}"),
            language: language.into(),
            variant: None,
            version: 1,
        };
        instance.put_roots(vec![RootRef {
            id: 1,
            name: "motion-preview".into(),
            path: "C:/motion-preview".into(),
        }]);
        instance.put_documents(
            vec![DocumentView {
                reference: reference.clone(),
                text: Some(Arc::new("fn main() {}".into())),
            }],
            &[],
        );
        instance
            .dispatch(&[Inbound::DocumentOpened(reference)])
            .unwrap();
        let nodes = instance.take_ui_snapshot().expect("startup UI");
        let shared = nodes
            .iter()
            .filter_map(|node| node.shared.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(
            shared.len(),
            1,
            "{id}: one private candidate for this session"
        );
        assert_eq!(shared[0].key, "loog.language-services.status.v1");
        assert_eq!(
            shared[0].contexts.as_ref().unwrap(),
            &[format!("C:/motion-preview/{filename}")]
        );
        assert!(
            nodes
                .iter()
                .all(|node| node.description.background.is_none())
        );
        let icon = nodes
            .iter()
            .find(|n| n.description.id.ends_with("-icon"))
            .expect("status icon");
        let icon_id = icon.node;
        let (_, spin) = icon.motion.animation.as_ref().expect("startup spin");
        assert_eq!(spin.iterations, None, "{id}: startup is animated");
        assert!(matches!(spin.tracks[0].property, MotionProperty::Rotation));
        let controls = instance.take_process_controls();
        let process = controls
            .iter()
            .find_map(|c| {
                if let ProcessControl::Spawn { process, .. } = c {
                    Some(*process)
                } else {
                    None
                }
            })
            .unwrap();
        let request: Value = controls
            .iter()
            .find_map(|c| {
                let ProcessControl::Write { bytes, .. } = c else {
                    return None;
                };

                serde_json::from_slice(bytes).ok()
            })
            .expect("initialize request");
        let deliver = |instance: &mut loog_extension_host::Instance, value: Value| {
            instance
                .dispatch(&[Inbound::ProcessOutput(ProcessOutput {
                    process,
                    channel: OutputChannel::Stdout,
                    bytes: vec![],
                    text: Some(value.to_string()),
                })])
                .unwrap();
        };
        deliver(
            &mut instance,
            json!({"jsonrpc":"2.0","id":request["id"],"result":{"capabilities":{"textDocumentSync":2,"hoverProvider":true}}}),
        );
        let nodes = instance.take_ui_snapshot().expect("ready UI");
        assert!(
            nodes
                .iter()
                .any(|n| n.description.text.as_deref() == Some("Language Services: Ready"))
        );
        let icon = nodes.iter().find(|n| n.node == icon_id).unwrap();
        assert!(icon.motion.animation.is_none(), "{id}: ready stays static");
        let ready_icon = icon.description.icon.as_deref().unwrap();
        assert!(ready_icon.starts_with("svg-path:24x24:"));
        assert!(ready_icon.len() <= 256);

        deliver(
            &mut instance,
            json!({"jsonrpc":"2.0","method":"$/progress","params":{"token":"index","value":{"kind":"begin","title":"Indexing"}}}),
        );
        let nodes = instance.take_ui_snapshot().unwrap();
        let icon = nodes.iter().find(|n| n.node == icon_id).unwrap();
        let started = icon.motion.animation.as_ref().unwrap().0;
        assert_eq!(icon.motion.animation.as_ref().unwrap().1.iterations, None);
        deliver(
            &mut instance,
            json!({"jsonrpc":"2.0","method":"$/progress","params":{"token":"index","value":{"kind":"report","percentage":50}}}),
        );
        assert!(
            instance.take_ui_snapshot().is_none(),
            "{id}: progress does not mutate the UI or restart spin {started}"
        );
        deliver(
            &mut instance,
            json!({"jsonrpc":"2.0","method":"$/progress","params":{"token":"index","value":{"kind":"end"}}}),
        );
        let nodes = instance.take_ui_snapshot().unwrap();
        assert!(
            nodes
                .iter()
                .find(|n| n.node == icon_id)
                .unwrap()
                .motion
                .animation
                .is_none(),
            "{id}: completed progress returns to a static ready icon"
        );
        instance
            .dispatch(&[Inbound::ProcessExited(ProcessExit {
                process,
                code: Some(1),
                killed: false,
                error: None,
            })])
            .unwrap();
        let nodes = instance.take_ui_snapshot().unwrap();
        assert!(
            nodes
                .iter()
                .find(|n| n.node == icon_id)
                .unwrap()
                .motion
                .animation
                .is_none(),
            "{id}: failures stop motion"
        );
        instance.dispatch(&[Inbound::Shutdown]).unwrap();
        assert!(instance.take_ui_snapshot().unwrap().is_empty());
        println!("{id}: startup spin, static ready, progress continuity and cleanup passed");
    }
}
