//! A scriptable stand-in for a language server, used by the LSP client's tests.
//!
//! It is a real process that speaks the protocol over stdin and stdout, so the
//! tests exercise pipes, threads and process control without needing rust-analyzer.
//! Flags choose how it behaves; see `main` for the list. Every message it receives
//! is appended to the file named by `--log` as one JSON object per line.
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::time::Duration;

struct Options {
    args: Vec<String>,
}

impl Options {
    fn flag(&self, name: &str) -> bool {
        self.args.iter().any(|a| a == name)
    }
    fn value(&self, name: &str) -> Option<&str> {
        let at = self.args.iter().position(|a| a == name)?;
        self.args.get(at + 1).map(String::as_str)
    }
    fn number(&self, name: &str) -> u64 {
        self.value(name).and_then(|v| v.parse().ok()).unwrap_or(0)
    }
}

fn read_message(input: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

struct Output<'a> {
    options: &'a Options,
}

impl Output<'_> {
    fn frame(body: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(body).unwrap();
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend(body);
        frame
    }

    fn write(&self, bytes: &[u8]) {
        let mut out = std::io::stdout().lock();
        let chunk = self.options.number("--chunk") as usize;
        if chunk == 0 {
            let _ = out.write_all(bytes);
            let _ = out.flush();
            return;
        }
        // One small piece at a time, so the client sees headers and bodies split anywhere.
        for piece in bytes.chunks(chunk) {
            let _ = out.write_all(piece);
            let _ = out.flush();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn send(&self, body: &Value) {
        let mut bytes = Self::frame(body);
        if self.options.flag("--glue") {
            // The answer and a notification in one write: two messages in one read.
            bytes.extend(Self::frame(&json!({
                "jsonrpc": "2.0", "method": "window/logMessage",
                "params": { "type": 4, "message": "glued" }
            })));
        }
        self.write(&bytes);
    }
}

/// Where a protocol position falls in `text`, counting columns in UTF-8 bytes or
/// UTF-16 units: an independent reading of the protocol, so the client's ranges
/// are checked against something other than its own position code.
fn offset_of(text: &str, position: &Value, utf8: bool) -> usize {
    let line = position["line"].as_u64().unwrap_or(0) as usize;
    let column = position["character"].as_u64().unwrap_or(0) as usize;
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut at = 0;
    let mut current = 0;
    while current < line {
        match bytes.get(at) {
            None => return text.len(),
            Some(b'\n') => {
                at += 1;
                start = at;
                current += 1;
            }
            Some(b'\r') => {
                at += 1;
                if bytes.get(at) == Some(&b'\n') {
                    at += 1;
                }
                start = at;
                current += 1;
            }
            Some(_) => at += 1,
        }
    }
    let mut units = 0;
    let mut offset = start;
    for ch in text[start..].chars() {
        if ch == '\n' || ch == '\r' || units >= column {
            break;
        }
        units += if utf8 { ch.len_utf8() } else { ch.len_utf16() };
        offset += ch.len_utf8();
    }
    offset
}

fn apply_changes(text: &mut String, changes: &Value, utf8: bool) {
    for change in changes.as_array().into_iter().flatten() {
        let new_text = change["text"].as_str().unwrap_or_default();
        match change.get("range") {
            None => *text = new_text.to_owned(),
            Some(range) => {
                let start = offset_of(text, &range["start"], utf8);
                let end = offset_of(text, &range["end"], utf8);
                text.replace_range(start..end.max(start), new_text);
            }
        }
    }
}

/// Provider capabilities for `--features a,b,c`.
fn feature_capabilities(options: &Options, caps: &mut serde_json::Map<String, Value>) {
    let Some(list) = options.value("--features") else {
        return;
    };
    for feature in list.split(',') {
        match feature {
            "hover" => caps.insert("hoverProvider".into(), json!(true)),
            "definition" => caps.insert("definitionProvider".into(), json!(true)),
            "declaration" => caps.insert("declarationProvider".into(), json!(true)),
            "typeDefinition" => caps.insert("typeDefinitionProvider".into(), json!(true)),
            "implementation" => caps.insert("implementationProvider".into(), json!(true)),
            "references" => caps.insert("referencesProvider".into(), json!(true)),
            "highlight" => caps.insert("documentHighlightProvider".into(), json!(true)),
            "symbols" => caps.insert("documentSymbolProvider".into(), json!(true)),
            "workspaceSymbols" => caps.insert("workspaceSymbolProvider".into(), json!(true)),
            "completion" => caps.insert(
                "completionProvider".into(),
                json!({ "triggerCharacters": ["."], "resolveProvider": true }),
            ),
            "signature" => caps.insert(
                "signatureHelpProvider".into(),
                json!({ "triggerCharacters": ["("], "retriggerCharacters": [","] }),
            ),
            "rename" => caps.insert("renameProvider".into(), json!({ "prepareProvider": true })),
            "codeAction" => caps.insert(
                "codeActionProvider".into(),
                json!({ "resolveProvider": true }),
            ),
            "formatting" => caps.insert("documentFormattingProvider".into(), json!(true)),
            "rangeFormatting" => caps.insert("documentRangeFormattingProvider".into(), json!(true)),
            "onType" => caps.insert(
                "documentOnTypeFormattingProvider".into(),
                json!({ "firstTriggerCharacter": ";", "moreTriggerCharacter": ["}"] }),
            ),
            "inlay" => caps.insert("inlayHintProvider".into(), json!(true)),
            "folding" => caps.insert("foldingRangeProvider".into(), json!(true)),
            "selection" => caps.insert("selectionRangeProvider".into(), json!(true)),
            "callHierarchy" => caps.insert("callHierarchyProvider".into(), json!(true)),
            "typeHierarchy" => caps.insert("typeHierarchyProvider".into(), json!(true)),
            "codeLens" => caps.insert(
                "codeLensProvider".into(),
                json!({ "resolveProvider": options.flag("--lazy-lens") }),
            ),
            "fileOps" => caps.insert(
                "workspace".into(),
                json!({ "fileOperations": {
                    "didCreate": { "filters": [{ "pattern": { "glob": "**/*.rs" } }] },
                    "didRename": { "filters": [{ "pattern": { "glob": "**/*.rs" } }] },
                    "didDelete": { "filters": [{ "pattern": { "glob": "**/*.rs" } }] } } }),
            ),
            "pull" => caps.insert(
                "diagnosticProvider".into(),
                json!({ "interFileDependencies": false, "workspaceDiagnostics": false }),
            ),
            "execute" => caps.insert(
                "executeCommandProvider".into(),
                json!({ "commands": ["fake.run"] }),
            ),
            _ => None,
        };
    }
}

fn location(uri: &Value, line: u32, from: u32, to: u32) -> Value {
    json!({ "uri": uri, "range": {
        "start": { "line": line, "character": from },
        "end": { "line": line, "character": to } } })
}

fn span(line: u32, from: u32, to: u32) -> Value {
    json!({ "start": { "line": line, "character": from },
            "end": { "line": line, "character": to } })
}

/// The canned answer to an interactive request, so tests can tell what was asked
/// from what came back. `None` for methods the fake does not know.
fn feature_result(method: &str, params: &Value, options: &Options) -> Option<Value> {
    let uri = &params["textDocument"]["uri"];
    let uri_text = uri.as_str().unwrap_or("");
    let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
    let character = params["position"]["character"].as_u64().unwrap_or(0) as u32;
    let tag = options.value("--tag").unwrap_or("fake");
    Some(match method {
        "textDocument/hover" => {
            if options.flag("--hover-null") {
                Value::Null
            } else {
                json!({ "contents": { "kind": "markdown",
                    "value": format!("hover {line}:{character} from {tag}") },
                    "range": span(line, character, character + 1) })
            }
        }
        "textDocument/definition"
        | "textDocument/declaration"
        | "textDocument/typeDefinition"
        | "textDocument/implementation" => json!([location(uri, 0, 0, 1)]),
        "textDocument/references" => json!([location(uri, 0, 0, 1), location(uri, 1, 2, 4)]),
        "textDocument/documentHighlight" => {
            json!([{ "range": span(0, 0, 2), "kind": 2 }, { "range": span(1, 0, 2), "kind": 3 }])
        }
        "textDocument/documentSymbol" => json!([{
            "name": "main", "kind": 12,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 2, "character": 1 } },
            "selectionRange": span(0, 3, 7),
            "children": []
        }]),
        "workspace/symbol" => json!([{
            "name": format!("sym-{}", params["query"].as_str().unwrap_or("")),
            "kind": 12, "location": { "uri": if cfg!(windows) { "file:///C:/elsewhere.rs" } else { "file:///elsewhere.rs" } }
        }]),
        "textDocument/completion" if options.value("--completion-items-file").is_some() => {
            let bytes = std::fs::read(options.value("--completion-items-file").unwrap()).ok()?;
            serde_json::from_slice(&bytes).ok()?
        }
        "textDocument/completion" => json!({
            "isIncomplete": options.flag("--completion-incomplete"),
            "items": [
                { "label": format!("alpha-{tag}"), "kind": 3, "sortText": "1" },
                { "label": "beta", "kind": 6, "insertText": options.value("--completion-snippet-file").and_then(|path| std::fs::read_to_string(path).ok()).unwrap_or_else(|| "beta($1)$0".into()),
                  "insertTextFormat": 2, "data": { "n": 2 } },
            ]
        }),
        "completionItem/resolve" => {
            let mut item = params.clone();
            item["documentation"] = json!({ "kind": "markdown", "value": "resolved docs" });
            item
        }
        "textDocument/signatureHelp" if options.value("--signature-file").is_some() => {
            let bytes = std::fs::read(options.value("--signature-file").unwrap()).ok()?;
            serde_json::from_slice(&bytes).ok()?
        }
        "textDocument/signatureHelp" => json!({
            "signatures": [{ "label": "fn f(a: i32, b: i32)",
                "parameters": [{ "label": "a: i32" }, { "label": "b: i32" }] }],
            "activeSignature": 0, "activeParameter": 1
        }),
        "textDocument/prepareRename" => json!({
            "range": span(line, character, character + 1),
            "placeholder": "name"
        }),
        "textDocument/rename" => {
            if let Some(version) = options
                .value("--edit-version")
                .and_then(|s| s.parse::<i64>().ok())
            {
                json!({"documentChanges":[{"textDocument":{"uri":uri,"version":version},"edits":[{"range":span(0,0,1),"newText":params["newName"]}]}]})
            } else {
                json!({ "changes": { uri_text: [{"range": span(0, 0, 1), "newText": params["newName"] }] } })
            }
        }
        "textDocument/codeAction" if options.value("--code-actions-file").is_some() => {
            let bytes = std::fs::read(options.value("--code-actions-file").unwrap()).ok()?;
            serde_json::from_slice(&bytes).ok()?
        }
        "textDocument/codeAction" => json!([
            { "title": "Fix it", "kind": "quickfix", "isPreferred": true,
              "edit": { "changes": { uri_text: [{
                  "range": span(0, 0, 0), "newText": "// fixed\n" }] } } },
            { "title": "Lazy", "kind": "refactor", "data": 7 }
        ]),
        "codeAction/resolve" => {
            let mut action = params.clone();
            action["command"] = json!({ "title": "Run", "command": "fake.run" });
            action
        }
        "textDocument/formatting" | "textDocument/rangeFormatting" => {
            json!([{ "range": span(0, 0, 0), "newText": "// formatted\n" }])
        }
        "textDocument/onTypeFormatting" => {
            json!([{ "range": span(0, 0, 0), "newText": format!("// after {}\n", params["ch"].as_str().unwrap_or("")) }])
        }
        "textDocument/inlayHint" => json!([{
            "position": { "line": 0, "character": 3 }, "label": ": i32", "kind": 1 }]),
        "textDocument/foldingRange" => json!([{ "startLine": 0, "endLine": 2 }]),
        "textDocument/selectionRange" => json!([{
            "range": span(0, 1, 2),
            "parent": { "range": span(0, 0, 9) } }]),
        "textDocument/prepareCallHierarchy" | "textDocument/prepareTypeHierarchy" => json!([{
            "name": "f", "kind": 12, "uri": uri,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 2, "character": 1 } },
            "selectionRange": span(0, 3, 4),
            "data": "item" }]),
        "callHierarchy/incomingCalls" => json!([{
            "from": params["item"], "fromRanges": [span(1, 0, 1)] }]),
        "callHierarchy/outgoingCalls" => json!([{
            "to": params["item"], "fromRanges": [span(1, 0, 1)] }]),
        "typeHierarchy/supertypes" | "typeHierarchy/subtypes" => json!([params["item"]]),
        "textDocument/codeLens" if options.flag("--lazy-lens") => json!([{
            "range": span(0, 0, 4), "data": { "id": 7 } }]),
        "textDocument/codeLens" => json!([{
            "range": span(0, 0, 4),
            "command": { "title": "Run test", "command": "fake.run", "arguments": [1] } }]),
        "codeLens/resolve" => {
            let mut lens = params.clone();
            lens["command"] =
                json!({"title":"Run resolved test","command":"fake.run","arguments":[7]});
            lens
        }
        "workspace/executeCommand" => json!({ "ran": params["command"] }),
        _ => return None,
    })
}

