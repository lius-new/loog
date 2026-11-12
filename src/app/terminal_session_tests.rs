//! Preserved native and cross-layer terminal regressions, composed by app.
#[cfg(test)]
mod tests {
    use crate::app::terminal_binding::TerminalTab;
    use crate::app::terminal_binding::fixtures::Sessions as TerminalTabs;
    use crate::app::terminal_runtime::TerminalDisplay;
    use crate::features::terminal::ShellKind;
    use crate::features::terminal::TerminalSize;
    #[cfg(windows)]
    use crate::features::terminal::TerminalStatus;
    #[cfg(windows)]
    use crate::infrastructure::terminal::pty::shell_friendly_path;
    use crate::infrastructure::terminal::session::TerminalController;
    #[cfg(windows)]
    use lgui::ApplicationHandle;
    use std::env;
    use std::path::PathBuf;
    #[cfg(windows)]
    use std::sync::Arc;
    #[cfg(windows)]
    use std::thread;
    #[cfg(windows)]
    use std::{io::Write, path::Path, time::Duration};

    #[test]
    fn narrowing_through_a_wide_glyph_keeps_subsequent_redraws_valid() {
        let controller = TerminalController::new();
        let size = TerminalSize {
            rows: 2,
            cols: 4,
            pixel_width: 7,
            pixel_height: 19,
        };
        controller.resize(size);
        controller.test_parser().process("ab中".as_bytes());
        controller.resize(TerminalSize { cols: 3, ..size });
        controller.test_parser().process("\x1b[1;2H文".as_bytes());
        assert_eq!(controller.test_parser().screen().contents(), "a文");
        assert_eq!(controller.snapshot().cols, 3);
        controller.test_parser().process(b"\r\nafter redraw");
        assert!(
            controller
                .test_parser()
                .screen()
                .contents()
                .contains("redraw")
        );
    }

    #[test]
    fn width_changes_never_leave_half_a_wide_character_at_the_row_edge() {
        for cols in 2..=120 {
            let mut parser = vt100::Parser::new(2, cols + 1, 20);
            parser.process(format!("{}\x1b[41m中", "x".repeat(usize::from(cols - 1))).as_bytes());
            parser.screen_mut().resize(2, cols);
            let edge = parser.screen().cell(0, cols - 1).unwrap();
            assert!(!edge.is_wide(), "columns={cols}");
            assert!(!edge.is_wide_continuation(), "columns={cols}");
            assert_eq!(edge.bgcolor(), vt100::Color::Idx(1));
            parser.process(format!("\x1b[1;{}H文", cols - 1).as_bytes());
            assert!(parser.screen().cell(0, cols - 2).unwrap().is_wide());
            assert!(
                parser
                    .screen()
                    .cell(0, cols - 1)
                    .unwrap()
                    .is_wide_continuation()
            );
            parser.screen_mut().resize(2, cols + 3);
            parser.process("\x1b[2;1H继续输入".as_bytes());
        }
    }

    #[test]
    fn scrollbar_snapshot_tracks_retained_history_offset_and_active_screen() {
        let controller = TerminalController::new();
        assert_eq!(controller.snapshot().scrollback_rows, 0);
        controller
            .test_parser()
            .process("line\r\n".repeat(40).as_bytes());
        let snapshot = controller.snapshot();
        assert_eq!(snapshot.scrollback, 0);
        assert_eq!(snapshot.scrollback_rows, 17);
        controller.set_scrollback(usize::MAX);
        let snapshot = controller.snapshot();
        assert_eq!(snapshot.scrollback, snapshot.scrollback_rows);
        assert!(!snapshot.cursor_visible);
        controller.test_parser().process(b"\x1b[?1049h");
        assert_eq!(controller.snapshot().scrollback_rows, 0);
        controller.test_parser().process(b"\x1b[?1049l");
        assert_eq!(controller.snapshot().scrollback_rows, 17);
        controller.set_scrollback(0);
        assert_eq!(controller.snapshot().scrollback, 0);
    }

    #[test]
    fn shrinking_and_growing_terminal_height_restores_logs_cursor_and_styles() {
        let controller = TerminalController::new();
        let size = TerminalSize {
            rows: 8,
            cols: 80,
            pixel_width: 7,
            pixel_height: 19,
        };
        controller.resize(size);
        controller.test_parser().process(
            "started api\r\n\r\nNative API started\r\nDocker dependencies remain running\r\n\x1b[31m[api] 中文服务日志\x1b[m\r\n[api] Swagger ready".as_bytes(),
        );
        let original = controller.test_parser().screen().clone();
        let generation = controller.generation();
        for rows in [3, 1, 5, 2, 8] {
            controller.resize(TerminalSize { rows, ..size });
        }
        let parser = controller.test_parser();
        assert_eq!(parser.screen().contents(), original.contents());
        assert_eq!(
            parser.screen().cursor_position(),
            original.cursor_position()
        );
        assert_eq!(parser.screen().cell(4, 0), original.cell(4, 0));
        assert_eq!(controller.generation(), generation);
    }

    #[test]
    fn output_received_while_short_remains_in_history_after_growing() {
        let controller = TerminalController::new();
        let size = TerminalSize {
            rows: 6,
            cols: 80,
            pixel_width: 7,
            pixel_height: 19,
        };
        controller.resize(size);
        controller
            .test_parser()
            .process(b"log 0\r\nlog 1\r\nlog 2\r\nlog 3");
        controller.resize(TerminalSize { rows: 2, ..size });
        controller.test_parser().process(b"\r\nlog 4\r\nlog 5");
        controller.resize(size);
        assert_eq!(
            controller.test_parser().screen().contents(),
            "log 0\nlog 1\nlog 2\nlog 3\nlog 4\nlog 5"
        );
    }

