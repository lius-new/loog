//! Background-only reads through asynchronous provider requests. No UI or
//! executor thread waits here; callers must run this on an IO worker.
use super::context::AppContext;
use loog_workspace_api::{Answer, FileKind, Operation, ProviderPath, ResourceRef, ScopeId};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

pub(super) fn relative_background(
    state: &AppContext,
    scope: ScopeId,
    resource: &ResourceRef,
    relative: &str,
    limit: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, String> {
    background(state, scope, resource, cancelled, |request, path| {
        relative_read(request, path, relative, limit)
    })
}
pub(super) fn resolve_background(
    state: &AppContext,
    scope: ScopeId,
    resource: &ResourceRef,
    relative: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<(loog_workspace_api::FileInfo, Vec<ProviderPath>), String> {
    background(state, scope, resource, cancelled, |request, path| {
        let file = relative_target(request, path, relative)?;
        let mut ancestors = Vec::new();
        if file.kind == FileKind::Directory {
            let mut path = file.path.clone();
            loop {
                if ancestors.len() >= 64 || ancestors.contains(&path) {
                    return Err("Directory ancestry exceeded its budget".into());
                }
                ancestors.push(path.clone());
                match request(Operation::ParentPath { path })? {
                    Answer::Parent { path: Some(parent) } => path = parent,
                    Answer::Parent { path: None } => break,
                    _ => return Err("Invalid directory ancestry response".into()),
                }
            }
            ancestors.reverse();
        }
        Ok((file, ancestors))
    })
}
fn background<T>(
    state: &AppContext,
    scope: ScopeId,
    resource: &ResourceRef,
    cancelled: &dyn Fn() -> bool,
    work: impl FnOnce(
        &mut dyn FnMut(Operation) -> Result<Answer, String>,
        &ProviderPath,
    ) -> Result<T, String>,
) -> Result<T, String> {
    background_typed(state, scope, resource, cancelled, |request, path| {
        work(
            &mut |operation| request(operation).map_err(|error| error.message),
            path,
        )
    })
}

pub(super) fn background_typed<T>(
    state: &AppContext,
    scope: ScopeId,
    resource: &ResourceRef,
    cancelled: &dyn Fn() -> bool,
    work: impl FnOnce(
        &mut dyn FnMut(Operation) -> Result<Answer, loog_workspace_api::Failure>,
        &ProviderPath,
    ) -> Result<T, String>,
) -> Result<T, String> {
    use loog_workspace_api::{Failure, FailureKind};
    let ResourceRef::Provided { path, environment } = resource else {
        return Err("Expected a provided resource".into());
    };
    if !state
        .get()
        .explorer
        .resources
        .root(scope)
        .is_some_and(|root| root.descriptor.resource.environment() == Some(environment))
    {
        return Err("Resource scope is no longer connected".into());
    }
    let host = state
        .services()
        .and_then(|runtime| runtime.extensions())
        .ok_or("Resource provider is unavailable")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    work(
        &mut |operation| {
            if cancelled() || Instant::now() >= deadline {
                return Err(Failure::new(
                    FailureKind::Cancelled,
                    "Resource read cancelled or timed out",
                ));
            }
            let (send, receive) = mpsc::channel();
            let request = host.request_resource(
                scope,
                operation,
                Box::new(move |_, result| {
                    let _ = send.send(result);
                }),
            )?;
            loop {
                if cancelled() || Instant::now() >= deadline {
                    host.cancel_resource(request);
                    return Err(Failure::new(
                        FailureKind::Cancelled,
                        "Resource read cancelled or timed out",
                    ));
                }
                match receive.recv_timeout(Duration::from_millis(50)) {
                    Ok(result) => {
                        return if cancelled() || Instant::now() >= deadline {
                            Err(Failure::new(
                                FailureKind::Cancelled,
                                "Resource read cancelled or timed out",
                            ))
                        } else {
                            result
                        };
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(Failure::new(
                            FailureKind::Unavailable,
                            "Resource provider stopped",
                        ));
                    }
                }
            }
        },
        path,
    )
}
fn relative_read(
    request: &mut dyn FnMut(Operation) -> Result<Answer, String>,
    base: &ProviderPath,
    relative: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    if relative.is_empty() || relative.len() > 32 * 1024 || relative.contains('\0') {
        return Err("Invalid resource read bounds or relative path".into());
    }
    if !(1..=32 * 1024 * 1024).contains(&limit) {
        return Err("Invalid resource read limit".into());
    }
    let file = relative_target(request, base, relative)?;
    let path = file.path.clone();
    if !matches!(file.kind, FileKind::File | FileKind::Symlink) || file.size > limit as u64 {
        return Err("Resource is not a regular file within its size limit".into());
    }
    read_file(request, path, file.revision, file.size, limit)
}
fn relative_target(
    request: &mut dyn FnMut(Operation) -> Result<Answer, String>,
    base: &ProviderPath,
    relative: &str,
) -> Result<loog_workspace_api::FileInfo, String> {
    if relative.is_empty() || relative.len() > 32 * 1024 || relative.contains('\0') {
        return Err("Invalid relative resource path".into());
    }
    let Answer::Parent { path: Some(parent) } =
        request(Operation::ParentPath { path: base.clone() })?
    else {
        return Err("Resource has no readable parent directory".into());
    };
    let Answer::Path { path } = request(Operation::ResolvePath {
        base: parent,
        relative: relative.into(),
    })?
    else {
        return Err("Invalid relative resource response".into());
    };
    let Answer::Info { file } = request(Operation::Stat { path: path.clone() })? else {
        return Err("Invalid resource metadata".into());
    };
    if file.path != path {
        return Err("Resource identity changed while resolving".into());
    }
    Ok(file)
}
pub(super) fn read_file(
    request: &mut dyn FnMut(Operation) -> Result<Answer, String>,
    path: ProviderPath,
    mut revision: Option<String>,
    expected_size: u64,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        let Answer::Bytes {
            bytes: chunk,
            revision: actual,
            eof,
        } = request(Operation::Read {
            path: path.clone(),
            offset: bytes.len() as u64,
            limit: 128 * 1024,
        })?
        else {
            return Err("Invalid resource byte response".into());
        };
        if chunk.len() > 128 * 1024
            || bytes.len() + chunk.len() > limit
            || (!eof && chunk.is_empty())
        {
            return Err("Resource read exceeds its budget or makes no progress".into());
        }
        if revision.is_some() && revision != actual || !eof && actual.is_none() {
            return Err("Resource changed during reading or has no stable revision".into());
        }
        revision = actual;
        bytes.extend(chunk);
        if eof {
            return if bytes.len() as u64 == expected_size {
                Ok(bytes)
            } else {
                Err("Resource length changed while reading".into())
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_resource_bytes_preserve_provider_names_and_reject_mixed_revisions() {
        let path = ProviderPath::new("opaque:file").unwrap();
        for change in [false, true] {
            let mut reads = 0;
            let result = relative_read(
                &mut |operation| {
                    Ok(match operation {
                        Operation::ParentPath { path: base } => {
                            assert_eq!(base.as_str(), "opaque:document");
                            Answer::Parent {
                                path: Some(ProviderPath::new("opaque:parent").unwrap()),
                            }
                        }
                        Operation::ResolvePath { base, relative } => {
                            assert_eq!(base.as_str(), "opaque:parent");
                            assert_eq!(relative, "../中文\\image.png");
                            Answer::Path { path: path.clone() }
                        }
                        Operation::Stat { .. } => Answer::Info {
                            file: loog_workspace_api::FileInfo {
                                path: path.clone(),
                                name: "image.png".into(),
                                kind: FileKind::File,
                                size: 2,
                                revision: Some("one".into()),
                                readonly: true,
                            },
                        },
                        Operation::Read { offset, .. } => {
                            assert_eq!(offset, reads);
                            reads += 1;
                            Answer::Bytes {
                                bytes: vec![reads as u8],
                                eof: reads == 2,
                                revision: Some(
                                    if reads == 2 && change { "two" } else { "one" }.into(),
                                ),
                            }
                        }
                        _ => panic!("unexpected resource operation"),
                    })
                },
                &ProviderPath::new("opaque:document").unwrap(),
                "../中文\\image.png",
                8,
            );
            if change {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), [1, 2]);
            }
        }
    }
}
