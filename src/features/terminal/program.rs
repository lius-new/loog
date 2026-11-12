//! Program labels and candidate selection from supplied process information.
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramCandidate {
    pub pid: u32,
    pub name: String,
}

#[derive(Default)]
pub struct SustainedProgram {
    candidate: Option<ProgramCandidate>,
    since: Option<Instant>,
}

impl SustainedProgram {
    pub fn sustained_pid(&self, now: Instant) -> Option<u32> {
        self.since
            .filter(|since| now.duration_since(*since) >= Duration::from_secs(1))
            .and_then(|_| self.candidate.as_ref().map(|program| program.pid))
    }
    pub fn observe(&mut self, candidate: Option<ProgramCandidate>, now: Instant) -> Option<String> {
        if candidate != self.candidate {
            self.candidate = candidate;
            self.since = self.candidate.as_ref().map(|_| now);
        }
        self.since
            .filter(|since| now.duration_since(*since) >= Duration::from_secs(1))
            .and_then(|_| self.candidate.as_ref().map(|program| program.name.clone()))
    }
}

pub(crate) fn program_name(path: &str) -> Option<String> {
    let name = path.trim().rsplit(['/', '\\']).next()?;
    let name = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))
        .unwrap_or(name);
    (!name.is_empty()).then(|| {
        name.chars()
            .filter(|ch| !ch.is_control())
            .take(64)
            .collect()
    })
}

/// Select from the current shell descendants without a helper process.
/// Missing launcher links and ambiguous independent branches produce no label.
/// Keep a sustained program selected while it is still in this shell's tree.
#[cfg(any(windows, test))]
pub(crate) fn descendant_program(
    root: u32,
    preferred: Option<u32>,
    processes: &[(u32, u32, String)],
) -> Option<ProgramCandidate> {
    if root == 0 {
        return None;
    }
    let mut parents = vec![root];
    let mut visited = vec![root];
    while !parents.is_empty() {
        let mut next = Vec::new();
        for (pid, parent, _) in processes {
            if parents.contains(parent) && !visited.contains(pid) {
                visited.push(*pid);
                next.push(*pid);
            }
        }
        parents = next;
    }
    let programs = processes
        .iter()
        .filter(|entry| entry.0 != root && visited.contains(&entry.0))
        .collect::<Vec<_>>();
    let candidate = |entry: &(u32, u32, String)| {
        Some(ProgramCandidate {
            pid: entry.0,
            name: program_name(&entry.2)?,
        })
    };
    if let Some(entry) = preferred.and_then(|pid| programs.iter().find(|entry| entry.0 == pid)) {
        return candidate(entry);
    }
    let mut roots = programs
        .iter()
        .copied()
        .filter(|entry| !programs.iter().any(|parent| parent.0 == entry.1));
    let mut entry = roots.next()?;
    if roots.next().is_some() {
        return None;
    }
    let mut seen = Vec::new();
    loop {
        if seen.contains(&entry.0) {
            return None;
        }
        seen.push(entry.0);
        let children = programs
            .iter()
            .copied()
            .filter(|child| child.1 == entry.0)
            .collect::<Vec<_>>();
        if children.len() != 1 {
            return candidate(entry);
        }
        entry = children[0];
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_sustained_processes_get_labels_and_exit_resets_immediately() {
        let start = Instant::now();
        let app = ProgramCandidate {
            pid: 12,
            name: "node".into(),
        };
        let mut tracker = SustainedProgram::default();
        assert_eq!(tracker.observe(Some(app.clone()), start), None);
        assert_eq!(
            tracker.observe(Some(app.clone()), start + Duration::from_millis(999)),
            None
        );
        assert_eq!(
            tracker.observe(Some(app), start + Duration::from_secs(1)),
            Some("node".into())
        );
        assert_eq!(tracker.observe(None, start + Duration::from_secs(2)), None);
    }

    #[test]
    fn repeated_short_processes_and_program_switches_do_not_reuse_elapsed_time() {
        let start = Instant::now();
        let mut tracker = SustainedProgram::default();
        for pid in 1..10 {
            let app = ProgramCandidate {
                pid,
                name: "git".into(),
            };
            assert_eq!(
                tracker.observe(
                    Some(app),
                    start + Duration::from_millis(u64::from(pid) * 300)
                ),
                None
            );
        }
        let app = ProgramCandidate {
            pid: 42,
            name: "python".into(),
        };
        assert_eq!(
            tracker.observe(Some(app.clone()), start + Duration::from_secs(4)),
            None
        );
        assert_eq!(
            tracker.observe(Some(app), start + Duration::from_secs(5)),
            Some("python".into())
        );
    }

    #[test]
    fn ancestor_probe_follows_structural_launcher_chains() {
        let processes = vec![
            (2, 99, "unrelated-system-helper.exe".into()),
            (3, 1, "unknown-launcher.exe".into()),
            (4, 3, "C:\\tools\\node.exe".into()),
            (5, 99, "unrelated.exe".into()),
        ];
        assert_eq!(
            descendant_program(1, None, &processes),
            Some(ProgramCandidate {
                pid: 4,
                name: "node".into()
            })
        );
        assert_eq!(descendant_program(99, None, &[]), None);
        assert_eq!(program_name("/usr/bin/python"), Some("python".into()));
    }

    #[test]
    fn lost_parentage_falls_back_without_borrowing_another_terminals_program() {
        let processes = vec![
            (1, 0, "shell.exe".into()),
            (11, 10, "orphan-launcher.exe".into()),
            (12, 11, "orphan-tool.exe".into()),
            (21, 20, "other-terminal.exe".into()),
        ];
        assert_eq!(descendant_program(1, Some(12), &processes), None);
        assert_eq!(descendant_program(1, Some(21), &processes), None);
        assert_eq!(descendant_program(0, None, &processes), None);
    }

    #[test]
    fn a_sustained_application_keeps_its_label_when_it_spawns_helpers() {
        let processes = vec![
            (1, 0, "shell.exe".into()),
            (12, 1, "arbitrary-tool.exe".into()),
            (13, 12, "unknown-helper.exe".into()),
        ];
        assert_eq!(
            descendant_program(1, Some(12), &processes),
            Some(ProgramCandidate {
                pid: 12,
                name: "arbitrary-tool".into(),
            })
        );
        assert_eq!(
            descendant_program(1, Some(12), &[(1, 0, "shell.exe".into())]),
            None
        );
    }

    #[test]
    fn independent_branches_are_ambiguous_unless_the_program_is_already_established() {
        let processes = vec![(2, 1, "one.exe".into()), (3, 1, "two.exe".into())];
        assert_eq!(descendant_program(1, None, &processes), None);
        assert_eq!(
            descendant_program(1, Some(3), &processes),
            Some(ProgramCandidate {
                pid: 3,
                name: "two".into(),
            })
        );
        let after_exit = vec![(2, 1, "one.exe".into())];
        assert_eq!(
            descendant_program(1, Some(3), &after_exit),
            Some(ProgramCandidate {
                pid: 2,
                name: "one".into(),
            })
        );
    }

    #[test]
    fn malformed_cycles_terminate_without_using_the_shell_itself_as_a_program() {
        let processes = vec![
            (2, 1, "cycle.exe".into()),
            (3, 2, "cycle.exe".into()),
            (2, 3, "cycle.exe".into()),
        ];
        assert_eq!(descendant_program(1, None, &processes), None);
        assert_eq!(
            descendant_program(1, Some(1), &[(1, 0, "shell.exe".into())]),
            None
        );
    }
}
