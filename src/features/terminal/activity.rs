//! Incremental command lifecycle reports; process names never imply activity.
use crate::features::terminal::directory::DirectoryReports;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CommandActivity {
    #[default]
    Unknown,
    Idle,
    Running,
}

#[derive(Default)]
pub(crate) struct CommandReports {
    stream: DirectoryReports,
    activity: CommandActivity,
    command_name: Option<String>,
    pending_name: Option<String>,
}
impl CommandReports {
    pub fn activity(&self) -> CommandActivity {
        self.activity
    }
    pub fn command_name(&self) -> Option<&str> {
        (self.activity == CommandActivity::Running)
            .then_some(self.command_name.as_deref())
            .flatten()
    }
    pub fn process(&mut self, bytes: &[u8]) -> Option<PathBuf> {
        let activity = &mut self.activity;
        let command_name = &mut self.command_name;
        let pending_name = &mut self.pending_name;
        self.stream.process_with(bytes, |packet| {
            let Ok(packet) = std::str::from_utf8(packet) else {
                return;
            };
            let mut fields = packet.split(';');
            let family = fields.next();
            if !matches!(family, Some("133" | "633")) {
                return;
            }
            match fields.next() {
                Some("A" | "B" | "D") => {
                    *activity = CommandActivity::Idle;
                    *command_name = None;
                    *pending_name = None;
                }
                Some("C") => {
                    if *activity != CommandActivity::Running {
                        *command_name = pending_name.take();
                    }
                    *activity = CommandActivity::Running;
                }
                // Shell integration reports script entry names as a property.
                // Ordinary executables retain native process-tree labels.
                Some("P") if family == Some("633") => {
                    if let Some(value) = fields.next().and_then(|v| v.strip_prefix("CommandName="))
                    {
                        let name = decode_name(value);
                        if *activity == CommandActivity::Running {
                            *command_name = name;
                        } else {
                            *pending_name = name;
                        }
                    }
                }
                _ => {}
            }
        })
    }
}

// OSC 633 properties use \xHH for reserved ASCII bytes and \\ for backslashes.
// Keep only the bounded display name; never retain command arguments.
fn decode_name(value: &str) -> Option<String> {
    if value.len() > 4096 {
        return None;
    }
    let mut decoded = Vec::new();
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            decoded.push(byte);
        } else {
            match bytes.next()? {
                b'\\' => decoded.push(b'\\'),
                b'x' => {
                    let hi = (bytes.next()? as char).to_digit(16)? as u8;
                    let lo = (bytes.next()? as char).to_digit(16)? as u8;
                    decoded.push(hi * 16 + lo);
                }
                _ => return None,
            }
        }
    }
    let name = std::str::from_utf8(&decoded).ok()?;
    if name.chars().any(char::is_control) {
        return None;
    }
    crate::features::terminal::program::program_name(name).filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn script_names_work_on_either_side_of_command_start_and_clear_at_prompt() {
        for before in [true, false] {
            let mut reports = CommandReports::default();
            reports.process(b"\x1b]133;A\x07");
            let name = b"\x1b]633;P;CommandName=npm\x07";
            if before {
                reports.process(name);
            }
            assert_eq!(reports.command_name(), None);
            reports.process(b"\x1b]133;C\x07");
            if !before {
                reports.process(name);
            }
            assert_eq!(reports.command_name(), Some("npm"));
            // A second start from another shell integration preserves the name.
            reports.process(b"\x1b]633;C\x07");
            assert_eq!(reports.command_name(), Some("npm"));
            reports.process(b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;C\x07");
            assert_eq!(reports.command_name(), None);
        }
    }
    #[test]
    fn script_name_properties_are_fragmented_bounded_and_safely_decoded() {
        let packet = b"\x1b]633;P;CommandName=build\\x20tool\\x3bname\x07";
        for split in 0..=packet.len() {
            let mut reports = CommandReports::default();
            reports.process(b"\x1b]133;C\x07");
            reports.process(&packet[..split]);
            reports.process(&packet[split..]);
            assert_eq!(reports.command_name(), Some("build tool;name"));
        }
        for value in ["", r"bad\x0a", r"bad\xzz", r"bad\", &"x".repeat(4097)] {
            let mut reports = CommandReports::default();
            reports.process(format!("\x1b]133;C\x07\x1b]633;P;CommandName={value}\x07").as_bytes());
            assert_eq!(reports.command_name(), None, "{value}");
        }
    }
    #[test]
    fn command_activity_survives_fragmented_reports_and_finishes_at_the_prompt() {
        for family in ["133", "633"] {
            for end in ["\x07", "\x1b\\"] {
                let start = format!("\x1b]{family};C{end}");
                for split in 0..=start.len() {
                    let mut reports = CommandReports::default();
                    assert_eq!(reports.activity(), CommandActivity::Unknown);
                    reports.process(&start.as_bytes()[..split]);
                    reports.process(&start.as_bytes()[split..]);
                    assert_eq!(reports.activity(), CommandActivity::Running);
                    reports.process(
                        format!("\x1b]{family};D;130{end}\x1b]{family};A{end}").as_bytes(),
                    );
                    assert_eq!(reports.activity(), CommandActivity::Idle);
                }
            }
        }
    }
    #[test]
    fn text_input_echo_titles_and_unrelated_reports_do_not_assert_busy() {
        let mut reports = CommandReports::default();
        reports.process(b"bash wslhost task\r\n\x1b]0;title\x07\x1b]133;P;name=value\x07");
        assert_eq!(reports.activity(), CommandActivity::Unknown);
        reports.process(b"\x1b]133;A\x07\x1b]133;B\x07");
        assert_eq!(reports.activity(), CommandActivity::Idle);
    }
    #[test]
    fn malformed_and_oversized_packets_do_not_change_activity() {
        let mut reports = CommandReports::default();
        reports.process(b"\x1b]133;C\x18\x1b]133;\xff\x07");
        reports.process(format!("\x1b]133;C;{}\x07", "x".repeat(40_000)).as_bytes());
        assert_eq!(reports.activity(), CommandActivity::Unknown);
        reports.process(b"\x1b]133;C\x07");
        assert_eq!(reports.activity(), CommandActivity::Running);
        reports.process(b"\x1b]133;D\x07");
        assert_eq!(reports.activity(), CommandActivity::Idle);
    }
    #[test]
    fn directory_reports_and_command_activity_share_the_same_stream() {
        let path = if cfg!(windows) {
            r"C:\folder"
        } else {
            "/folder"
        };
        let mut reports = CommandReports::default();
        let cwd = reports.process(format!("\x1b]133;C\x07\x1b]9;9;{path}\x07").as_bytes());
        assert_eq!(cwd, Some(PathBuf::from(path)));
        assert_eq!(reports.activity(), CommandActivity::Running);
    }
}
