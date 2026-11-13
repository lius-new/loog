//! Independent in-memory provider for native consumer and fault acceptance.
//! Tokens deliberately have no filesystem hierarchy. No SSH or Agent protocol.
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap};

const ROOT: &str = "collection:7";
const FILE: &str = "object:中文%17";
const OTHER: &str = "object:other";
#[derive(Default)]
struct Memory {
    active: bool,
    registration: Option<u64>,
    mounting: Option<u64>,
    files: BTreeMap<String, (Vec<u8>, u64)>,
    writes: BTreeMap<u64, (String, Vec<u8>, usize, Option<String>)>,
    next: u64,
    hold_reads: bool,
    lose_commit: bool,
    delayed: Vec<Value>,
    commits: u64,
}
thread_local! { static MEMORY: RefCell<Memory> = RefCell::default(); }
fn submit(command: Value) -> u64 {
    crate::loog::workspace::session::submit(&command.to_string()).unwrap()
}
pub fn command(command: &str) -> bool {
    let Some(command) = command.strip_prefix("example.tools.memory.") else {
        return false;
    };
    MEMORY.with(|memory| {
        let mut state = memory.borrow_mut();
        match command {
            "start" => {
                *state = Memory { active: true, ..Memory::default() };
                let bytes = [255, 254].into_iter().chain("中文\r\nfirst".encode_utf16().flat_map(u16::to_le_bytes)).collect();
                state.files.insert(FILE.into(), (bytes, 1));
                state.files.insert(OTHER.into(), (b"second".to_vec(), 1));
                state.registration = Some(submit(json!({"kind":"register", "descriptor":{
                    "key":{"provider":"example.tools","instance":"opaque-memory"},
                    "name":"Memory collection", "platform":"independent", "capabilities":["files_read","files_write"]
                }})));
            }
            "hold" => { state.hold_reads = true; crate::host::log(0, "memory holding"); }
            "release" => {
                state.hold_reads = false;
                for response in state.delayed.drain(..).rev() { submit(response); }
            }
            "lose" => { state.lose_commit = true; crate::host::log(0, "memory losing next receipt"); }
            "inspect" => crate::host::log(0, &format!("memory commits={}", state.commits)),
            _ => panic!("unknown memory fixture command"),
        }
    });
    true
}
fn failure(kind: &str, message: &str) -> Value {
    json!({"Err":{"kind":kind,"message":message}})
}
fn info(path: &str, bytes: &[u8], revision: u64) -> Value {
    json!({"path":path,"name":if path == FILE { "中文.txt" } else { "second.txt" },
        "kind":"file","size":bytes.len(),"revision":revision.to_string(),"readonly":false})
}
pub fn event(event: &str) -> bool {
    MEMORY.with(|memory| {
        let mut state = memory.borrow_mut();
        if !state.active {
            return false;
        }
        let event: Value = serde_json::from_str(event).unwrap();
        if event["kind"] == "reply" {
            let ticket = event["ticket"].as_u64();
            if ticket == state.registration && state.registration.is_some() {
                state.registration = None;
                let environment = event["result"]["Ok"]["id"].as_u64().expect("registration");
                state.mounting = Some(submit(
                    json!({"kind":"mount","environment":environment,"root":ROOT}),
                ));
            } else if ticket == state.mounting && state.mounting.is_some() {
                state.mounting = None;
                let scope = event["result"]["Ok"]["id"].as_u64().expect("mount");
                submit(json!({"kind":"present","scope":scope}));
            }
            return true;
        }
        if event["kind"] != "delivery" || event["event"]["kind"] != "request" {
            return true;
        }
        let request = &event["event"];
        let operation = &request["operation"];
        let kind = operation["kind"].as_str().unwrap();
        let result = state.answer(operation);
        let response = json!({"kind":"complete","request":request["id"],"result":result});
        if kind == "read" && operation["path"] == FILE && state.hold_reads {
            state.delayed.push(response);
            crate::host::log(0, "memory read held");
        } else if kind == "commit_write" && state.lose_commit {
            state.lose_commit = false;
            // The mutation happened; deliberately lose only its success receipt.
            crate::host::log(0, "memory committed without receipt");
        } else {
            submit(response);
        }
        true
    })
}
impl Memory {
    fn answer(&mut self, operation: &Value) -> Value {
        let path = operation["path"].as_str().unwrap_or("");
        let answer = match operation["kind"].as_str().unwrap() {
            "list" if path == ROOT => {
                let target = if operation["cursor"].is_null() {
                    FILE
                } else {
                    OTHER
                };
                let (bytes, revision) = &self.files[target];
                json!({"kind":"directory","entries":[info(target,bytes,*revision)],
                    "cursor":if target == FILE { Some("page-two") } else { None }})
            }
            "list" => return failure("permission_denied", "Collection cannot be listed"),
            "stat" => match self.files.get(path) {
                Some((bytes, revision)) => json!({"kind":"info","file":info(path,bytes,*revision)}),
                None => return failure("not_found", "Object does not exist"),
            },
            "read" => {
                let Some((bytes, revision)) = self.files.get(path) else {
                    return failure("not_found", "Object does not exist");
                };
                let start = operation["offset"].as_u64().unwrap() as usize;
                let end = start
                    .saturating_add(operation["limit"].as_u64().unwrap() as usize)
                    .min(bytes.len());
                if start > end {
                    return failure("invalid_request", "Read offset");
                }
                json!({"kind":"bytes","bytes":bytes[start..end],"revision":revision.to_string(),"eof":end==bytes.len()})
            }
            "parent_path" => {
                json!({"kind":"parent","path":if path==ROOT { None } else { Some(ROOT) }})
            }
            "child_path" | "resolve_path" => json!({"kind":"path","path":"object:missing"}),
            "relative_path" => {
                json!({"kind":"relative_path","components":[if path == FILE { "中文.txt" } else { "second.txt" }]})
            }
            "begin_write" => {
                self.next += 1;
                self.writes.insert(
                    self.next,
                    (
                        path.into(),
                        Vec::new(),
                        operation["size"].as_u64().unwrap() as usize,
                        operation["expected_revision"].as_str().map(str::to_owned),
                    ),
                );
                json!({"kind":"started","handle":self.next})
            }
            "append_write" => {
                let write = operation["write"].as_u64().unwrap();
                let Some((_, bytes, size, _)) = self.writes.get_mut(&write) else {
                    return failure("not_found", "Write ended");
                };
                let chunk: Vec<u8> = serde_json::from_value(operation["bytes"].clone()).unwrap();
                if operation["offset"].as_u64() != Some(bytes.len() as u64)
                    || bytes.len() + chunk.len() > *size
                {
                    return failure("invalid_request", "Write offset");
                }
                bytes.extend(chunk);
                json!({"kind":"ok"})
            }
            "commit_write" => {
                let Some((path, bytes, size, expected)) =
                    self.writes.remove(&operation["write"].as_u64().unwrap())
                else {
                    return failure("not_found", "Write ended");
                };
                let revision = self.files.get(&path).map_or(0, |(_, revision)| *revision);
                if bytes.len() != size
                    || expected
                        .as_ref()
                        .is_some_and(|v| v != &revision.to_string())
                {
                    return failure("conflict", "Revision changed");
                }
                self.files.insert(path, (bytes, revision + 1));
                self.commits += 1;
                json!({"kind":"written","revision":(revision+1).to_string()})
            }
            "abort_write" => {
                self.writes.remove(&operation["write"].as_u64().unwrap());
                json!({"kind":"ok"})
            }
            _ => return failure("unsupported", "Memory fixture capability unavailable"),
        };
        json!({"Ok":answer})
    }
}
