//! Native image publications from target bytes, without a visible window.
use super::*;
pub(super) fn run(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    use image::ImageEncoder;
    let loog_workspace_api::ResourceRef::Provided {
        path: base,
        environment,
    } = &root.resource
    else {
        panic!("provided root")
    };
    let path = child_path(
        ui,
        host,
        root.scope,
        base.clone(),
        &format!("native-image-{}-中文.png", std::process::id()),
    );
    let resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), path.as_str()).unwrap();
    let write = |width: u32, color: [u8; 4]| {
        let pixels = color.repeat(width as usize);
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&pixels, width, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        request(
            ui,
            host,
            root.scope,
            Operation::Write {
                path: path.clone(),
                bytes,
                expected_revision: None,
            },
        );
    };
    write(1, [255, 255, 255, 255]);
    let Answer::Info { file } =
        request(ui, host, root.scope, Operation::Stat { path: path.clone() })
    else {
        panic!("image stat")
    };
    crate::app::resources::open(&ui.state, root.scope, file);
    wait(ui, |app| {
        app.workspace.file_id_for_resource(&resource).is_some()
    });
    let (id, view, identity) = {
        let app = ui.state.get();
        let id = app.workspace.file_id_for_resource(&resource).unwrap();
        let view = app.workspace.active().unwrap();
        let doc = &app.workspace.documents[&id];
        let image = doc.image.as_ref().unwrap();
        assert!(!doc.is_text());
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(
            app.workspace.view(view).unwrap().presentation,
            crate::domain::view::Presentation::Image
        );
        assert!(app.workspace.save_snapshot(id).is_none());
        assert!(crate::app::image_preview::frame(&app, view).is_some());
        (id, view, image.identity)
    };
    write(2, [20, 120, 220, 255]);
    wait(ui, |app| {
        app.workspace.documents[&id]
            .image
            .as_ref()
            .is_some_and(|image| image.width == 2 && image.identity != identity)
    });
    ui.state.update(|app| {
        assert_eq!(app.workspace.active(), Some(view));
        assert_eq!(app.workspace.file_id_for_resource(&resource), Some(id));
        assert!(crate::app::image_preview::frame(app, view).is_some());
        app.workspace.close(id);
    });
    request(
        ui,
        host,
        root.scope,
        Operation::Remove {
            path,
            recursive: false,
        },
    );
    println!(
        "CHECK: target PNG bytes open in the native image view and refresh through resource notifications"
    );
}
