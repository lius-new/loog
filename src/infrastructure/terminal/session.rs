//! PTY controller and worker lifecycle; GUI notifications are injected.
use crate::features::terminal::input::paste_bytes;
use crate::features::terminal::screen::{
    GridPoint, Palette, Selection, find_matches, line_runs, selection_text, visible_selection,
};
use crate::features::terminal::{
    SearchStatus, ShellKind, TerminalSize, TerminalSnapshot, TerminalStatus,
};
use crate::infrastructure::terminal::notification::FrameNotifier;
use crate::infrastructure::terminal::pty::{
    RunningProcess, resize_process, spawn_shell_option, terminate_process,
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
#[cfg(windows)]
use std::time::Duration;
const SCROLLBACK_ROWS: usize = 2_000;

/// Contents of every cell in a visible row of `screen`.
fn screen_cells(screen: &vt100::Screen, row: u16) -> Vec<String> {
    let (_, cols) = screen.size();
    (0..cols)
        .map(|c| {
            screen
                .cell(row, c)
                .map(|cell| cell.contents().to_owned())
                .unwrap_or_default()
        })
        .collect()
}

#[derive(Clone)]
pub struct TerminalController {
    inner: Arc<TerminalInner>,
}

struct TerminalInner {
    lifecycle: Mutex<()>,
    #[cfg(test)]
    test_writes: Mutex<Vec<Vec<u8>>>,
    disposed: AtomicBool,
    parser: Mutex<vt100::Parser>,
    process: Mutex<Option<RunningProcess>>,
    shell: Mutex<ShellKind>,
    detected_name: Mutex<Option<String>>,
    status: Mutex<TerminalStatus>,
    selection: Mutex<Option<Selection>>,
    /// Time, cell and count of the last left press, for double/triple click.
    last_click: Mutex<Option<(std::time::Instant, u16, u16, u8)>>,
    /// Row and cell under the pointer while Ctrl is held, for link underline.
    link_hover: Mutex<Option<(u16, u16)>>,
    /// The text being searched for and the match shown, counted from 0.
    search: Mutex<(String, usize)>,
    mouse: Mutex<crate::features::terminal::mouse::TerminalMouse>,
    generation: AtomicU64,
    cwd: Mutex<Option<PathBuf>>,
    profile: Mutex<Option<crate::features::terminal::profiles::ShellProfile>>,
    notice: Mutex<Option<String>>,
}

impl TerminalController {
    pub fn new() -> Self {
        Self::with_shell(ShellKind::default())
    }

    pub fn with_shell(shell: ShellKind) -> Self {
        Self {
            inner: Arc::new(TerminalInner {
                lifecycle: Mutex::new(()),
                #[cfg(test)]
                test_writes: Mutex::new(Vec::new()),
                disposed: AtomicBool::new(false),
                parser: Mutex::new(vt100::Parser::new(24, 80, SCROLLBACK_ROWS)),
                process: Mutex::new(None),
                shell: Mutex::new(shell),
                detected_name: Mutex::new(if matches!(shell, ShellKind::Wsl(_)) {
                    super::profiles::builtin(shell).map(|option| option.name)
                } else {
                    None
                }),
                status: Mutex::new(TerminalStatus::Idle),
                selection: Mutex::new(None),
                last_click: Mutex::new(None),
                link_hover: Mutex::new(None),
                search: Mutex::new((String::new(), 0)),
                mouse: Mutex::new(crate::features::terminal::mouse::TerminalMouse::default()),
                generation: AtomicU64::new(0),
                cwd: Mutex::new(None),
                profile: Mutex::new(None),
                notice: Mutex::new(None),
            }),
        }
    }

    pub fn ensure_started(
        &self,
        cwd: Option<&Path>,
        size: TerminalSize,
        application: impl Into<FrameNotifier>,
    ) {
        let application = application.into();
        let should_start = matches!(*lock(&self.inner.status), TerminalStatus::Idle);
        if should_start {
            self.restart(self.shell(), cwd, size, application);
        } else {
            self.resize(size);
        }
    }

    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }
    pub fn shell(&self) -> ShellKind {
        *lock(&self.inner.shell)
    }

    pub(crate) fn set_profile(
        &self,
        profile: Option<crate::features::terminal::profiles::ShellProfile>,
    ) {
        *lock(&self.inner.profile) = profile;
    }

    pub fn shell_name(&self) -> String {
        lock(&self.inner.profile)
            .as_ref()
            .map(|p| p.name.clone())
            .or_else(|| lock(&self.inner.detected_name).clone())
            .unwrap_or_else(|| self.shell().label().into())
    }

    pub fn short_label(&self) -> String {
        lock(&self.inner.profile)
            .as_ref()
            .map(|p| p.name.clone())
            .or_else(|| lock(&self.inner.detected_name).clone())
            .unwrap_or_else(|| self.shell().short_label().into())
    }

    pub fn working_directory(&self) -> Option<PathBuf> {
        lock(&self.inner.cwd).clone()
    }

    /// Inspect the OS process outside reactive UI state locks.
    pub fn program_candidate(
        &self,
        probe: &crate::infrastructure::terminal::program::ProgramProbe,
        preferred: Option<u32>,
    ) -> Option<crate::features::terminal::program::ProgramCandidate> {
        if !matches!(*lock(&self.inner.status), TerminalStatus::Running) {
            return None;
        }
        #[cfg(windows)]
        let (pid, console) = {
            let process = lock(&self.inner.process);
            let process = process.as_ref()?;
            (process.pid, process.console.clone())
        };
        #[cfg(windows)]
        {
            if let Some(console) = console
                && let Some(members) = console.members()
            {
                return probe.session_candidate(&members, &console.baseline, preferred);
            }
            probe.candidate(pid, None)
        }
        #[cfg(not(windows))]
        {
            let _ = preferred;
            let (pid, foreground) = {
                let process = lock(&self.inner.process);
                let process = process.as_ref()?;
                let foreground = process.master.process_group_leader().map(|pid| pid as u32);
                (process.pid, foreground)
            };
            probe.candidate(pid, foreground)
        }
    }

    pub fn restart(
        &self,
        shell: ShellKind,
        cwd: Option<&Path>,
        size: TerminalSize,
        application: impl Into<FrameNotifier>,
    ) {
        let application = application.into();
        let lifecycle = lock(&self.inner.lifecycle);
        if self.inner.disposed.load(Ordering::Acquire) {
            return;
        }
        let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.kill_current();
        *lock(&self.inner.shell) = shell;
        *lock(&self.inner.status) = TerminalStatus::Starting;
        *lock(&self.inner.parser) = vt100::Parser::new(size.rows, size.cols, SCROLLBACK_ROWS);
        *lock(&self.inner.selection) = None;

        let cwd = crate::infrastructure::terminal::integration::starting_directory(cwd, None);
        *lock(&self.inner.mouse) = crate::features::terminal::mouse::TerminalMouse::default();
        *lock(&self.inner.cwd) = cwd.clone();
        let selected = if matches!(shell, ShellKind::Custom(_)) {
            lock(&self.inner.profile)
                .as_ref()
                .map(crate::infrastructure::terminal::profiles::custom)
        } else {
            crate::infrastructure::terminal::profiles::builtin(shell)
        };
        let available = selected
            .clone()
            .filter(crate::features::terminal::profiles::ShellOption::available);
        *lock(&self.inner.detected_name) = selected
            .as_ref()
            .filter(|_| matches!(shell, ShellKind::Wsl(_)))
            .map(|option| option.name.clone());
        let launch = available.clone().or_else(|| {
            crate::infrastructure::terminal::profiles::defaults()
                .into_iter()
                .next()
        });
        *lock(&self.inner.notice) = if available.is_none() {
            launch.as_ref().map(|fallback| {
                format!(
                    "{} is unavailable; using {}.",
                    selected
                        .as_ref()
                        .map(|s| s.name.as_str())
                        .unwrap_or(shell.label()),
                    fallback.name
                )
            })
        } else {
            None
        };
        // The tab keeps its configured shell, so a restored session or a
        // restart after the profile is repaired launches that shell again.
        let spawned = launch
            .ok_or_else(|| "No usable shell was found. Add a terminal in Settings.".to_owned())
            .and_then(|launch| spawn_shell_option(&launch, cwd.as_deref(), size));
        match spawned {
            Ok(spawned) => {
                let mut reader = spawned.reader;
                #[cfg(not(windows))]
                let child = spawned.child;
                *lock(&self.inner.process) = Some(spawned.process);
                *lock(&self.inner.status) = TerminalStatus::Running;
                drop(lifecycle);
                application.request_frame_for(generation);

                let reader_inner = Arc::downgrade(&self.inner);
                let reader_application = application.clone();
                thread::Builder::new()
                    .name("loog-terminal-output".to_owned())
                    .spawn(move || {
                        let mut bytes = [0_u8; 8 * 1024];
                        let mut directories =
                            crate::features::terminal::directory::DirectoryReports::default();
                        loop {
                            let count = match reader.read(&mut bytes) {
                                Ok(count) => count,
                                Err(error) => {
                                    if update_current(&reader_inner, generation, |inner| {
                                        *lock(&inner.status) = TerminalStatus::Failed(format!(
                                            "terminal output stream failed: {error}"
                                        ));
                                    })
                                    .is_some()
                                    {
                                        reader_application.request_frame_for(generation);
                                    }
                                    break;
                                }
                            };
                            if count == 0 {
                                break;
                            }
                            let cwd = directories.process(&bytes[..count]);
                            if update_current(&reader_inner, generation, |inner| {
                                if let Some(cwd) = cwd {
                                    *lock(&inner.cwd) = Some(cwd);
                                }
                                lock(&inner.parser).process(&bytes[..count]);
                            })
                            .is_none()
                            {
                                break;
                            }
                            reader_application.request_frame_for(generation);
                        }
                    })
                    .expect("terminal output thread should start");

                spawn_waiter(
                    Arc::downgrade(&self.inner),
                    generation,
                    #[cfg(not(windows))]
                    child,
                    application,
                );
            }
            Err(error) => {
                *lock(&self.inner.status) = TerminalStatus::Failed(error);
                drop(lifecycle);
                application.request_frame_for(generation);
            }
        }
    }

    pub(super) fn dispose(&self) {
        let _lifecycle = lock(&self.inner.lifecycle);
        self.inner.disposed.store(true, Ordering::Release);
        self.shutdown();
    }

    fn shutdown(&self) {
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        self.kill_current();
        *lock(&self.inner.status) = TerminalStatus::Stopped;
    }
    pub fn terminate(&self, application: impl Into<FrameNotifier>) {
        {
            let _lifecycle = lock(&self.inner.lifecycle);
            self.shutdown();
        }
        application.into().request_frame();
    }

    pub fn write(&self, bytes: &[u8]) {
        #[cfg(test)]
        lock(&self.inner.test_writes).push(bytes.to_vec());
        lock(&self.inner.parser).screen_mut().set_scrollback(0);
        // Typing into the shell ends the selection, as in other terminals.
        *lock(&self.inner.selection) = None;
        let mut process = lock(&self.inner.process);
        let Some(process) = process.as_mut() else {
            return;
        };
        if let Err(error) = process
            .writer
            .write_all(bytes)
            .and_then(|_| process.writer.flush())
        {
            *lock(&self.inner.status) = TerminalStatus::Failed(error.to_string());
        }
    }

    pub fn resize(&self, size: TerminalSize) {
        {
            let mut parser = lock(&self.inner.parser);
            if parser.screen().size() != (size.rows, size.cols) {
                let displacement = parser.screen_mut().resize(size.rows, size.cols);
                if let Some(selection) = lock(&self.inner.selection).as_mut() {
                    selection.anchor.row += i64::from(displacement);
                    selection.head.row += i64::from(displacement);
                }
            }
        }

        let mut process = lock(&self.inner.process);
        let Some(process) = process.as_mut() else {
            return;
        };
        if process.size != size && resize_process(process, size).is_ok() {
            process.size = size;
        }
    }

    pub fn scroll(&self, rows: i32) {
        let mut parser = lock(&self.inner.parser);
        crate::features::terminal::mouse::scroll_history(parser.screen_mut(), rows);
    }

    pub fn set_scrollback(&self, rows: usize) {
        lock(&self.inner.parser).screen_mut().set_scrollback(rows);
    }

    /// Forgets the screen and its history, then asks the shell to redraw its
    /// prompt. A full-screen program on the alternate screen is left alone.
    pub fn clear_output(&self) {
        {
            let mut parser = lock(&self.inner.parser);
            if parser.screen().alternate_screen() {
                return;
            }
            let (rows, cols) = parser.screen().size();
            *parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
            *lock(&self.inner.selection) = None;
        }
        // Ctrl+L: shells redraw their prompt on a freshly cleared screen.
        self.write(&[0x0c]);
    }

    pub fn set_mouse_position(&self, row: u16, col: u16) {
        lock(&self.inner.mouse).set_position(row, col);
    }

    pub fn wheel(&self, delta: f32, modifiers: crate::features::terminal::input::Modifiers) {
        let bytes = {
            let mut parser = lock(&self.inner.parser);
            lock(&self.inner.mouse).wheel(parser.screen_mut(), delta, modifiers)
        };
        if !bytes.is_empty() {
            self.write(&bytes);
        }
    }

    /// Writes pasted text, wrapping it for shells that enabled bracketed paste.
    pub fn paste(&self, value: &str) {
        if value.is_empty() {
            return;
        }
        let bracketed = lock(&self.inner.parser).screen().bracketed_paste();
        self.write(&paste_bytes(value, bracketed));
    }

    /// Starts a selection at a visible cell boundary.
    pub fn begin_selection(&self, row: u16, col: u16) {
        let point = self.grid_point(row, col);
        *lock(&self.inner.selection) = Some(Selection {
            anchor: point,
            head: point,
        });
    }

    /// Handles a left press: shift extends the current selection, a second
    /// quick click on the same cell selects the word and a third the line.
    /// `col` is the nearest cell boundary, `cell` the cell under the pointer.
    pub fn press_selection(&self, row: u16, col: u16, cell: u16, extend: bool) {
        if extend {
            let mut selection = lock(&self.inner.selection);
            if let Some(selection) = selection.as_mut() {
                selection.head = self.grid_point(row, col);
                *lock(&self.inner.last_click) = None;
                return;
            }
        }
        let clicks = {
            let mut last = lock(&self.inner.last_click);
            let now = std::time::Instant::now();
            let count = last
                .filter(|(time, r, c, _)| {
                    *r == row && *c == cell && now.duration_since(*time).as_millis() < 500
                })
                .map_or(1, |(_, _, _, count)| count % 3 + 1);
            *last = Some((now, row, cell, count));
            count
        };
        if clicks == 1 {
            self.begin_selection(row, col);
            return;
        }
        let cells = self.row_cells(row);
        let (from, to) = if clicks == 2 {
            crate::features::terminal::screen::word_range(&cells, usize::from(cell))
        } else {
            (0, crate::features::terminal::screen::line_end(&cells))
        };
        let anchor = self.grid_point(row, from);
        let head = self.grid_point(row, to);
        *lock(&self.inner.selection) = Some(Selection { anchor, head });
    }

    /// Contents of every cell in a visible row.
    fn row_cells(&self, row: u16) -> Vec<String> {
        screen_cells(lock(&self.inner.parser).screen(), row)
    }

    /// The `http(s)` link shown at a visible cell, if any.
    pub fn url_at(&self, row: u16, cell: u16) -> Option<String> {
        crate::features::terminal::screen::url_at(&self.row_cells(row), usize::from(cell))
    }

    /// Moves the free end of the selection to a visible cell boundary.
    pub fn extend_selection(&self, row: u16, col: u16) {
        let point = self.grid_point(row, col);
        if let Some(selection) = lock(&self.inner.selection).as_mut() {
            selection.head = point;
        }
    }

    /// Starts a search and shows its last match, the one nearest the live end.
    pub fn search(&self, query: &str) -> SearchStatus {
        *lock(&self.inner.search) = (query.to_owned(), usize::MAX);
        self.show_match(0)
    }

    /// Moves to the next (`+1`) or previous (`-1`) match, wrapping around. The
    /// matches are read again, so output that arrived since is included.
    pub fn search_step(&self, delta: i32) -> SearchStatus {
        self.show_match(delta)
    }

    /// Ends the search and drops the highlighted match.
    pub fn search_clear(&self) {
        *lock(&self.inner.search) = (String::new(), 0);
        *lock(&self.inner.selection) = None;
    }

    fn show_match(&self, delta: i32) -> SearchStatus {
        let (query, current) = lock(&self.inner.search).clone();
        let mut parser = lock(&self.inner.parser);
        let matches = find_matches(parser.screen_mut(), &query);
        let total = matches.len();
        if total == 0 {
            *lock(&self.inner.selection) = None;
            *lock(&self.inner.search) = (query, 0);
            return SearchStatus::default();
        }
        // A fresh search arrives with `usize::MAX`, which lands on the last match.
        let index = if current == usize::MAX {
            total - 1
        } else {
            (current.min(total - 1) as i64 + i64::from(delta)).rem_euclid(total as i64) as usize
        };
        let (start, end) = matches[index];
        *lock(&self.inner.selection) = Some(Selection {
            anchor: start,
            head: end,
        });
        // Show the match in the middle of the view; the live screen needs no scrolling.
        let rows = i64::from(parser.screen().size().0);
        let offset = if start.row >= 0 {
            0
        } else {
            usize::try_from(rows / 2 - start.row).unwrap_or(0)
        };
        parser.screen_mut().set_scrollback(offset);
        *lock(&self.inner.search) = (query, index);
        SearchStatus {
            total,
            current: index + 1,
        }
    }

    pub fn clear_selection(&self) -> bool {
        lock(&self.inner.selection).take().is_some()
    }

    pub fn selected_text(&self) -> Option<String> {
        let mut parser = lock(&self.inner.parser);
        let (start, end) = (*lock(&self.inner.selection))?.ordered()?;
        Some(selection_text(parser.screen_mut(), start, end))
    }

    fn grid_point(&self, row: u16, col: u16) -> GridPoint {
        let parser = lock(&self.inner.parser);
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        GridPoint {
            row: i64::from(row.min(rows.saturating_sub(1))) - screen.scrollback() as i64,
            col: col.min(cols),
        }
    }

    pub fn snapshot_with_palette(&self, palette: &Palette) -> TerminalSnapshot {
        let parser = lock(&self.inner.parser);
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let lines = (0..rows)
            .map(|row| line_runs(screen, row, cols, palette))
            .collect();
        let selection = lock(&self.inner.selection)
            .and_then(Selection::ordered)
            .map(|(start, end)| visible_selection(start, end, screen.scrollback(), rows, cols))
            .unwrap_or_default();
        TerminalSnapshot {
            shell: self.shell(),
            shell_name: self.shell_name(),
            notice: lock(&self.inner.notice).clone(),
            status: lock(&self.inner.status).clone(),
            rows,
            cols,
            lines,
            scrollback: screen.scrollback(),
            scrollback_rows: screen.scrollback_rows(),
            cursor: screen.cursor_position(),
            cursor_visible: !screen.hide_cursor() && screen.scrollback() == 0,
            application_cursor: screen.application_cursor(),
            selection,
            link: (*lock(&self.inner.link_hover)).and_then(|(row, cell)| {
                let cells = screen_cells(screen, row);
                crate::features::terminal::screen::url_span(&cells, usize::from(cell))
                    .map(|(start, end, _)| (row, start, end))
            }),
        }
    }

    /// Sets the cell whose link is underlined; returns whether it changed.
    pub fn set_link_hover(&self, cell: Option<(u16, u16)>) -> bool {
        let mut hover = lock(&self.inner.link_hover);
        let changed = *hover != cell;
        *hover = cell;
        changed
    }

    fn kill_current(&self) {
        if let Some(mut process) = lock(&self.inner.process).take() {
            terminate_process(&mut process);
        }
    }
}

