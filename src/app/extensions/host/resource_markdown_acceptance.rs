//! Target relative resources through the production parser, layout and commands.
use super::*;
use crate::app::{
    markdown_assets::Base,
    markdown_render::{Request, RequestKey, Status},
    markdown_service::MarkdownService,
};
use crate::infrastructure::markdown_worker::{Consumer, ParseStatus};

struct Clipboard;
impl lgui::services::Clipboard for Clipboard {
    fn read_text(&self) -> Result<Option<String>, lgui::services::ClipboardError> {
        Ok(None)
    }
    fn write_text(&self, _: &str) -> Result<(), lgui::services::ClipboardError> {
        Ok(())
    }
}
pub(super) fn run(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    use image::ImageEncoder;
    let loog_workspace_api::ResourceRef::Provided {
        path: base,
        environment,
    } = &root.resource
    else {
        panic!("provided root")
    };
    let project = child_path(
        ui,
        host,
        root.scope,
        base.clone(),
        &format!("native-markdown-{}", std::process::id()),
    );
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: project.clone(),
        },
    );
    let directory = child_path(ui, host, root.scope, project.clone(), "assets");
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: directory.clone(),
        },
    );
    let image = child_path(ui, host, root.scope, directory.clone(), "中文\\image.png");
    let source = child_path(ui, host, root.scope, project.clone(), "README.md");
    let target = child_path(ui, host, root.scope, project.clone(), "next.md");
    let write_image = |width: u32| {
        let mut bytes = vec![];
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &[30, 120, 210, 255].repeat(width as usize),
                width,
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        request(
            ui,
            host,
            root.scope,
            Operation::Write {
                path: image.clone(),
                bytes,
                expected_revision: None,
            },
        );
    };
    write_image(2);
    for (path, text) in [
        (
            &source,
            "# Origin\n\n![Target image](assets/中文%5Cimage.png)\n\n[Next](next.md#target)\n",
        ),
        (&target, "# Target\n\n[Assets](assets)\n"),
    ] {
        request(
            ui,
            host,
            root.scope,
            Operation::Write {
                path: path.clone(),
                bytes: text.as_bytes().to_vec(),
                expected_revision: None,
            },
        );
    }
    let Answer::Info { file } = request(
        ui,
        host,
        root.scope,
        Operation::Stat {
            path: source.clone(),
        },
    ) else {
        panic!("markdown file")
    };
    crate::app::resources::open(&ui.state, root.scope, file);
    let resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), source.as_str()).unwrap();
    wait(ui, |app| {
        app.workspace.file_id_for_resource(&resource).is_some()
    });
    let source_id = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(&resource)
        .unwrap();
    let view = ui.state.get().workspace.active().unwrap();
    ui.runtime.install_markdown(MarkdownService::new(
        ui.state.clone(),
        ui.tasks.application(),
        Arc::new(Clipboard),
    ));
    let service = ui.runtime.markdown().unwrap();
    ui.state
        .update(|app| assert!(crate::app::markdown_view::toggle(app)));
    let render = |app: &AppState| {
        let Some(ParseStatus::Ready(publication)) = service.query(app, view) else {
            return None;
        };
        let status = service.render.request(Request {
            view: Consumer::Document(view),
            key: RequestKey {
                parse: publication.revision,
                width: 640.0,
                height: 480.0,
                position: Default::default(),
                ink: Default::default(),
                density: 1.0,
                font_size: 16.0,
                line_height: 24.0,
                base: Base::for_view(app, view),
                remote: true,
                page: true,
                fragment: None,
                horizontal: vec![],
            },
            publication,
        });
        match status {
            Status::Ready(rendered) => Some(rendered),
            Status::Failed(error) => panic!("Markdown layout: {error}"),
            _ => None,
        }
    };
    for width in [2, 3] {
        if width == 3 {
            write_image(width);
        }
        wait(ui, |app| {
            render(app).is_some_and(|rendered| {
                rendered.images.values().any(|asset| {
                    asset
                        .outcome
                        .as_ref()
                        .is_ok_and(|image| image.width == width && image.height == 1)
                })
            })
        });
    }
    let pane = ui.state.get().workspace.active_pane();
    crate::app::resource_link::open(&ui.state, pane, view, "next.md#target");
    let destination =
        loog_workspace_api::ResourceRef::provided(environment.clone(), target.as_str()).unwrap();
    wait(ui, |app| {
        app.workspace
            .active_document()
            .is_some_and(|id| app.workspace.documents[&id].meta.resource == destination)
    });
    let target_id = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(&destination)
        .unwrap();
    let target_view = ui.state.get().workspace.active().unwrap();
    assert_eq!(
        ui.state
            .get()
            .workspace
            .view(target_view)
            .unwrap()
            .presentation,
        crate::domain::view::Presentation::Markdown
    );
    assert_eq!(
        ui.state.get().editor.markdown_views[&target_view]
            .fragment
            .as_deref(),
        Some("target")
    );
    crate::app::resource_link::open(&ui.state, pane, target_view, "assets");
    wait(ui, |app| {
        app.explorer
            .resources
            .root(root.scope)
            .and_then(|root| root.directories.get(&directory))
            .is_some_and(|directory| directory.expanded && directory.loaded)
    });
    assert_eq!(ui.state.get().workspace.active(), Some(target_view));
    ui.state.update(|app| {
        app.workspace.close(target_id);
        app.workspace.close(source_id);
    });
    for path in [image, source, target, directory, project] {
        request(
            ui,
            host,
            root.scope,
            Operation::Remove {
                path,
                recursive: false,
            },
        );
    }
    println!(
        "CHECK: target Markdown parses/layouts with relative images, refreshes changed pixels and opens file/heading/directory links in its environment"
    );
}