fn capabilities(options: &Options) -> Value {
    let mut caps = serde_json::Map::new();
    match options.value("--encoding") {
        Some("utf-8") => caps.insert("positionEncoding".into(), json!("utf-8")),
        Some("utf-16") => caps.insert("positionEncoding".into(), json!("utf-16")),
        Some("utf-32") => caps.insert("positionEncoding".into(), json!("utf-32")),
        _ => None,
    };
    match options.value("--sync") {
        Some("full") => caps.insert("textDocumentSync".into(), json!(1)),
        Some("incremental") => caps.insert("textDocumentSync".into(), json!(2)),
        Some("object") => caps.insert(
            "textDocumentSync".into(),
            json!({ "openClose": true, "change": 2, "save": { "includeText": true } }),
        ),
        _ => None,
    };
    let legend =
        json!({ "tokenTypes": ["keyword", "function"], "tokenModifiers": ["declaration"] });
    match options.value("--semantic") {
        Some("full") => caps.insert(
            "semanticTokensProvider".into(),
            json!({ "legend": legend, "full": true }),
        ),
        Some("delta") => caps.insert(
            "semanticTokensProvider".into(),
            json!({ "legend": legend, "full": { "delta": true } }),
        ),
        Some("range") => caps.insert(
            "semanticTokensProvider".into(),
            json!({ "legend": legend, "range": true }),
        ),
        _ => None,
    };
    feature_capabilities(options, &mut caps);
    Value::Object(caps)
}

