//! Incremental OSC 9;9 parsing, independent of shell startup and disk I/O.
use std::path::PathBuf;

#[derive(Default)]
pub struct DirectoryReports {
    escape: bool,
    osc: Option<Vec<u8>>,
    oversized: bool,
}

impl DirectoryReports {
    pub fn process(&mut self, bytes: &[u8]) -> Option<PathBuf> {
        self.process_with(bytes, |_| {})
    }
    pub(crate) fn process_with(
        &mut self,
        bytes: &[u8],
        mut report: impl FnMut(&[u8]),
    ) -> Option<PathBuf> {
        let mut latest = None;
        for &byte in bytes {
            if let Some(osc) = &mut self.osc {
                if byte == 7 || (self.escape && byte == b'\\') {
                    if !self.oversized {
                        latest = report_path(osc).or(latest);
                        report(osc);
                    }
                    self.osc = None;
                    self.escape = false;
                    self.oversized = false;
                } else if byte == 0x18 || byte == 0x1a {
                    self.osc = None;
                    self.escape = false;
                    self.oversized = false;
                } else {
                    if self.escape {
                        // An ESC not followed by ST cancels the malformed OSC.
                        self.osc = (byte == b']').then(Vec::new);
                        self.oversized = false;
                    } else if byte != 0x1b && !self.oversized {
                        if osc.len() < 32 * 1024 {
                            osc.push(byte);
                        } else {
                            osc.clear();
                            self.oversized = true;
                        }
                    }
                    self.escape = byte == 0x1b;
                }
            } else {
                if self.escape && byte == b']' {
                    self.osc = Some(Vec::new());
                }
                self.escape = byte == 0x1b;
            }
        }
        latest
    }
}

fn report_path(bytes: &[u8]) -> Option<PathBuf> {
    let value = std::str::from_utf8(bytes).ok()?.strip_prefix("9;9;")?;
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_survive_every_possible_read_boundary() {
        let cwd = PathBuf::from(if cfg!(windows) {
            r"C:\中文 folder;with spaces"
        } else {
            "/tmp/中文 folder;with spaces"
        });
        for end in ["\x07", "\x1b\\"] {
            let report = format!("text\x1b]9;9;\"{}\"{end}prompt", cwd.display());
            for split in 0..=report.len() {
                let mut parser = DirectoryReports::default();
                let first = parser.process(&report.as_bytes()[..split]);
                let last = parser.process(&report.as_bytes()[split..]);
                assert_eq!(last.or(first), Some(cwd.clone()), "split {split}");
            }
        }
    }

    #[test]
    fn rejects_invalid_reports_and_recovers_after_oversized_or_cancelled_osc() {
        let cwd = PathBuf::from(if cfg!(windows) { r"C:\tmp" } else { "/tmp" });
        let valid = format!("\x1b]9;9;{}\x07", cwd.display());
        for invalid in [
            b"\x1b]0;title\x07".to_vec(),
            b"\x1b]9;9;relative\x07".to_vec(),
            b"\x1b]9;9;\xff\x07".to_vec(),
            b"\x1b]9;9;\x18".to_vec(),
            format!("\x1b]9;9;{}\x07", "a".repeat(40_000)).into_bytes(),
        ] {
            let mut parser = DirectoryReports::default();
            assert_eq!(parser.process(&invalid), None);
            assert_eq!(parser.process(valid.as_bytes()), Some(cwd.clone()));
        }
    }
}