    #[test]
    fn height_resize_keeps_wrapped_lines_and_incomplete_escape_sequences() {
        let mut parser = vt100::Parser::new(5, 6, 20);
        parser.process(b"abcdefghijkl\r\nlast\x1b[3");
        let original = parser.screen().contents();
        parser.screen_mut().set_size(1, 6);
        parser.screen_mut().set_size(5, 6);
        assert_eq!(parser.screen().contents(), original);
        assert!(parser.screen().row_wrapped(0));
        parser.process(b"1m!");
        assert_eq!(
            parser.screen().cell(2, 4).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        let mut parser = vt100::Parser::new(4, 6, 20);
        parser.process(b"abcdef");
        parser.screen_mut().set_size(2, 6);
        parser.screen_mut().set_size(4, 6);
        parser.process(b"g");
        assert_eq!(parser.screen().contents(), "abcdefg");
        assert!(parser.screen().row_wrapped(0));
    }

    #[test]
    fn resizing_alternate_screen_keeps_primary_logs_and_history_separate() {
        let mut parser = vt100::Parser::new(6, 40, 20);
        parser.process(b"log 0\r\nlog 1\r\nlog 2\r\nlog 3\r\nlog 4");
        let original = parser.screen().clone();
        parser.process(b"\x1b[?1049hTUI screen");
        parser.screen_mut().set_size(2, 40);
        assert!(parser.screen().alternate_screen());
        parser.screen_mut().set_scrollback(100);
        assert_eq!(parser.screen().scrollback(), 0);
        assert!(!parser.screen().contents().contains("log"));
        parser.process(b"\x1b[?1049l");
        parser.screen_mut().set_size(6, 40);
        assert_eq!(parser.screen().contents(), original.contents());
        assert_eq!(
            parser.screen().cursor_position(),
            original.cursor_position()
        );
    }

    #[test]
    fn shrinking_preserves_nonempty_rows_below_the_cursor() {
        let mut parser = vt100::Parser::new(5, 40, 20);
        parser.process(b"line 0\r\nline 1\r\nline 2\r\nline 3\x1b[H");
        parser.screen_mut().set_size(2, 40);
        parser.screen_mut().set_size(5, 40);
        assert_eq!(parser.screen().contents(), "line 0\nline 1\nline 2\nline 3");
    }

    #[test]
    fn shrinking_respects_history_limit_and_preserves_latest_logs() {
        let mut parser = vt100::Parser::new(5, 40, 3);
        parser.process(b"log 0\r\nlog 1\r\nlog 2\r\nlog 3\r\nlog 4");
        parser.screen_mut().set_size(1, 40);
        assert_eq!(parser.screen().contents(), "log 4");
        parser.screen_mut().set_scrollback(100);
        assert_eq!(parser.screen().scrollback(), 3);
        assert_eq!(parser.screen().contents(), "log 1");
        parser.screen_mut().set_scrollback(0);
        parser.screen_mut().set_size(5, 40);
        assert_eq!(parser.screen().contents(), "log 1\nlog 2\nlog 3\nlog 4");
    }

    #[cfg(windows)]
    #[test]
    fn custom_profiles_preserve_arguments_split_configuration_and_missing_path_fallback() {
        use crate::features::terminal::profiles::ShellProfile;
        use std::time::{Instant, SystemTime, UNIX_EPOCH};
        let root = env::temp_dir().join(format!(
            "loog-custom-shell-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let source = root.join("probe.rs");
        let program = root.join("中文 custom shell.exe");
        std::fs::write(&source, r#"use std::io::Write; fn main() { for (i, arg) in std::env::args().skip(1).enumerate() { println!("ARG{i}={arg:?}"); } std::io::stdout().flush().unwrap(); let mut line = String::new(); let _ = std::io::stdin().read_line(&mut line); }"#).unwrap();
        let built = std::process::Command::new("rustc")
            .arg("--crate-name=loog_custom_shell_probe")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let profile = ShellProfile {
            id: 123,
            name: "Custom shell".into(),
            program,
            args: vec![
                "".into(),
                "space path".into(),
                "literal \"quotes\"".into(),
                "C:\\trailing path\\".into(),
                "中文 $() ` &".into(),
            ],
        };
        let mut tabs = TerminalTabs::restored(vec![], None);
        tabs.set_profiles(std::slice::from_ref(&profile));
        let first = tabs.add_at(ShellKind::Custom(profile.id), Some(root.clone()));
        let controller = tabs.active().unwrap().controller.test_controller().clone();
        let second = tabs.split(first).unwrap();
        assert_eq!(
            *tabs
                .active()
                .unwrap()
                .controller
                .test_controller()
                .test_profile(),
            Some(profile.clone())
        );
        assert_eq!(tabs.active_id(), Some(second));
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let size = TerminalSize {
            rows: 24,
            cols: 120,
            pixel_width: 840,
            pixel_height: 456,
        };
        controller.restart(
            ShellKind::Custom(profile.id),
            Some(&root),
            size,
            application.clone(),
        );
        let expected = profile
            .args
            .iter()
            .enumerate()
            .map(|(i, arg)| format!("ARG{i}={arg:?}"))
            .collect::<Vec<_>>();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let output = controller.test_parser().screen().contents();
            if expected.iter().all(|arg| output.contains(arg)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "custom args: {output:?}; expected: {expected:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(controller.shell(), ShellKind::Custom(profile.id));
        controller.terminate(&application);
        let mut missing = profile.clone();
        missing.program = root.join("missing.exe");
        tabs.set_profiles(std::slice::from_ref(&missing));
        controller.restart(
            ShellKind::Custom(profile.id),
            Some(&root),
            size,
            application.clone(),
        );
        assert_eq!(controller.shell(), ShellKind::Custom(profile.id));
        assert_eq!(controller.short_label(), missing.name);
        assert!(matches!(
            controller.snapshot().status,
            TerminalStatus::Running
        ));
        assert!(
            controller
                .snapshot()
                .notice
                .unwrap()
                .contains("Custom shell is unavailable")
        );
        assert_eq!(tabs.test_profiles(), vec![missing]);
        controller.terminate(&application);

        // Repairing discovery must launch the configured profile again, rather
        // than preserving the shell that was temporarily used as a fallback.
        tabs.set_profiles(std::slice::from_ref(&profile));
        controller.restart(
            ShellKind::Custom(profile.id),
            Some(&root),
            size,
            application.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let output = controller.test_parser().screen().contents();
            if expected.iter().all(|arg| output.contains(arg)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "repaired custom args: {output:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(controller.shell(), ShellKind::Custom(profile.id));
        assert_eq!(controller.short_label(), profile.name);
        assert!(controller.snapshot().notice.is_none());
        assert_eq!(tabs.test_profiles(), vec![profile]);
        controller.terminate(&application);
    }

    #[cfg(windows)]
    #[test]
    fn conpty_delivers_wheels_as_native_mouse_records_and_restores_history() {
        use crate::features::terminal::input::Modifiers as KeyModifiers;
        use std::time::Instant;

        if !crate::infrastructure::terminal::profiles::shell_command(ShellKind::Bash).is_file() {
            eprintln!("Git Bash not installed; skipping native mouse round trip");
            return;
        }
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let controller = TerminalController::with_shell(ShellKind::Bash);
        let size = TerminalSize {
            rows: 32,
            cols: 120,
            pixel_width: 840,
            pixel_height: 608,
        };
        controller.restart(ShellKind::Bash, None, size, application);

        fn wait_for(controller: &TerminalController, predicate: impl Fn(&vt100::Screen) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if predicate(controller.test_parser().screen()) {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            panic!(
                "native mouse probe timed out: {:?}",
                controller.test_parser().screen().contents()
            );
        }

        let executable = std::env::current_exe()
            .unwrap()
            .display()
            .to_string()
            .replace('\\', "/")
            .replace('\'', "'\\''");
        controller.write(format!("LOOG_MOUSE_PROBE=1 '{executable}' --exact app::terminal_session_tests::tests::native_mouse_probe --ignored --nocapture\r").as_bytes());
        wait_for(&controller, |screen| {
            screen.contents().contains("MOUSE_READY")
                && screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr
        });
        assert!(controller.test_parser().screen().alternate_screen());
        assert_eq!(
            controller.test_parser().screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::AnyMotion
        );
        controller.set_mouse_position(7, 13);
        controller.wheel(1.0, KeyModifiers::empty());
        wait_for(&controller, |screen| {
            screen.contents().contains("WHEEL:up:13:7:false:false")
        });
        // Shift must bypass application reporting, even in the alternate screen.
        controller.wheel(1.0, KeyModifiers::SHIFT);
        controller.wheel(-1.0, KeyModifiers::CONTROL | KeyModifiers::ALT);
        wait_for(&controller, |screen| {
            screen.contents().contains("WHEEL:down:13:7:true:true")
        });
        controller.write(b"q");
        wait_for(&controller, |screen| {
            !screen.alternate_screen()
                && screen.mouse_protocol_mode() == vt100::MouseProtocolMode::None
        });
        controller.write(b"for ((i=1;i<=80;i++)); do printf 'history-%03d\\n' $i; done\r");
        wait_for(&controller, |screen| {
            screen.contents().contains("history-080")
        });
        // The shell still prints its next prompt after the last line; output
        // arriving after the wheel would change the scrollback being checked.
        let settled = std::cell::RefCell::new((String::new(), Instant::now()));
        wait_for(&controller, |screen| {
            let contents = screen.contents();
            let mut settled = settled.borrow_mut();
            if contents != settled.0 {
                *settled = (contents, Instant::now());
            }
            settled.1.elapsed() >= Duration::from_millis(500)
        });
        controller.wheel(1.0, KeyModifiers::empty());
        assert_eq!(controller.test_parser().screen().scrollback(), 3);
        controller.wheel(-1.0, KeyModifiers::empty());
        assert_eq!(controller.test_parser().screen().scrollback(), 0);
    }

    /// Runs inside the test's ConPTY to exercise the same Windows input API
    /// used by Codex/crossterm, rather than merely inspecting generated bytes.
    #[cfg(windows)]
    #[test]
    #[ignore = "native console helper launched by the mouse round-trip test"]
    fn native_mouse_probe() {
        if std::env::var_os("LOOG_MOUSE_PROBE").is_none() {
            return;
        }
        use std::ffi::c_void;
        #[repr(C)]
        struct InputRecord {
            kind: u16,
            padding: u16,
            data: [u32; 4],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetStdHandle(kind: u32) -> *mut c_void;
            fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
            fn ReadConsoleInputW(
                handle: *mut c_void,
                records: *mut InputRecord,
                length: u32,
                read: *mut u32,
            ) -> i32;
        }
        unsafe {
            let input = GetStdHandle((-10i32) as u32);
            let output = GetStdHandle((-11i32) as u32);
            let (mut input_mode, mut output_mode) = (0, 0);
            assert_ne!(GetConsoleMode(input, &mut input_mode), 0);
            assert_ne!(GetConsoleMode(output, &mut output_mode), 0);
            assert_ne!(SetConsoleMode(output, output_mode | 4), 0);
            assert_ne!(SetConsoleMode(input, (input_mode | 0x98) & !0x247), 0);
            print!("\x1b[?1049h\x1b[?1006hMOUSE_READY\r\n");
            std::io::stdout().flush().unwrap();
            let mut wheels = 0;
            loop {
                let mut record = InputRecord {
                    kind: 0,
                    padding: 0,
                    data: [0; 4],
                };
                let mut read = 0;
                assert_ne!(ReadConsoleInputW(input, &mut record, 1, &mut read), 0);
                if record.kind == 2 && record.data[3] == 4 {
                    let x = record.data[0] as u16;
                    let y = (record.data[0] >> 16) as u16;
                    let delta = (record.data[1] >> 16) as i16;
                    let alt = record.data[2] & 3 != 0;
                    let ctrl = record.data[2] & 12 != 0;
                    let direction = if delta > 0 { "up" } else { "down" };
                    println!("WHEEL:{direction}:{x}:{y}:{alt}:{ctrl}");
                    std::io::stdout().flush().unwrap();
                    wheels += 1;
                } else if wheels >= 2 && record.kind == 1 && record.data[0] != 0 {
                    break;
                }
            }
            print!("\x1b[?1006l\x1b[?1049l");
            std::io::stdout().flush().unwrap();
            assert_ne!(SetConsoleMode(input, input_mode), 0);
            assert_ne!(SetConsoleMode(output, output_mode), 0);
        }
    }

    #[test]
    fn saved_directories_stay_with_independent_tabs_when_siblings_close() {
        let mut tabs = TerminalTabs::restored(vec![ShellKind::Cmd; 3], Some(1));
        let dirs = (0..3)
            .map(|i| Some(env::temp_dir().join(format!("cwd-{i}"))))
            .collect::<Vec<_>>();
        tabs.restore_directories(&dirs);
        assert_eq!(tabs.directories(), dirs);
        let ids = tabs.tabs().iter().map(|t| t.id).collect::<Vec<_>>();
        let changed = env::temp_dir().join("changed-live-directory");
        *tabs.tabs()[1].controller.test_controller().test_cwd() = Some(changed.clone());
        assert_eq!(
            tabs.region_view(&[ids[2], ids[1]], Some(ids[1]))
                .directories(),
            vec![dirs[2].clone(), Some(changed.clone())]
        );
        tabs.close(ids[0]);
        assert_eq!(tabs.directories(), vec![Some(changed), dirs[2].clone()]);
        let mut legacy = TerminalTabs::restored(tabs.shells(), tabs.active_index());
        legacy.restore_directories(&[]);
        assert_eq!(legacy.directories(), vec![None, None]);
    }

    #[cfg(windows)]
    #[test]
    fn real_shell_directories_follow_cd_and_survive_session_restoration() {
        use crate::app::workspace_session::{WorkspaceSession, write_json};
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        let root = env::temp_dir().join(format!(
            "loog-cwd-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let initial = root.join("initial");
        let changed = root.join("中文 spaces & quote'");
        std::fs::create_dir_all(&initial).unwrap();
        std::fs::create_dir_all(&changed).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let size = TerminalSize {
            rows: 24,
            cols: 160,
            pixel_width: 1120,
            pixel_height: 456,
        };

        fn wait_for_directory(controller: &TerminalController, expected: &Path, shell: ShellKind) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if controller
                    .working_directory()
                    .and_then(|p| p.canonicalize().ok())
                    == expected.canonicalize().ok()
                {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            let output = controller.test_parser().screen().contents();
            panic!(
                "{} cwd={:?} status={:?} output={output:?}",
                shell.label(),
                controller.working_directory(),
                controller.snapshot().status
            );
        }

        for shell in ShellKind::ALL {
            if !crate::infrastructure::terminal::profiles::shell_command(shell).is_file() {
                eprintln!("{} not installed; skipping real shell test", shell.label());
                continue;
            }
            let mut tabs = TerminalTabs::restored(vec![], None);
            let first = tabs.add_at(shell, Some(initial.clone()));
            let controller = tabs.active().unwrap().controller.test_controller().clone();
            controller.restart(shell, Some(&initial), size, application.clone());
            let command = match shell {
                ShellKind::PowerShell => format!(
                    "Set-Location -LiteralPath '{}'\r",
                    changed.display().to_string().replace('\'', "''")
                ),
                ShellKind::Cmd => format!("cd /d \"{}\"\r", changed.display()),
                ShellKind::Bash => format!(
                    "cd -- '{}'\r",
                    changed
                        .display()
                        .to_string()
                        .replace('\\', "/")
                        .replace('\'', "'\\''")
                ),
                ShellKind::System | ShellKind::Custom(_) | ShellKind::Wsl(_) => {
                    unreachable!("Windows builtin test")
                }
            };
            controller.write(command.as_bytes());
            wait_for_directory(&controller, &changed, shell);
            // Inheritance must read the controller immediately, before any UI poll.
            assert_eq!(tabs.directories(), vec![controller.working_directory()]);
            let second = tabs.split(first).unwrap();
            assert_eq!(tabs.tabs()[1].cwd, controller.working_directory());
            let session = WorkspaceSession {
                terminal_tabs: tabs.shells(),
                terminal_groups: tabs.saved_groups(),
                terminal_directories: tabs.directories(),
                active_terminal: tabs.active_index(),
                ..Default::default()
            };
            let path = root.join("workspace.json");
            write_json(&path, &session).unwrap();
            let decoded: WorkspaceSession =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            controller.terminate(&application);
            let mut restored = TerminalTabs::restored_with_groups(
                decoded.terminal_tabs,
                decoded.active_terminal,
                &decoded.terminal_groups,
            );
            restored.restore_directories(&decoded.terminal_directories);
            assert_eq!(restored.active_id(), Some(second));
            for tab in restored.tabs() {
                let cwd = tab.working_directory().unwrap();
                tab.controller.test_controller().restart(
                    shell,
                    Some(&cwd),
                    size,
                    application.clone(),
                );
                // Require a new real prompt report, not just the requested spawn cwd.
                *tab.controller.test_controller().test_cwd() = None;
                tab.controller.test_controller().write(b"\r");
                wait_for_directory(&tab.controller.test_controller(), &changed, shell);
                tab.controller.test_controller().terminate(&application);
            }
        }
    }

    #[test]
    fn tab_names_show_the_purpose_then_the_running_program_alone_then_the_shell() {
        let mut tabs =
            TerminalTabs::restored(vec![ShellKind::Bash, ShellKind::Bash, ShellKind::Cmd], None);
        let first = tabs.tabs()[0].id;
        assert_eq!(
            tabs.tabs()
                .iter()
                .map(TerminalTab::label)
                .collect::<Vec<_>>(),
            vec!["bash", "bash", "cmd"]
        );
        tabs.update_programs(&[(first, Some("node".into()))]);
        assert_eq!(tabs.tabs()[0].label(), "node");
        tabs.set_purpose(first, Some("  Testing  "));
        assert_eq!(tabs.tabs()[0].label(), "bash · Testing");
        tabs.update_programs(&[(first, Some("python".into()))]);
        assert_eq!(tabs.tabs()[0].label(), "bash · Testing");
        tabs.set_purpose(first, None);
        assert_eq!(tabs.tabs()[0].label(), "python");
        tabs.update_programs(&[(first, None)]);
        assert_eq!(tabs.tabs()[0].label(), "bash");
        tabs.set_purpose(first, Some("\r\n "));
        assert_eq!(tabs.tabs()[0].label(), "bash");
    }

    #[test]
    fn purposes_survive_restoration_moves_and_sibling_closure_without_saving_programs() {
        let mut tabs = TerminalTabs::new();
        let first = tabs.active_id().unwrap();
        tabs.set_purpose(first, Some("Server"));
        tabs.update_programs(&[(first, Some("node".into()))]);
        let second = tabs.split(first).unwrap();
        assert_eq!(tabs.tabs()[1].purpose, None);
        tabs.set_purpose(second, Some("Testing"));
        let mut restored = TerminalTabs::restored_with_groups(
            tabs.shells(),
            tabs.active_index(),
            &tabs.saved_groups(),
        );
        restored.restore_purposes(&tabs.purposes());
        assert_eq!(restored.purposes(), tabs.purposes());
        assert!(
            restored
                .tabs()
                .iter()
                .all(|tab| tab.running_program.is_none())
        );
        restored.detach_group(second);
        assert_eq!(
            restored
                .region_view(&[second], Some(second))
                .active()
                .unwrap()
                .label(),
            format!("{} · Testing", ShellKind::default().short_label())
        );
        restored.close(first);
        assert_eq!(restored.purposes(), vec![Some("Testing".into())]);
    }

    #[cfg(windows)]
    #[test]
    fn shell_cwd_drops_the_verbatim_prefix() {
        assert_eq!(
            shell_friendly_path(Path::new(r"\\?\D:\Documents\loog")),
            PathBuf::from(r"D:\Documents\loog")
        );
        assert_eq!(
            shell_friendly_path(Path::new(r"\\?\UNC\server\share\dir")),
            PathBuf::from(r"\\server\share\dir")
        );
        assert_eq!(
            shell_friendly_path(Path::new(r"D:\plain")),
            PathBuf::from(r"D:\plain")
        );
        assert_eq!(
            shell_friendly_path(Path::new(r"\\?\Volume{abc}\dir")),
            PathBuf::from(r"\\?\Volume{abc}\dir")
        );
    }

    #[test]
    fn splitting_keeps_independent_sessions_in_one_group_and_close_keeps_siblings() {
        let mut tabs = TerminalTabs::new();
        let first = tabs.active_id().unwrap();
        let other = tabs.add(ShellKind::Cmd);
        let second = tabs.split(first).unwrap();
        let third = tabs.split(second).unwrap();
        assert_eq!(
            tabs.active_group()
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            vec![first, second, third]
        );
        assert_eq!(tabs.saved_groups(), vec![0, 0, 0, 1]);
        assert!(
            !tabs.tabs()[0]
                .controller
                .test_controller()
                .test_same_runtime(&tabs.tabs()[1].controller.test_controller())
        );
        tabs.select(first);
        tabs.close(first);
        assert_eq!(tabs.active_id(), Some(second));
        tabs.close(second);
        assert_eq!(tabs.active_id(), Some(third));
        assert_eq!(tabs.saved_groups(), vec![0, 1]);
        tabs.close(third);
        assert_eq!(tabs.active_id(), Some(other));
        assert!(tabs.split(999).is_none());
    }

    #[test]
    fn moving_a_group_leader_preserves_controller_cwd_scrollback_and_selection() {
        use crate::domain::application_layout::{ApplicationLayout, Placement, Visibility};
        use crate::model::pane_layout::Direction;
        use lgui::prelude::UiRect;
        let mut tabs = TerminalTabs::restored(vec![], None);
        let cwd = PathBuf::from("existing-workspace");
        let first = tabs.add_at(ShellKind::Cmd, Some(cwd.clone()));
        let sibling = tabs.split(first).unwrap();
        let controller = tabs.tabs()[0].controller.test_controller().clone();
        for _ in 0..30 {
            controller.test_parser().process(b"preserved output\r\n");
        }
        controller.scroll(3);
        controller.begin_selection(0, 0);
        controller.extend_selection(0, 9);
        let selected = controller.selected_text();
        let selection = controller.test_selection();
        let scrollback = controller.test_parser().screen().scrollback();
        let generation = controller.test_generation();
        let rect = UiRect::new(0.0, 0.0, 1200.0, 800.0);
        let visible = Visibility {
            git: true,
            files: true,
            terminal: true,
        };
        let mut layout = ApplicationLayout::initial(rect.into(), 200.0, 200.0, 240.0);
        layout.sync(&[(first, first), (sibling, first)], Some(first));
        let plan = layout
            .plan(
                first,
                &Placement::Side(1, Direction::Right),
                rect.into(),
                visible,
                vec![first, sibling],
            )
            .unwrap();
        let destination = plan.destination;
        assert!(layout.commit(plan, rect.into(), visible, &[first, sibling]));
        tabs.detach_group(first);
        tabs.select(first);
        let (sessions, active) = layout.terminal_view(destination).unwrap();
        let view = tabs.region_view(&sessions, active);
        assert!(controller.test_same_runtime(&view.active().unwrap().controller.test_controller()));
        assert_eq!(view.active().unwrap().cwd, Some(cwd));
        assert_eq!(
            view.active()
                .unwrap()
                .controller
                .test_controller()
                .test_generation(),
            generation
        );
        assert_ne!(tabs.tabs()[0].group_id, tabs.tabs()[1].group_id);
        let original_size = controller.test_parser().screen().size();
        controller.resize(TerminalSize {
            rows: 12,
            cols: 60,
            pixel_width: 440,
            pixel_height: 230,
        });
        let displaced = usize::from(original_size.0 - 12);
        assert_eq!(
            controller.test_parser().screen().scrollback(),
            scrollback + displaced
        );
        assert_eq!(controller.selected_text(), selected);
        controller.resize(TerminalSize {
            rows: original_size.0,
            cols: original_size.1,
            pixel_width: 7,
            pixel_height: 19,
        });
        assert_eq!(controller.test_selection(), selection);
        assert_eq!(controller.test_parser().screen().scrollback(), scrollback);
        assert_eq!(controller.selected_text(), selected);
        // Newly created sessions join the remaining default tab strip.
        let third = tabs.split(sibling).unwrap();
        layout.sync(
            &tabs
                .tabs()
                .iter()
                .map(|t| (t.id, t.group_id))
                .collect::<Vec<_>>(),
            Some(third),
        );
        assert_eq!(
            layout.terminal_region(third),
            layout.terminal_region(sibling)
        );
        assert!(layout.terminal_region(third).is_some());
        assert_ne!(layout.terminal_region(third), layout.terminal_region(first));
        let ordered = tabs.region_view(&[sibling, first], Some(first));
        assert_eq!(
            ordered.tabs().iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![sibling, first]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "manual Windows ConPTY process-name smoke test"]
    fn builtin_shells_are_idle_then_detect_and_clear_programs() {
        use crate::features::terminal::program::SustainedProgram;
        use crate::infrastructure::terminal::program::ProgramProbe;
        use std::time::{Duration, Instant};

        for shell in ShellKind::ALL {
            let controller = TerminalController::with_shell(shell);
            let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
            controller.restart(
                shell,
                None,
                TerminalSize {
                    rows: 12,
                    cols: 80,
                    pixel_width: 560,
                    pixel_height: 228,
                },
                application.clone(),
            );
            let mut idle_tracker = SustainedProgram::default();
            let mut idle_labels = Vec::new();
            for _ in 0..15 {
                let now = Instant::now();
                let candidate = controller
                    .program_candidate(&ProgramProbe::new(), idle_tracker.sustained_pid(now));
                if let Some(label) = idle_tracker.observe(candidate, now) {
                    idle_labels.push(label);
                }
                thread::sleep(Duration::from_millis(100));
            }
            let command: &[u8] = match shell {
                ShellKind::PowerShell => b"ping.exe -n 4 127.0.0.1 | Out-Null\r",
                ShellKind::Cmd => b"ping.exe -n 4 127.0.0.1 >nul\r",
                ShellKind::Bash => b"ping.exe -n 4 127.0.0.1 >/dev/null\r",
                ShellKind::System | ShellKind::Custom(_) | ShellKind::Wsl(_) => {
                    unreachable!("Windows builtin test")
                }
            };
            controller.write(command);
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut tracker = SustainedProgram::default();
            let mut saw_program = false;
            let mut returned_to_idle = false;
            while Instant::now() < deadline {
                let probe = ProgramProbe::new();
                let candidate =
                    controller.program_candidate(&probe, tracker.sustained_pid(Instant::now()));
                let name = tracker.observe(candidate.clone(), Instant::now());
                if name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case("ping"))
                {
                    saw_program = true;
                }
                if saw_program && candidate.is_none() && name.is_none() {
                    returned_to_idle = true;
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            controller.terminate(&application);
            assert!(
                idle_labels.is_empty(),
                "{} was busy at its prompt: {idle_labels:?}",
                shell.label()
            );
            assert!(
                saw_program,
                "{} did not identify its sustained application",
                shell.label()
            );
            assert!(
                returned_to_idle,
                "{} did not clear its application after exit",
                shell.label()
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "manual Windows WSL transport regression"]
    fn real_wsl_prompt_is_not_reported_as_a_windows_foreground_program() {
        use crate::infrastructure::terminal::program::ProgramProbe;
        use std::time::Instant;
        let Some(option) = crate::infrastructure::terminal::profiles::options(&[])
            .into_iter()
            .find(|option| matches!(option.kind, ShellKind::Wsl(_)))
        else {
            return;
        };
        let controller = TerminalController::with_shell(option.kind);
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        controller.restart(
            option.kind,
            None,
            TerminalSize {
                rows: 12,
                cols: 80,
                pixel_width: 560,
                pixel_height: 228,
            },
            application.clone(),
        );
        controller.write(b"printf '%s%s\\n' 'loog-' 'wsl-ready'\r");
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut ready = false;
        while Instant::now() < deadline {
            ready = controller
                .test_parser()
                .screen()
                .contents()
                .contains("loog-wsl-ready");
            if ready {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let mut candidates = Vec::new();
        for _ in 0..15 {
            if let Some(candidate) = controller.program_candidate(&ProgramProbe::new(), None) {
                candidates.push(candidate);
            }
            thread::sleep(Duration::from_millis(100));
        }
        controller.terminate(&application);
        assert!(ready, "WSL did not execute the test command");
        assert!(
            candidates.is_empty(),
            "WSL transport was reported as a task: {candidates:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "manual npm script-label ConPTY regression; requires local node/npm"]
    fn npm_script_labels_agree_across_powershell_and_bash_and_clear_after_exit() {
        use crate::features::terminal::activity::CommandActivity;
        use crate::features::terminal::program::SustainedProgram;
        use crate::infrastructure::terminal::program::ProgramProbe;
        use std::time::{Instant, SystemTime, UNIX_EPOCH};

        let root = env::temp_dir().join(format!(
            "loog-script-label-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"dev":"node dev.cjs"}}"#,
        )
        .unwrap();
        // The fixture exits by itself even if a shell launcher loses parentage.
        std::fs::write(
            root.join("dev.cjs"),
            "console.log('loog-script-ready');setTimeout(()=>{},2600);",
        )
        .unwrap();
        let mut results = Vec::new();
        for shell in [ShellKind::PowerShell, ShellKind::Bash] {
            let controller = TerminalController::with_shell(shell);
            let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
            controller.restart(
                shell,
                Some(&root),
                TerminalSize {
                    rows: 16,
                    cols: 100,
                    pixel_width: 700,
                    pixel_height: 304,
                },
                application.clone(),
            );
            let deadline = Instant::now() + Duration::from_secs(12);
            while controller.command_activity() != CommandActivity::Idle
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(30));
            }
            controller.write(b"npm run dev\r");
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut tracker = SustainedProgram::default();
            let mut saw_label = false;
            let mut ready = false;
            let mut cleared = false;
            while Instant::now() < deadline {
                let now = Instant::now();
                let candidate =
                    controller.program_candidate(&ProgramProbe::new(), tracker.sustained_pid(now));
                let name = tracker.observe(candidate, now);
                ready |= controller
                    .test_parser()
                    .screen()
                    .contents()
                    .contains("loog-script-ready");
                saw_label |= name.as_deref() == Some("npm");
                if saw_label
                    && controller.command_activity() == CommandActivity::Idle
                    && name.is_none()
                {
                    cleared = true;
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let output = controller.test_parser().screen().contents();
            // Move the live shell out before releasing ConPTY; Windows may
            // otherwise keep its current directory locked briefly during exit.
            controller.write(b"cd ..\r");
            let deadline = Instant::now() + Duration::from_secs(2);
            while controller.working_directory().as_deref() == Some(root.as_path())
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(30));
            }
            controller.terminate(&application);
            results.push((shell.label(), ready, saw_label, cleared, output));
        }
        for file in ["package.json", "dev.cjs"] {
            std::fs::remove_file(root.join(file)).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match std::fs::remove_dir(&root) {
                Ok(()) => break,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(30)),
                Err(error) => panic!("fixture cleanup: {error}; results: {results:?}"),
            }
        }
        for (shell, ready, label, cleared, output) in results {
            eprintln!("{shell}: ready={ready}, npm={label}, cleared={cleared}");
            assert!(
                ready && label && cleared,
                "{shell}: ready={ready}, npm={label}, cleared={cleared}\n{output}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "manual command lifecycle ConPTY regression"]
    fn real_command_activity_starts_and_finishes_for_powershell_and_bash() {
        use crate::features::terminal::activity::CommandActivity;
        use std::time::Instant;
        for shell in [ShellKind::PowerShell, ShellKind::Bash] {
            let controller = TerminalController::with_shell(shell);
            let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
            controller.restart(
                shell,
                None,
                TerminalSize {
                    rows: 12,
                    cols: 80,
                    pixel_width: 560,
                    pixel_height: 228,
                },
                application.clone(),
            );
            let idle_deadline = Instant::now() + Duration::from_secs(10);
            while controller.command_activity() != CommandActivity::Idle
                && Instant::now() < idle_deadline
            {
                thread::sleep(Duration::from_millis(30));
            }
            let was_idle = controller.command_activity() == CommandActivity::Idle;
            controller.write(b"ping.exe -n 4 127.0.0.1\r");
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut running = false;
            let mut finished = false;
            while Instant::now() < deadline {
                let activity = controller.command_activity();
                running |= activity == CommandActivity::Running;
                if running && activity == CommandActivity::Idle {
                    finished = true;
                    break;
                }
                thread::sleep(Duration::from_millis(30));
            }
            controller.terminate(&application);
            assert!(was_idle, "{} did not report its idle prompt", shell.label());
            assert!(
                running && finished,
                "{} lifecycle did not start and finish",
                shell.label()
            );
            assert_eq!(controller.command_activity(), CommandActivity::Unknown);
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "manual Windows ConPTY smoke test"]
    fn supported_shells_round_trip_through_real_conpty_sessions() {
        assert_shell_round_trip(ShellKind::PowerShell, b"Write-Output (6*7)\r", "42");
        assert_shell_round_trip(
            ShellKind::PowerShell,
            "Write-Output '中文输入测试'\r".as_bytes(),
            "中文输入测试",
        );
        assert_shell_round_trip(ShellKind::Cmd, b"set /a 6*7\r", "42");
        assert_shell_round_trip(ShellKind::Bash, b"printf '%s\\n' $((6*7))\r", "42");
    }

    #[cfg(target_os = "windows")]
    fn assert_shell_round_trip(shell: ShellKind, command: &[u8], expected: &str) {
        use std::time::{Duration, Instant};

        let controller = TerminalController::new();
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let size = TerminalSize {
            rows: 12,
            cols: 80,
            pixel_width: 7,
            pixel_height: 19,
        };
        controller.restart(
            shell,
            env::current_dir().ok().as_deref(),
            size,
            application.clone(),
        );
        controller.write(command);

        let deadline = Instant::now() + Duration::from_secs(8);
        let mut saw_answer = false;
        let mut last_contents = String::new();
        let mut last_status = TerminalStatus::Idle;
        while Instant::now() < deadline {
            let snapshot = controller.snapshot();
            last_status = snapshot.status;
            last_contents = snapshot
                .lines
                .into_iter()
                .flatten()
                .map(|run| run.text)
                .collect::<String>();
            if last_contents.contains(expected) {
                saw_answer = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        controller.terminate(&application);
        assert!(
            saw_answer,
            "{} did not return {expected:?}; status={last_status:?}; contents={last_contents:?}",
            shell.label(),
        );
    }
}

#[cfg(test)]
mod keyboard_tests {
    use crate::app::terminal_runtime::TerminalDisplay;
    use crate::infrastructure::terminal::session::TerminalController;
    use lgui::core::{KeyModifiers, KeyState, KeyboardEvent, LogicalKey, NamedKey, PhysicalKey};

    #[test]
    fn win32_input_negotiation_survives_chunks_clear_and_resize() {
        for split in 0..=8 {
            let controller = TerminalController::new();
            let enable = b"\x1b[?9001h";
            controller.test_parser().process(&enable[..split]);
            controller.test_parser().process(&enable[split..]);
            assert!(controller.snapshot().win32_input_mode);
            controller.resize(crate::features::terminal::TerminalSize {
                rows: 24,
                cols: 60,
                pixel_width: 420,
                pixel_height: 456,
            });
            assert!(controller.snapshot().win32_input_mode);
            let formatted = controller.test_parser().screen().input_mode_formatted();
            let mut restored = vt100::Parser::default();
            restored.process(&formatted);
            assert!(restored.screen().win32_input_mode());
            let previous = restored.screen().clone();
            restored.process(b"\x1b[?9001l");
            let diff = restored.screen().input_mode_diff(&previous);
            let mut replay = vt100::Parser::default();
            replay.process(&formatted);
            replay.process(&diff);
            assert!(!replay.screen().win32_input_mode());
            controller.clear_output();
            assert!(controller.snapshot().win32_input_mode);
            controller.test_parser().process(b"\x1bc");
            assert!(!controller.snapshot().win32_input_mode);
        }
    }

    #[test]
    fn user_unbinding_releases_reserved_keys_to_the_terminal() {
        use crate::features::keymap::{
            bindings::BindingSource, dispatcher::dispatch_key, keymap_file, keystroke::Keystroke,
        };
        let parsed = keymap_file::parse(
            r#"[{"context":"Terminal","bindings":{"ctrl-b":null}}]"#,
            BindingSource::User,
        )
        .unwrap();
        assert!(parsed.errors.is_empty());
        let keymap = keymap_file::build(parsed.bindings);
        let mut app = crate::app::state::AppState::new();
        app.terminal.ui.terminal_focused = true;
        let stack = crate::app::keymap_context::context_stack(&app);
        for (key, expected) in [("b", 2), ("g", 7), ("p", 16)] {
            let result = dispatch_key(
                &keymap,
                vec![],
                Keystroke::parse(&format!("ctrl-{key}")).unwrap(),
                &stack,
            );
            assert!(result.is_unbound());
            let event = KeyboardEvent {
                state: KeyState::Down,
                key: LogicalKey::Character(key.into()),
                modifiers: KeyModifiers::CONTROL,
                ..Default::default()
            };
            assert_eq!(
                crate::ui::terminal::keys::key_bytes_in_mode(&event, false, false),
                Some(vec![expected])
            );
        }
        let default = keymap_file::default_keymap();
        if !cfg!(target_os = "macos") {
            assert!(
                !dispatch_key(
                    &default,
                    vec![],
                    Keystroke::parse("ctrl-b").unwrap(),
                    &stack
                )
                .is_unbound()
            );
        }
    }

    #[test]
    fn release_tracking_excludes_shortcuts_and_preserves_session_ownership() {
        use crate::ui::terminal::keys::PressedKeys;
        let mut keys = PressedKeys::default();
        let mut event = KeyboardEvent {
            state: KeyState::Down,
            key: LogicalKey::Named(NamedKey::Enter),
            code: PhysicalKey::Enter,
            modifiers: KeyModifiers::SHIFT,
            ..Default::default()
        };
        assert!(keys.release(&event).is_empty());
        keys.press(1, &event);
        keys.press(1, &event);
        keys.press(2, &event);
        event.state = KeyState::Up;
        let releases = keys.release(&event);
        assert_eq!(
            releases
                .iter()
                .map(|(session, _)| *session)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(
            releases
                .iter()
                .all(|(_, event)| event.state == KeyState::Up)
        );
        assert!(keys.release(&event).is_empty());
        keys.press(1, &event);
        let releases = keys.release_all();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].1.state, KeyState::Up);
        assert!(releases[0].1.modifiers.is_empty());
        assert!(keys.release_all().is_empty());
    }

    #[test]
    fn modifier_and_release_records_preserve_selection_for_copy() {
        let controller = TerminalController::new();
        controller.test_parser().process(b"selected");
        controller.begin_selection(0, 0);
        controller.extend_selection(0, 8);
        assert_eq!(controller.selected_text().as_deref(), Some("selected"));
        for state in [KeyState::Down, KeyState::Up] {
            let event = KeyboardEvent {
                state,
                key: LogicalKey::Named(NamedKey::Control),
                code: PhysicalKey::ControlLeft,
                modifiers: KeyModifiers::CONTROL,
                ..Default::default()
            };
            assert!(crate::ui::terminal::keys::is_modifier(&event));
            controller.write_protocol(
                &crate::ui::terminal::keys::key_bytes_in_mode(&event, false, true).unwrap(),
            );
            assert_eq!(controller.selected_text().as_deref(), Some("selected"));
        }
        controller.write(b"x");
        assert_eq!(controller.selected_text(), None);
    }

    #[cfg(windows)]
    #[test]
    fn conpty_receives_modified_keys_as_native_console_records() {
        use crate::features::terminal::{ShellKind, TerminalSize, profiles::ShellProfile};
        use std::{
            sync::Arc,
            time::{Duration, Instant},
        };
        let controller = TerminalController::with_shell(ShellKind::Custom(90001));
        controller.set_profile(Some(ShellProfile {
            id: 90001,
            name: "Keyboard probe".into(),
            program: std::env::current_exe().unwrap(),
            args: vec![
                "--exact".into(),
                "app::terminal_session_tests::keyboard_tests::native_keyboard_probe".into(),
                "--ignored".into(),
                "--nocapture".into(),
            ],
        }));
        let application = Arc::new(lgui::ApplicationHandle::new(|task| task(), || {}));
        struct Cleanup(TerminalController, Arc<lgui::ApplicationHandle>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.terminate(self.1.clone());
            }
        }
        let _cleanup = Cleanup(controller.clone(), application.clone());
        controller.restart(
            ShellKind::Custom(90001),
            None,
            TerminalSize {
                rows: 32,
                cols: 120,
                pixel_width: 840,
                pixel_height: 608,
            },
            application,
        );
        let wait_for = |needle: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let contents = controller.test_parser().screen().contents();
                if contents.contains(needle) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "waiting for {needle}: {contents:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait_for("KEY_READY");
        assert!(controller.snapshot().win32_input_mode);
        for (key, code, modifiers, expected) in [
            (
                LogicalKey::Named(NamedKey::Enter),
                PhysicalKey::Enter,
                KeyModifiers::SHIFT,
                "KEY:13:13:16:1",
            ),
            (
                LogicalKey::Named(NamedKey::Enter),
                PhysicalKey::Enter,
                KeyModifiers::CONTROL,
                "KEY:13:13:8:1",
            ),
            (
                LogicalKey::Character("j".into()),
                PhysicalKey::KeyJ,
                KeyModifiers::CONTROL,
                "KEY:74:10:8:1",
            ),
            (
                LogicalKey::Character("g".into()),
                PhysicalKey::KeyG,
                KeyModifiers::CONTROL,
                "KEY:71:7:8:1",
            ),
            (
                LogicalKey::Character("p".into()),
                PhysicalKey::KeyP,
                KeyModifiers::CONTROL,
                "KEY:80:16:8:1",
            ),
            (
                LogicalKey::Named(NamedKey::Enter),
                PhysicalKey::Enter,
                KeyModifiers::ALT,
                "KEY:13:13:2:1",
            ),
            (
                LogicalKey::Character(" ".into()),
                PhysicalKey::Space,
                KeyModifiers::CONTROL,
                "KEY:32:0:8:1",
            ),
        ] {
            let mut event = KeyboardEvent {
                state: KeyState::Down,
                key,
                code,
                modifiers,
                ..Default::default()
            };
            let bytes = crate::ui::terminal::keys::key_bytes_in_mode(&event, false, true).unwrap();
            controller.write(&bytes);
            wait_for(expected);
            event.state = KeyState::Up;
            controller
                .write(&crate::ui::terminal::keys::key_bytes_in_mode(&event, false, true).unwrap());
            wait_for(&format!("{}0", &expected[..expected.len() - 1]));
        }
        controller.write(b"z");
        wait_for("KEY:90:122:0:1");
        let event = KeyboardEvent {
            state: KeyState::Down,
            key: LogicalKey::Named(NamedKey::F12),
            code: PhysicalKey::F12,
            ..Default::default()
        };
        controller
            .write(&crate::ui::terminal::keys::key_bytes_in_mode(&event, false, true).unwrap());
        wait_for("KEY_DONE");
    }

    /// A child of the native round-trip test, isolated from the user's sessions.
    #[cfg(windows)]
    #[test]
    #[ignore = "console helper launched by the keyboard round-trip test"]
    fn native_keyboard_probe() {
        use std::{ffi::c_void, io::Write};
        #[repr(C)]
        struct InputRecord {
            kind: u16,
            padding: u16,
            data: [u32; 4],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetStdHandle(kind: u32) -> *mut c_void;
            fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
            fn ReadConsoleInputW(
                handle: *mut c_void,
                records: *mut InputRecord,
                length: u32,
                read: *mut u32,
            ) -> i32;
        }
        unsafe {
            let input = GetStdHandle((-10i32) as u32);
            let output = GetStdHandle((-11i32) as u32);
            let (mut input_mode, mut output_mode) = (0, 0);
            assert_ne!(GetConsoleMode(input, &mut input_mode), 0);
            assert_ne!(GetConsoleMode(output, &mut output_mode), 0);
            assert_ne!(SetConsoleMode(output, output_mode | 4), 0);
            assert_ne!(SetConsoleMode(input, input_mode & !0x207), 0);
            print!("\x1b[?9001hKEY_READY\r\n");
            std::io::stdout().flush().unwrap();
            loop {
                let mut record = InputRecord {
                    kind: 0,
                    padding: 0,
                    data: [0; 4],
                };
                let mut read = 0;
                assert_ne!(ReadConsoleInputW(input, &mut record, 1, &mut read), 0);
                if record.kind != 1 {
                    continue;
                }
                let key = record.data[1] >> 16;
                let character = record.data[2] >> 16;
                println!(
                    "KEY:{key}:{character}:{}:{}",
                    record.data[3], record.data[0]
                );
                std::io::stdout().flush().unwrap();
                if key == 123 && record.data[0] == 1 {
                    break;
                }
            }
            print!("\x1b[?9001lKEY_DONE\r\n");
            std::io::stdout().flush().unwrap();
            assert_ne!(SetConsoleMode(input, input_mode), 0);
            assert_ne!(SetConsoleMode(output, output_mode), 0);
        }
    }
}
