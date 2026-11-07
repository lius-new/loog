use super::shell;
use crate::app::window as window_geometry;
use crate::infrastructure::launch;
use crate::ui;
use crate::ui::file_icons;
use lgui::icons::SvgIconRegistry;
use lgui::prelude::*;
use lgui::{WinitApplication, WinitWindowIcon, WinitWindowOptions};

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    crate::infrastructure::language_packs::initialize();
    launch::init(std::env::args_os().skip(1));
    let native_window_options = native_window_options()?;
    let settings = crate::infrastructure::settings_filesystem::load();
    let window_options = window_geometry::restore_options(
        WindowOptions::new("loom")
            .text_rendering(ui::theme::text_rendering(settings.theme))
            .title("Loom")
            .size(Size::new(900.0, 600.0))
            // Frameless: we render our own custom title bar (see ui/titlebar.rs).
            .native_titlebar(false)
            // Windows 11 DWM rounded corners.
            .corner_radius(8)
            .on_close_requested(window_geometry::handle_close_requested)
            .with_platform_options(native_window_options),
    );
    let window_options =
        window_options.scale_preference(window_geometry::scale_preference(settings.ui_scale));

    Application::with_backend(WinitApplication::new(GraphicsPreference::Auto))
        .font_families(ui::fonts::SANS_FAMILIES)
        .svg_icons(svg_icons())
        .provide(RendererKind::Skia(GraphicsPreference::Auto))
        .memory_options(MemoryOptions::unbounded(
            ImageCachePolicy::WhileVisible,
            false,
        ))
        .window_options(window_options)
        .run(shell::app)?;
    // "Restart to update" closed the window; the installer replaces Loom now
    // that its files are no longer in use.
    crate::infrastructure::update::run_pending_installer();
    Ok(())
}

