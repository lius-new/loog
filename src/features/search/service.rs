//! Bounded cancellable search; all disk operations are injected.
use super::ports::SearchSourcePort;
use super::{FileMatches, Matcher, Request, Results, hash};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_MATCHES: usize = 10_000;
pub const MAX_FILES: usize = 5_000;

/// Publications are cumulative snapshots, safe for a latest-only UI mailbox.
pub fn run(
    source: &dyn SearchSourcePort,
    request: Request,
    cancel: &AtomicBool,
    mut publish: impl FnMut(Results),
) -> Result<(), String> {
    let matcher = Arc::new(Matcher::new(&request.spec)?);
    let buffers = request
        .buffers
        .into_iter()
        .map(|buffer| (source.normalize(&buffer.path), buffer))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut results = Results::default();
    let mut result_bytes = 0;
    let mut last_publish = Instant::now();
    for root in request.roots {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let root = source.normalize(&root);
        if !source.is_directory(&root) {
            record_error(
                &mut results,
                format!("{}: folder is unavailable", root.display()),
            );
            continue;
        }
        let walk = source.walk(&root, &request.spec, matcher.clone());
        let stop = cancel;
        for entry in walk {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    record_error(&mut results, error.to_string());
                    continue;
                }
            };
            if !entry.is_file {
                continue;
            }
            let relative = entry.path().strip_prefix(&root).unwrap_or(entry.path());
            if !matcher.includes(relative) {
                continue;
            }
            let mut path = source.normalize(entry.path());
            if !seen.insert(path.clone()) {
                continue;
            }
            let text = if let Some(buffer) = buffers.get(&path) {
                path = buffer.path.clone();
                buffer.text.clone()
            } else {
                match source.read_text(&path) {
                    Ok(text) => Arc::new(text),
                    Err(error)
                        if matches!(
                            error.as_str(),
                            "Binary file"
                                | "Unsupported text encoding"
                                | "File is larger than 4 MiB"
                        ) =>
                    {
                        results.skipped += 1;
                        match error.as_str() {
                            "Binary file" => results.skipped_binary += 1,
                            "Unsupported text encoding" => results.skipped_encoding += 1,
                            _ => results.skipped_large += 1,
                        }
                        continue;
                    }
                    Err(error) => {
                        record_error(&mut results, error);
                        continue;
                    }
                }
            };
            if text.len() > MAX_FILE_BYTES {
                results.skipped += 1;
                results.skipped_large += 1;
                continue;
            }
            if text.as_bytes().contains(&0) {
                results.skipped += 1;
                results.skipped_binary += 1;
                continue;
            }
            results.searched += 1;
            let remaining = MAX_MATCHES.saturating_sub(results.matches);
            let mut matches = matcher.matches(&text, cancel, remaining + 1);
            if matches.len() > remaining {
                results.limit_hit = true;
                matches.truncate(remaining);
            }
            if !matches.is_empty() {
                if results.files.len() >= MAX_FILES {
                    results.limit_hit = true;
                    break;
                }
                result_bytes += path.as_os_str().len() + root.as_os_str().len();
                let mut accepted = 0;
                for found in &matches {
                    let size = found.preview.len() + 96;
                    if result_bytes + size > 16 * 1024 * 1024 {
                        results.limit_hit = true;
                        break;
                    }
                    result_bytes += size;
                    accepted += 1;
                }
                matches.truncate(accepted);
                if matches.is_empty() {
                    break;
                }
                results.matches += matches.len();
                results.files.push(Arc::new(FileMatches {
                    path: path.into(),
                    root: root.clone().into(),
                    provider: None,
                    matches,
                    content_hash: hash(&text),
                }));
            }
            if last_publish.elapsed() >= Duration::from_millis(60) {
                publish(results.clone());
                last_publish = Instant::now();
            }
            if results.limit_hit {
                break;
            }
        }
        if results.limit_hit {
            break;
        }
    }
    results.files.sort_by(|a, b| a.path.cmp(&b.path));
    results.cancelled = cancel.load(Ordering::Relaxed);
    results.complete = true;
    publish(results);
    Ok(())
}

fn record_error(results: &mut Results, error: String) {
    results.errors += 1;
    if results.first_error.is_none() {
        results.first_error = Some(error);
    }
}