impl Default for TerminalController {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TerminalInner {
    fn drop(&mut self) {
        if let Ok(process) = self.process.get_mut()
            && let Some(mut process) = process.take()
        {
            terminate_process(&mut process);
        }
    }
}

#[cfg(windows)]
fn spawn_waiter(inner: Weak<TerminalInner>, generation: u64, application: FrameNotifier) {
    thread::Builder::new()
        .name("loog-terminal-wait".to_owned())
        .spawn(move || {
            loop {
                let exit = update_current(&inner, generation, |inner| {
                    let process = lock(&inner.process);
                    let process = process.as_ref()?;
                    let exit = (!process.process.is_alive()).then(|| process.process.wait(Some(0)));
                    if let Some(exit) = exit {
                        *lock(&inner.status) = match exit {
                            Ok(code) => TerminalStatus::Exited(code),
                            Err(error) => TerminalStatus::Failed(error.to_string()),
                        };
                        Some(true)
                    } else {
                        Some(false)
                    }
                });
                match exit {
                    Some(Some(false)) => {}
                    Some(Some(true)) => {
                        application.request_frame_for(generation);
                        return;
                    }
                    _ => return,
                }
                thread::sleep(Duration::from_millis(50));
            }
        })
        .expect("terminal wait thread should start");
}

#[cfg(not(windows))]
fn spawn_waiter(
    inner: Weak<TerminalInner>,
    generation: u64,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    application: FrameNotifier,
) {
    thread::Builder::new()
        .name("loog-terminal-wait".to_owned())
        .spawn(move || {
            let status = child.wait();
            if update_current(&inner, generation, |inner| {
                *lock(&inner.status) = match status {
                    Ok(status) => TerminalStatus::Exited(status.exit_code()),
                    Err(error) => TerminalStatus::Failed(error.to_string()),
                };
            })
            .is_some()
            {
                application.request_frame_for(generation);
            }
        })
        .expect("terminal wait thread should start");
}

/// Reader, cwd, error and exit events commit only to their own running generation.
fn update_current<T>(
    inner: &Weak<TerminalInner>,
    generation: u64,
    update: impl FnOnce(&TerminalInner) -> T,
) -> Option<T> {
    let inner = inner.upgrade()?;
    let _lifecycle = lock(&inner.lifecycle);
    if inner.disposed.load(Ordering::Acquire)
        || inner.generation.load(Ordering::Acquire) != generation
    {
        return None;
    }
    Some(update(&inner))
}
#[cfg(test)]
fn current_inner(inner: &Weak<TerminalInner>, generation: u64) -> Option<Arc<TerminalInner>> {
    let inner = inner.upgrade()?;
    (inner.generation.load(Ordering::Acquire) == generation).then_some(inner)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
impl TerminalController {
    pub(crate) fn test_advance_generation(&self) -> u64 {
        let _lifecycle = lock(&self.inner.lifecycle);
        self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1
    }
    pub(crate) fn test_writes(&self) -> Vec<Vec<u8>> {
        lock(&self.inner.test_writes).clone()
    }
    pub(crate) fn test_parser(&self) -> MutexGuard<'_, vt100::Parser> {
        lock(&self.inner.parser)
    }
    pub(crate) fn test_profile(
        &self,
    ) -> MutexGuard<'_, Option<crate::features::terminal::profiles::ShellProfile>> {
        lock(&self.inner.profile)
    }
    pub(crate) fn test_cwd(&self) -> MutexGuard<'_, Option<PathBuf>> {
        lock(&self.inner.cwd)
    }
    pub(crate) fn test_selection(&self) -> Option<Selection> {
        *lock(&self.inner.selection)
    }
    pub(crate) fn test_generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }
    pub(crate) fn test_same_runtime(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    #[cfg(windows)]
    pub(crate) fn test_has_console(&self) -> bool {
        lock(&self.inner.process)
            .as_ref()
            .unwrap()
            .console
            .is_some()
    }
    #[cfg(windows)]
    pub(crate) fn test_process_alive(&self) -> bool {
        lock(&self.inner.process)
            .as_ref()
            .is_some_and(|process| process.process.is_alive())
    }
    #[cfg(windows)]
    pub(crate) fn test_observer_pid(&self) -> Option<u32> {
        lock(&self.inner.process)
            .as_ref()
            .and_then(|p| p.console.as_ref().map(|c| c.test_pid()))
    }
    #[cfg(windows)]
    pub(crate) fn test_pid(&self) -> u32 {
        lock(&self.inner.process).as_ref().unwrap().pid
    }
}