pub(crate) fn svg_icons() -> SvgIconRegistry {
    let rings = SvgIconRegistry::new();
    file_icons::register(
        // Codicons are drawn on a 16px grid; render them at theme::ICON_SIZE
        // so their 1px strokes land on whole pixels.
        rings
            .with_icon(
                ui::components::loading_indicator::ICON_KEY,
                ui::components::loading_indicator::SVG,
            )
            .with_icon("panel-left", icondata::VsLayoutSidebarLeft)
            .with_icon("panel-right", icondata::VsLayoutSidebarRight)
            .with_icon("panel-bottom", icondata::VsLayoutPanel)
            .with_icon("git-commit", icondata::VsGitCommit)
            .with_icon("output", icondata::VsOutput)
            .with_icon("filter", icondata::VsFilter)
            .with_icon("problem-error", icondata::VsError)
            .with_icon("check", icondata::VsCheck)
            .with_icon("image-background", icondata::VsColorMode)
            .with_icon("copy", icondata::VsCopy)
            .with_icon("enter", r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" d="M13 3v6H3m4-4L3 9l4 4"/></svg>"#)
            .with_icon("terminal", icondata::VsTerminal)
            // Bottom-panel tabs: one outline family (Lucide) so the four share
            // stroke weight and none carries a frame the others lack.
            .with_icon("panel-terminal", icondata::LuTerminal)
            .with_icon("panel-commits", icondata::LuGitCommitHorizontal)
            .with_icon("panel-output", icondata::LuScrollText)
            .with_icon("panel-problems", icondata::LuCircleAlert)
            .with_icon("terminal-stop", icondata::VsDebugStop)
            .with_icon("terminal-split", icondata::VsSplitHorizontal)
            .with_icon("terminal-more", icondata::VsEllipsis)
            // Four bare corners pointing out (maximize) or in (restore), with no
            // inner frame, as in the design.
            .with_icon("terminal-maximize", r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" d="M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10"/></svg>"#)
            .with_icon("terminal-restore", r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" d="M6 2.5V6H2.5M10 2.5V6h3.5M13.5 10H10v3.5M2.5 10H6v3.5"/></svg>"#)
            .with_icon("terminal-minimize", icondata::VsChromeMinimize)
            .with_icon("terminal-list", icondata::VsListFlat)
            .with_icon("terminal-purpose", icondata::VsTag)
            .with_icon("plus", icondata::VsAdd)
            .with_icon("chevron-down", icondata::VsChevronDown)
            .with_icon("chevron-up", icondata::VsChevronUp)
            .with_icon("chevron-left", icondata::VsChevronLeft)
            .with_icon("chevron-right", icondata::VsChevronRight)
            .with_icon("explorer", icondata::VsLayers)
            .with_icon("settings", icondata::VsSettingsGear)
            .with_icon("close", icondata::VsClose)
            .with_icon("extensions", icondata::VsExtensions)
            .with_icon("extension-code", icondata::VsCode)
            .with_icon("extension-theme", icondata::VsSymbolColor)
            .with_icon("extension-snippet", icondata::VsSymbolSnippet)
            .with_icon("extension-note", icondata::VsNote)
            .with_icon("extension-tools", icondata::VsTools)
            .with_icon("warning", icondata::VsWarning)
            .with_icon("folder-opened", icondata::VsFolderOpened)
            // Codicon's `remove` spans 14 of 16 px and reads wider than
            // `plus` beside it; this one matches the plus visually.
            .with_icon("minus", include_str!("../../assets/icons/minus.svg"))
            .with_icon("source-control", icondata::VsSourceControl)
            .with_icon("arrow-up", icondata::VsArrowUp)
            .with_icon("arrow-down", icondata::VsArrowDown)
            .with_icon("cloud", icondata::VsCloud)
            .with_icon("cloud-upload", icondata::VsCloudUpload)
            .with_icon("cloud-download", icondata::VsCloudDownload)
            .with_icon("info", icondata::VsInfo)
            .with_icon("refresh", icondata::VsRefresh)
            .with_icon("sync", icondata::VsSync)
            .with_icon("search", icondata::VsSearch)
            .with_icon("outline", icondata::VsSymbolStructure)
            .with_icon("expand-all", icondata::VsExpandAll)
            .with_icon("sort", icondata::VsSortPrecedence)
            .with_icon("symbol-class", icondata::VsSymbolClass)
            .with_icon("symbol-method", icondata::VsSymbolMethod)
            .with_icon("symbol-field", icondata::VsSymbolField)
            .with_icon("symbol-interface", icondata::VsSymbolInterface)
            .with_icon("symbol-enum", icondata::VsSymbolEnum)
            .with_icon("symbol-enum-member", icondata::VsSymbolEnumMember)
            .with_icon("symbol-variable", icondata::VsSymbolVariable)
            .with_icon("symbol-constant", icondata::VsSymbolConstant)
            .with_icon("symbol-namespace", icondata::VsSymbolNamespace)
            .with_icon("symbol-property", icondata::VsSymbolProperty)
            .with_icon("symbol-struct", icondata::VsSymbolStructure)
            .with_icon("symbol-event", icondata::VsSymbolEvent)
            .with_icon("symbol-operator", icondata::VsSymbolOperator)
            .with_icon("symbol-parameter", icondata::VsSymbolParameter)
            .with_icon("symbol-string", icondata::VsSymbolString)
            .with_icon("symbol-number", icondata::VsSymbolNumeric)
            .with_icon("symbol-boolean", icondata::VsSymbolBoolean)
            .with_icon("symbol-array", icondata::VsSymbolArray)
            .with_icon("symbol-key", icondata::VsSymbolKey)
            .with_icon("symbol-file", icondata::VsSymbolFile)
            .with_icon("symbol-misc", icondata::VsSymbolMisc)
            .with_icon("symbol-heading", icondata::VsSymbolKeyword)
            .with_icon("search-case", icondata::VsCaseSensitive)
            .with_icon("search-word", icondata::VsWholeWord)
            .with_icon("search-regex", icondata::VsRegex)
            .with_icon("search-stop", icondata::VsDebugStop)
            .with_icon("search-clear", icondata::VsClearAll)
            .with_icon("search-collapse", icondata::VsCollapseAll)
            .with_icon("discard", icondata::VsDiscard)
            .with_icon("go-to-file", icondata::VsGoToFile)
            .with_icon("arrow-left", icondata::VsArrowLeft)
            .with_icon("arrow-right", icondata::VsArrowRight)
            .with_icon("git-stash", icondata::VsGitStash)
            .with_icon("git-stash-apply", icondata::VsGitStashApply)
            .with_icon("git-stash-pop", icondata::VsGitStashPop)
            .with_icon("trash", icondata::VsTrash)
            .with_icon("list-tree", icondata::VsListTree)
            .with_icon("list-flat", icondata::VsListFlat)
            .with_icon("worktree-open", icondata::VsFolderOpened)
            .with_icon("repo", icondata::VsRepo)
            .with_icon("git-commit", icondata::VsGitCommit)
            .with_icon("git-ref-current", icondata::VsTarget)
            .with_icon("git-ref-tag", icondata::VsTag)
            .with_icon("diff", icondata::LuArrowLeftRight)
            // No Codicon equivalent yet.
            .with_icon("git-branch", icondata::LuGitBranch)
            .with_icon("command", icondata::LuCommand)
            .with_icon("update-available", icondata::VsArrowCircleDown)
            .with_icon("update-restart", icondata::VsDebugRestart)
            .with_icon("workspace-untrusted", icondata::VsWorkspaceUntrusted)
            // Filled caption silhouettes avoid fragile subpixel SVG strokes.
            .with_icon(
                "window-minimize",
                include_str!("../../assets/icons/window-minimize.svg"),
            )
            .with_icon(
                "window-maximize",
                include_str!("../../assets/icons/window-maximize.svg"),
            )
            .with_icon(
                "window-restore",
                include_str!("../../assets/icons/window-restore.svg"),
            )
            .with_icon(
                "window-close",
                include_str!("../../assets/icons/window-close.svg"),
            ),
    )
}

fn native_window_options() -> Result<WinitWindowOptions, Box<dyn std::error::Error>> {
    #[cfg(target_os = "windows")]
    {
        // Match the embedded multi-size executable icon exactly. Winit's runtime
        // RGBA-to-HICON conversion renders differently from the PE icon resource.
        let options = WinitWindowOptions::new().window_icon(icon_from_ico_frame(16)?);
        return Ok(options.taskbar_icon(icon_from_ico_frame(32)?));
    }

    #[cfg(not(target_os = "windows"))]
    {
        // The 32 px export uses artwork tuned for small sizes, so it is used as is
        // instead of downscaling the 1024 px image.
        let window_pixels = image::load_from_memory_with_format(
            include_bytes!("../../icons/source/app-icon-transparent-32.png"),
            image::ImageFormat::Png,
        )?
        .into_rgba8();
        let window_icon = WinitWindowIcon::from_rgba(window_pixels.into_raw(), 32, 32)?;
        Ok(WinitWindowOptions::new().window_icon(window_icon))
    }
}

#[cfg(target_os = "windows")]
fn icon_from_ico_frame(size: u32) -> Result<WinitWindowIcon, Box<dyn std::error::Error>> {
    let ico = include_bytes!("../../icons/app.ico");
    let entry_count = u16::from_le_bytes([ico[4], ico[5]]) as usize;

    for index in 0..entry_count {
        let entry = 6 + index * 16;
        if entry + 16 > ico.len() {
            break;
        }
        let width = if ico[entry] == 0 {
            256
        } else {
            u32::from(ico[entry])
        };
        let height = if ico[entry + 1] == 0 {
            256
        } else {
            u32::from(ico[entry + 1])
        };
        if width != size || height != size {
            continue;
        }

        let image_length = u32::from_le_bytes([
            ico[entry + 8],
            ico[entry + 9],
            ico[entry + 10],
            ico[entry + 11],
        ]) as usize;
        let image_offset = u32::from_le_bytes([
            ico[entry + 12],
            ico[entry + 13],
            ico[entry + 14],
            ico[entry + 15],
        ]) as usize;
        let image_end = image_offset
            .checked_add(image_length)
            .filter(|end| *end <= ico.len())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid ICO frame")
            })?;
        let pixels = image::load_from_memory_with_format(
            &ico[image_offset..image_end],
            image::ImageFormat::Png,
        )?
        .into_rgba8();
        return Ok(WinitWindowIcon::from_rgba(pixels.into_raw(), size, size)?);
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("Missing {size}x{size} PNG frame in icons/app.ico"),
    )
    .into())
}
