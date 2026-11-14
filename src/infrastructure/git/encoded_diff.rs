//! Git objects are decoded independently; patch text is never decoded as one file.
use super::command::{GitCommand, GitCommandRunner};
use crate::features::encoding::{self, TextFormat};
use crate::features::git::diff::{FileDiff, UnifiedDiff, parse_unified};
use crate::features::git::error::GitErrorKind;
use crate::features::git::{DiffTarget, GitError, GitResult};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
const LIMIT: u64 = crate::infrastructure::document_filesystem::MAX_EDITABLE_FILE_BYTES;
fn blob(
    runner: &GitCommandRunner,
    root: &Path,
    revision: &str,
    path: &Path,
) -> GitResult<Option<Vec<u8>>> {
    let object = format!("{revision}:{}", path.to_string_lossy().replace('\\', "/"));
    let result = runner.run(
        GitCommand::new()
            .cwd(root)
            .args(["cat-file", "-e", &object])
            .read_only(),
    );
    // Missing sides are normal for additions/deletions, other errors are reported by diff.
    if result.is_err() {
        return Ok(None);
    }
    let size = runner
        .run(
            GitCommand::new()
                .cwd(root)
                .args(["cat-file", "-s", &object])
                .read_only(),
        )?
        .stdout_text()?
        .trim()
        .parse::<u64>()
        .unwrap_or(u64::MAX);
    if size > LIMIT {
        return Err(GitError::new(
            GitErrorKind::Unsupported,
            "File is too large for text diff",
        ));
    }
    Ok(Some(
        runner
            .run(
                GitCommand::new()
                    .cwd(root)
                    .args(["cat-file", "blob", &object])
                    .read_only(),
            )?
            .stdout,
    ))
}
pub(super) fn compare(
    runner: &GitCommandRunner,
    root: &Path,
    target: DiffTarget,
    paths: &[PathBuf],
    commit: Option<&str>,
    context: Option<usize>,
) -> GitResult<Option<UnifiedDiff>> {
    if paths.len() != 1 {
        return Ok(None);
    }
    let path = &paths[0];
    let (old, new) = if let Some(commit) = commit {
        (
            blob(runner, root, &format!("{commit}^"), path)?,
            blob(runner, root, commit, path)?,
        )
    } else {
        let old = blob(
            runner,
            root,
            if target == DiffTarget::IndexToWorktree {
                ""
            } else {
                "HEAD"
            },
            path,
        )?;
        let new = if target == DiffTarget::HeadToIndex {
            blob(runner, root, "", path)?
        } else {
            // The working-tree side, read where the repository is.
            super::repo_fs::read(&root.join(path), LIMIT as usize)?
        };
        (old, new)
    };
    if old.is_none() && new.is_none() {
        return Ok(None);
    }
    let settings = crate::infrastructure::document_filesystem::options(&root.join(path));
    let decode = |bytes: &Option<Vec<u8>>| {
        bytes
            .as_ref()
            .map(|b| encoding::decode(b, None, &settings, LIMIT as usize))
            .transpose()
    };
    let (old_text, new_text) = match (decode(&old), decode(&new)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(error), _) | (_, Err(error)) => {
            if error != "Binary file" {
                return Err(GitError::new(
                    GitErrorKind::Unsupported,
                    "Could not determine the file encoding for diff",
                )
                .with_detail(error));
            }
            return Ok(Some(UnifiedDiff {
                files: vec![FileDiff {
                    old_path: old.as_ref().map(|_| path.clone()),
                    new_path: new.as_ref().map(|_| path.clone()),
                    binary: true,
                    ..Default::default()
                }],
            }));
        }
    };
    let plain = |value: &Option<encoding::DecodedText>| {
        value
            .as_ref()
            .is_none_or(|v| v.format == TextFormat::default())
    };
    if plain(&old_text) && plain(&new_text) {
        return Ok(None);
    }
    let old_string = old_text.as_ref().map_or("", |v| v.text.as_str());
    let new_string = new_text.as_ref().map_or("", |v| v.text.as_str());
    let format_change = match (&old_text, &new_text) {
        (Some(a), Some(b)) if a.format != b.format => Some(format!(
            "Encoding: {} → {}",
            a.format.label(),
            b.format.label()
        )),
        _ => None,
    };
    if old_string == new_string {
        return Ok(Some(UnifiedDiff {
            files: vec![FileDiff {
                old_path: old.as_ref().map(|_| path.clone()),
                new_path: new.as_ref().map(|_| path.clone()),
                encoded: true,
                format_change,
                ..Default::default()
            }],
        }));
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "loog-encoding-diff-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&directory)?;
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Temp(directory.clone());
    let a = directory.join("old.txt");
    let b = directory.join("new.txt");
    std::fs::write(&a, old_string)?;
    std::fs::write(&b, new_string)?;
    let output = runner.run(
        GitCommand::new()
            .cwd(&directory)
            .args([
                "-c",
                "core.autocrlf=false",
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--text",
            ])
            .args([format!("--unified={}", context.unwrap_or(3))])
            .args(["--"])
            .args([a.as_os_str(), b.as_os_str()])
            .accept_exit_code(1)
            .read_only(),
    )?;
    let mut diff = parse_unified(&output.stdout_text()?)?;
    for file in &mut diff.files {
        file.old_path = old.as_ref().map(|_| path.clone());
        file.new_path = new.as_ref().map(|_| path.clone());
        file.encoded = true;
        file.format_change = format_change.clone();
    }
    Ok(Some(diff))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::encoding::Charset;
    use crate::features::git::diff::DiffLine;
    use crate::infrastructure::git::{backend::CliGitBackend, runtime::GitRuntimeManager};
    #[test]
    fn encoding_git_diff_decodes_each_side_and_reports_format_only_changes() {
        let runtime = GitRuntimeManager::default().resolve().expect("Git runtime");
        let backend = CliGitBackend::new(runtime);
        let directory =
            std::env::temp_dir().join(format!("loog-encoding-git-tests-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        struct Temp(PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _temp = Temp(directory.clone());
        backend
            .runner()
            .run(
                GitCommand::new()
                    .cwd(&directory)
                    .args(["init", "-b", "main"]),
            )
            .unwrap();
        for args in [
            ["config", "user.name", "Encoding Test"],
            ["config", "user.email", "encoding@example.invalid"],
            ["config", "core.autocrlf", "false"],
        ] {
            backend
                .runner()
                .run(GitCommand::new().cwd(&directory).args(args))
                .unwrap();
        }
        let path = PathBuf::from("中文.txt");
        let full = directory.join(&path);
        let gbk = TextFormat::new(Charset::Gbk, false);
        let original = "这是用于验证编码识别和文件差异的中文文本。\n原来的内容。\n";
        crate::infrastructure::document_filesystem::write_encoded(&full, original, gbk).unwrap();
        backend
            .stage(&directory, std::slice::from_ref(&path))
            .unwrap();
        backend.commit(&directory, "initial", false, false).unwrap();
        let changed = original.replace("原来的内容", "修改后的内容");
        crate::infrastructure::document_filesystem::write_encoded(
            &full,
            &changed,
            TextFormat::new(Charset::Utf16Le, true),
        )
        .unwrap();
        let diff = backend
            .parsed_full_diff(
                &directory,
                DiffTarget::IndexToWorktree,
                std::slice::from_ref(&path),
            )
            .unwrap();
        assert!(!diff.files[0].binary);
        assert!(diff.files[0].encoded);
        assert!(
            diff.files[0]
                .format_change
                .as_ref()
                .unwrap()
                .contains("UTF-16LE")
        );
        assert!(
            diff.files[0]
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .any(|line| matches!(line,DiffLine::Addition(s) if s.contains("修改后的内容")))
        );
        let batch = backend
            .parsed_diff(&directory, DiffTarget::IndexToWorktree, &[])
            .unwrap();
        assert_eq!(batch.files.len(), 1);
        assert!(!batch.files[0].binary);
        crate::infrastructure::document_filesystem::write_encoded(
            &full,
            original,
            TextFormat::default(),
        )
        .unwrap();
        let diff = backend
            .parsed_full_diff(
                &directory,
                DiffTarget::IndexToWorktree,
                std::slice::from_ref(&path),
            )
            .unwrap();
        assert!(diff.files[0].hunks.is_empty());
        assert!(diff.files[0].format_change.is_some());
        backend
            .stage(&directory, std::slice::from_ref(&path))
            .unwrap();
        let staged = backend
            .parsed_full_diff(
                &directory,
                DiffTarget::HeadToIndex,
                std::slice::from_ref(&path),
            )
            .unwrap();
        assert!(staged.files[0].format_change.is_some());
        backend.commit(&directory, "convert", false, false).unwrap();
        let commit = backend.commit_full_diff(&directory, "HEAD", &path).unwrap();
        assert!(commit.files[0].format_change.is_some());
        let preview = backend.commit_file_diff(&directory, "HEAD", &path).unwrap();
        assert!(preview.files[0].format_change.is_some());
        assert!(preview.files[0].encoded);
    }
}