#[cfg(test)]
#[cfg(target_os = "windows")]
pub(crate) fn start_launcher_backed_test_shell() -> TerminalController {
    let command = crate::infrastructure::terminal::profiles::shell_command(ShellKind::Bash);
    let program = command
        .ancestors()
        .skip(1)
        .map(|parent| parent.join("bin/bash.exe"))
        .find(|path| path != &command && path.is_file())
        .unwrap_or(command);
    let spawned = crate::infrastructure::terminal::pty::spawn_shell_program(
        ShellKind::Bash,
        None,
        TerminalSize {
            rows: 12,
            cols: 80,
            pixel_width: 560,
            pixel_height: 228,
        },
        &program,
    )
    .unwrap();
    let controller = TerminalController::with_shell(ShellKind::Bash);
    let mut output = spawned.reader;
    thread::spawn(move || {
        let _ = std::io::copy(&mut output, &mut std::io::sink());
    });
    *lock(&controller.inner.process) = Some(spawned.process);
    *lock(&controller.inner.status) = TerminalStatus::Running;
    controller
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shutdown_uses_injected_notification_and_rejects_the_previous_generation() {
        use std::sync::atomic::AtomicUsize;
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let notify = FrameNotifier::new(move || {
            observed.fetch_add(1, Ordering::AcqRel);
        });
        let controller = TerminalController::new();
        let weak = Arc::downgrade(&controller.inner);
        assert!(current_inner(&weak, 0).is_some());
        controller.terminate(notify);
        assert_eq!(count.load(Ordering::Acquire), 1);
        assert!(current_inner(&weak, 0).is_none());
        assert_eq!(*lock(&controller.inner.status), TerminalStatus::Stopped);
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    #[test]
    fn old_output_cwd_failure_and_exit_cannot_overwrite_restarted_or_closed_session() {
        let controller = TerminalController::new();
        let weak = Arc::downgrade(&controller.inner);
        let old = controller.generation();
        let new = controller.test_advance_generation();
        assert!(
            update_current(&weak, old, |inner| {
                lock(&inner.parser).process(b"stale output");
                *lock(&inner.cwd) = Some("stale cwd".into());
                *lock(&inner.status) = TerminalStatus::Failed("old stream".into());
            })
            .is_none()
        );
        assert!(
            update_current(&weak, old, |inner| *lock(&inner.status) =
                TerminalStatus::Exited(99))
            .is_none()
        );
        assert!(controller.test_parser().screen().contents().is_empty());
        assert!(controller.working_directory().is_none());
        assert!(matches!(
            *lock(&controller.inner.status),
            TerminalStatus::Idle
        ));
        assert!(
            update_current(&weak, new, |inner| lock(&inner.parser)
                .process(b"fresh output"))
            .is_some()
        );
        controller.dispose();
        assert!(
            update_current(&weak, new, |inner| *lock(&inner.status) =
                TerminalStatus::Exited(0))
            .is_none()
        );
        assert!(matches!(
            *lock(&controller.inner.status),
            TerminalStatus::Stopped
        ));
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    fn seeded() -> TerminalController {
        let controller = TerminalController::new();
        controller.test_parser().screen_mut().set_size(3, 20);
        controller
            .test_parser()
            .process(b"alpha ERROR\r\nbeta\r\nerror gamma\r\ndelta\r\nlast error");
        controller
    }

    #[test]
    fn search_starts_at_the_last_match_wraps_both_ways_and_selects_each_match() {
        let controller = seeded();
        let status = controller.search("error");
        assert_eq!(
            status,
            SearchStatus {
                total: 3,
                current: 3
            }
        );
        // The newest match is on the live screen, so no scrolling is needed.
        assert_eq!(controller.test_parser().screen().scrollback(), 0);
        assert_eq!(controller.selected_text().as_deref(), Some("error"));

        // The middle match is still on the live screen.
        assert_eq!(controller.search_step(-1).current, 2);
        assert_eq!(controller.test_parser().screen().scrollback(), 0);
        assert_eq!(controller.selected_text().as_deref(), Some("error"));
        // The first one is in the history, so the view scrolls to it; the text
        // keeps the case it was printed with.
        assert_eq!(controller.search_step(-1).current, 1);
        assert!(controller.test_parser().screen().scrollback() > 0);
        assert_eq!(controller.selected_text().as_deref(), Some("ERROR"));
        // Wraps around in both directions, and returning to the live end scrolls back.
        assert_eq!(controller.search_step(-1).current, 3);
        assert_eq!(controller.test_parser().screen().scrollback(), 0);
        assert_eq!(controller.search_step(1).current, 1);
        assert_eq!(controller.search_step(1).current, 2);
    }

    #[test]
    fn a_search_with_no_match_or_after_clearing_leaves_no_selection() {
        let controller = seeded();
        assert_eq!(controller.search("nothing"), SearchStatus::default());
        assert!(controller.selected_text().is_none());
        assert_eq!(controller.search_step(1), SearchStatus::default());

        controller.search("alpha");
        assert!(controller.selected_text().is_some());
        controller.search_clear();
        assert!(controller.selected_text().is_none());
        assert_eq!(controller.search_step(1), SearchStatus::default());
    }

    #[test]
    fn stepping_reads_output_that_arrived_after_the_search_began() {
        let controller = seeded();
        assert_eq!(controller.search("error").total, 3);
        controller.test_parser().process(b"\r\nanother ERROR");
        let status = controller.search_step(0);
        assert_eq!(status.total, 4);
    }
}

#[cfg(test)]
impl TerminalController {
    pub(crate) fn bench_seed(&self, bytes: &[u8]) {
        lock(&self.inner.parser).process(bytes);
        *lock(&self.inner.status) = TerminalStatus::Running;
    }
}