// A save notification is an explicit test barrier: keep reading cancellation
// and document updates while a reply is held, rather than sleeping in the pipe loop.
fn feature_reply(out: &Output<'_>, options: &Options, deferred: &mut Vec<Value>, reply: Value) {
    if options.flag("--defer-feature-replies") {
        deferred.push(reply);
    } else {
        out.send(&reply);
    }
}

fn main() {
    let options = Options {
        args: std::env::args().skip(1).collect(),
    };
    let out = Output { options: &options };
    let log = |value: &Value| {
        if let Some(path) = options.value("--log")
            && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
        {
            // A replacement server can initialize while the previous process
            // logs shutdown. Formatting into the file performs many appends,
            // which can interleave and corrupt both records. Append one encoded
            // message at a time so shared logs keep complete JSON lines.
            let mut record = serde_json::to_vec(value).unwrap();
            record.push(b'\n');
            let _ = file.write_all(&record);
        }
    };
    let mut input = std::io::BufReader::new(std::io::stdin().lock());
    let mut shutdown_seen = false;
    let mut feature_errors = std::collections::HashSet::<String>::new();
    let mut feature_successes = std::collections::HashSet::<String>::new();
    let mut deferred_features = Vec::new();
    let mut save_seen = false;
    let mut registration_step = 0;
    let registration_script: Vec<Value> = options
        .value("--registration-script")
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    // Release these on didSave so a test can reopen a document before any old
    // publication arrives, without relying on sleeps or thread scheduling.
    let mut deferred_diagnostics = Vec::new();
    let mut last_uri = Value::Null;
    // --mirror FILE: the text of every open document as this server understood the
    // changes it was sent, rewritten after each one.
    let mut mirror: std::collections::BTreeMap<String, String> = Default::default();
    let utf8 = options.value("--encoding") == Some("utf-8");
    while let Some(message) = read_message(&mut input) {
        log(&message);
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let id = message.get("id").cloned();
        if options.value("--crash-on") == Some(method) && !method.is_empty() {
            std::process::exit(4);
        }
        match (method, id) {
            ("initialize", Some(id)) => {
                if options.flag("--ignore-initialize") {
                    continue;
                }
                if options.flag("--init-error") {
                    out.send(&json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32603, "message": "no usable workspace" }
                    }));
                    continue;
                }
                std::thread::sleep(Duration::from_millis(options.number("--init-delay-ms")));
                out.send(&json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "capabilities": capabilities(&options), "serverInfo": { "name": "fake" } }
                }));
            }
            ("initialized", None) => {
                if options.flag("--stderr-noise") {
                    eprintln!("fake server: starting up");
                    eprintln!("fake server: ready");
                }
                if options.flag("--ask-config") {
                    out.send(&json!({
                        "jsonrpc": "2.0", "id": 9001, "method": "workspace/configuration",
                        "params": { "items": [{ "section": "fake" }, { "section": "missing" }] }
                    }));
                }
                if options.flag("--ask-folders") {
                    out.send(&json!({ "jsonrpc": "2.0", "id": 9004, "method": "workspace/workspaceFolders" }));
                }
                if options.flag("--ask-unknown") {
                    out.send(&json!({
                        "jsonrpc": "2.0", "id": 9002, "method": "client/unheardOf",
                        "params": {}
                    }));
                }
                for request in registration_script.iter().filter(|request| {
                    request.get("onMethod").is_none() && request["step"].as_u64().unwrap_or(0) == 0
                }) {
                    out.send(&json!({"jsonrpc":"2.0", "id":request["id"], "method":request["method"], "params":request["params"]}));
                }
                if options.flag("--progress") || options.flag("--progress-pair") {
                    let tokens = if options.flag("--progress-pair") {
                        vec![json!(7), json!("7")]
                    } else {
                        vec![json!("load")]
                    };
                    for token in tokens {
                        out.send(&json!({
                        "jsonrpc": "2.0", "id": 9200, "method": "window/workDoneProgress/create",
                        "params": { "token": token }
                    }));
                        out.send(&json!({
                        "jsonrpc": "2.0", "method": "$/progress",
                        "params": { "token": token, "value": {
                            "kind": "begin", "title": "Loading project", "message": "0/10", "percentage": 0,
                            "cancellable": options.flag("--progress-cancellable") } }
                    }));
                        out.send(&json!({
                            "jsonrpc": "2.0", "method": "$/progress",
                            "params": { "token": token, "value": {
                                "kind": "report", "message": "3/10", "percentage": 30 } }
                        }));
                    }
                }
                if options.flag("--log-message") {
                    out.send(&json!({
                        "jsonrpc": "2.0", "method": "window/logMessage",
                        "params": { "type": 3, "message": "hello from the fake server" }
                    }));
                }
                if options.flag("--garbage-after-init") {
                    let junk = b"this is not json";
                    let mut frame = format!("Content-Length: {}\r\n\r\n", junk.len()).into_bytes();
                    frame.extend_from_slice(junk);
                    out.write(&frame);
                }
                if options.flag("--bad-frame") {
                    out.write(b"garbage without a colon\r\n\r\n");
                }
                if options.flag("--oversize") {
                    out.write(b"Content-Length: 99999999\r\n\r\n");
                }
                if options.flag("--crash-after-init") {
                    std::process::exit(3);
                }
            }
            ("textDocument/didOpen" | "textDocument/didChange", None) => {
                // --diagnostics: one error over the first two characters, tagged
                // with the document version unless --diag-unversioned.
                // --diag-once: only the first publication, like a server that sees no
                // change in its findings; --diag-delay-ms: publish late.
                let first = method == "textDocument/didOpen";
                {
                    let document = &message["params"]["textDocument"];
                    let uri = document["uri"].as_str().unwrap_or_default().to_owned();
                    last_uri = document["uri"].clone();
                    if first {
                        mirror.insert(
                            uri,
                            document["text"].as_str().unwrap_or_default().to_owned(),
                        );
                    } else if let Some(text) = mirror.get_mut(&uri) {
                        apply_changes(text, &message["params"]["contentChanges"], utf8);
                    }
                    if let Some(path) = options.value("--mirror") {
                        let _ = std::fs::write(path, serde_json::to_vec(&mirror).unwrap());
                    }
                }
                if options.flag("--diagnostics") && (first || !options.flag("--diag-once")) {
                    std::thread::sleep(Duration::from_millis(options.number("--diag-delay-ms")));
                    let document = &message["params"]["textDocument"];
                    let mut params = json!({
                        "uri": document["uri"],
                        "diagnostics": [{
                            "range": { "start": { "line": 0, "character": 0 },
                                       "end": { "line": 0, "character": 2 } },
                            "severity": 1, "source": "fake",
                            "message": options.value("--diag-message").unwrap_or("fake error")
                        }]
                    });
                    if !options.flag("--diag-unversioned") {
                        params["version"] = document["version"].clone();
                    }
                    if let Some(url) = options.value("--diag-code-url") {
                        params["diagnostics"][0]["code"] = json!(7);
                        params["diagnostics"][0]["codeDescription"] = json!({"href":url});
                    }
                    let publication = json!({
                        "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params
                    });
                    if options.flag("--diag-defer-until-save") {
                        deferred_diagnostics.push(publication);
                    } else {
                        out.send(&publication);
                    }
                }
            }
            ("textDocument/didClose", None) => {
                mirror.remove(
                    message["params"]["textDocument"]["uri"]
                        .as_str()
                        .unwrap_or_default(),
                );
                if let Some(path) = options.value("--mirror") {
                    let _ = std::fs::write(path, serde_json::to_vec(&mirror).unwrap());
                }
            }
            ("textDocument/didSave", None) => {
                save_seen = true;
                for reply in deferred_features.drain(..) {
                    out.send(&reply);
                }
                registration_step += 1;
                for request in registration_script
                    .iter()
                    .filter(|request| request["step"].as_u64() == Some(registration_step))
                {
                    out.send(&json!({"jsonrpc":"2.0", "id":request["id"], "method":request["method"], "params":request["params"]}));
                }
                for publication in deferred_diagnostics.drain(..) {
                    out.send(&publication);
                }
                if options.flag("--progress") {
                    out.send(&json!({
                        "jsonrpc": "2.0", "method": "$/progress",
                        "params": { "token": "load", "value": { "kind": "end" } }
                    }));
                }
                if options.flag("--refresh-on-save") {
                    out.send(&json!({
                        "jsonrpc": "2.0", "id": 9100, "method": "workspace/semanticTokens/refresh"
                    }));
                }
                if options.flag("--pull-refresh-on-save") {
                    out.send(&json!({ "jsonrpc": "2.0", "id": 9101, "method": "workspace/diagnostic/refresh" }));
                }
            }
            ("window/workDoneProgress/cancel", None) => {
                if !options.flag("--ignore-progress-cancel") {
                    out.send(&json!({ "jsonrpc": "2.0", "method": "$/progress",
                        "params": { "token": message["params"]["token"], "value": { "kind": "end" } } }));
                }
            }
            ("textDocument/diagnostic", Some(id)) => {
                // --pull-diagnostic TEXT: one error over the first character, the
                // same resultId every time so "unchanged" can be answered.
                let previous = message["params"]["previousResultId"].as_str();
                if save_seen && options.flag("--pull-refresh-on-save") {
                    out.send(&json!({ "jsonrpc": "2.0", "id": id, "result": {
                        "kind": "full", "resultId": "d2", "items": [] } }));
                } else if previous == Some("d1") && !options.flag("--pull-always-full") {
                    out.send(&json!({ "jsonrpc": "2.0", "id": id, "result": {
                        "kind": "unchanged", "resultId": "d1" } }));
                } else {
                    out.send(&json!({ "jsonrpc": "2.0", "id": id, "result": {
                        "kind": "full", "resultId": "d1", "items": [{
                            "range": span(0, 0, 1), "severity": 2,
                            "message": options.value("--pull-diagnostic").unwrap_or("pulled") }] } }));
                }
            }
            ("textDocument/semanticTokens/full/delta", Some(id)) => {
                // Replaces the one token of the previous result by a longer one.
                out.send(&json!({ "jsonrpc": "2.0", "id": id, "result": {
                    "resultId": "r2",
                    "edits": [{ "start": 0, "deleteCount": 5, "data": [0, 0, 3, 0, 0] }] } }));
            }
            (
                "textDocument/semanticTokens/full" | "textDocument/semanticTokens/range",
                Some(id),
            ) => {
                std::thread::sleep(Duration::from_millis(options.number("--slow-ms")));
                // The first characters (two, or --semantic-length) are a keyword.
                let result = if options.flag("--semantic-null") {
                    Value::Null
                } else {
                    let length = match options.number("--semantic-length") {
                        0 => 2,
                        length => length,
                    };
                    json!({ "resultId": "r1", "data": [0, 0, length, 0, 0] })
                };
                out.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            }
            ("shutdown", Some(id)) => {
                shutdown_seen = true;
                if !options.flag("--no-shutdown-reply") {
                    out.send(&json!({ "jsonrpc": "2.0", "id": id, "result": null }));
                }
            }
            ("exit", None) => {
                if options.flag("--linger") {
                    // Ignore the exit request: the client has to end the process.
                    std::thread::sleep(Duration::from_secs(60));
                }
                std::process::exit(if shutdown_seen { 0 } else { 1 });
            }
            (_, Some(id))
                if options.value("--features").is_some()
                    && feature_result(method, &Value::Null, &options).is_some()
                    && !options.flag("--ignore-requests") =>
            {
                for request in registration_script
                    .iter()
                    .filter(|request| request["onMethod"].as_str() == Some(method))
                {
                    out.send(&json!({"jsonrpc":"2.0", "id":request["id"], "method":request["method"], "params":request["params"]}));
                }
                std::thread::sleep(Duration::from_millis(options.number("--slow-ms")));
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                if options.flag("--feature-error")
                    || (options.flag("--feature-error-once")
                        && feature_errors.insert(method.to_owned()))
                {
                    let code = options
                        .value("--feature-error-code")
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(-32801);
                    feature_reply(
                        &out,
                        &options,
                        &mut deferred_features,
                        json!({ "jsonrpc": "2.0", "id": id,
                        "error": { "code": code, "message": "content modified",
                            "data": { "retriggerRequest": !options.flag("--do-not-retrigger") } } }),
                    );
                    continue;
                }
                // Leave successful feature requests pending while continuing to
                // read messages, so cancellation tests need no timing window.
                if options.flag("--ignore-feature-success")
                    || (options.flag("--ignore-first-feature-success")
                        && feature_successes.insert(method.to_owned()))
                {
                    continue;
                }
                if method == "workspace/executeCommand" && options.flag("--exec-edit") {
                    out.send(&json!({ "jsonrpc": "2.0", "id": 9300, "method": "workspace/applyEdit",
                        "params": { "label": "by the server", "edit": { "changes": { last_uri.as_str().unwrap_or(""): [{
                            "range": span(0, 0, 0), "newText": "// by server\n" }] } } } }));
                }
                if method == "workspace/executeCommand" && options.flag("--exec-show") {
                    out.send(
                        &json!({ "jsonrpc": "2.0", "id": 9301, "method": "window/showDocument",
                        "params": { "uri": last_uri, "selection": span(1, 0, 3) } }),
                    );
                }
                let result = feature_result(method, &params, &options).unwrap_or(Value::Null);
                feature_reply(
                    &out,
                    &options,
                    &mut deferred_features,
                    json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                );
            }
            (_, Some(id)) if !method.is_empty() => {
                if options.flag("--ignore-requests") {
                    continue;
                }
                std::thread::sleep(Duration::from_millis(options.number("--slow-ms")));
                // Answer with what was asked, so tests see exactly what arrived.
                out.send(&json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "method": method, "params": message.get("params").cloned().unwrap_or(Value::Null) }
                }));
            }
            _ => {}
        }
    }
    // The client closed our input.
    std::process::exit(if shutdown_seen { 0 } else { 2 });
}
