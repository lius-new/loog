//! Native file dialog adapter; app owns opening the selected document.
use lgui::dialogs::{FileDialogOptions, system_file_dialogs};
use std::path::PathBuf;
/// Choose an extension package (`.loomext`) to import.
pub fn pick_extension_package() -> Option<PathBuf> {
    let options = FileDialogOptions::new()
        .title("Import Extension")
        .filter("Loom extension", ["loomext"]);
    system_file_dialogs().pick_file(&options)
}

pub fn pick_file(directory: Option<PathBuf>) -> Option<PathBuf> {
    let mut options = FileDialogOptions::new().title("Open File");
    if let Some(directory) = directory {
        options = options.directory(directory);
    }
    system_file_dialogs().pick_file(&options)
}
