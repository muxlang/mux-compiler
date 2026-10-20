use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

fn send(stdin: &mut ChildStdin, message: Value) {
    let body = serde_json::to_vec(&message).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    stdin.write_all(&body).unwrap();
    stdin.flush().unwrap();
}

fn receive(stdout: &mut BufReader<ChildStdout>) -> Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; length.expect("content length header")];
    stdout.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn receive_response(stdout: &mut BufReader<ChildStdout>, id: u64) -> Value {
    loop {
        let message = receive(stdout);
        if message["id"] == id {
            return message;
        }
    }
}

#[test]
fn formatting_respects_project_disable_setting() {
    let root = std::env::temp_dir().join(format!("mux-lsp-format-disabled-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("mux-project.json"),
        r#"{"format":{"enabled":false}}"#,
    )
    .unwrap();
    let path = root.join("main.mux");
    let uri = url::Url::from_file_path(path).unwrap().to_string();
    let root_uri = url::Url::from_directory_path(&root).unwrap().to_string();
    let (mut child, mut stdin, mut stdout) = start_server();

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "workspace": {"workspaceEdit": {"documentChanges": true}}
                },
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": "format-disabled"}]
            }
        }),
    );
    assert_eq!(receive_response(&mut stdout, 1)["id"], 1);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "mux",
                    "version": 1,
                    "text": "func main() returns void {\nprint(1+2)\n}\n"
                }
            }
        }),
    );
    assert_eq!(
        receive(&mut stdout)["method"],
        "textDocument/publishDiagnostics"
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/formatting",
            "params": {
                "textDocument": {"uri": uri},
                "options": {"tabSize": 4, "insertSpaces": true}
            }
        }),
    );
    assert_eq!(receive_response(&mut stdout, 2)["result"], json!([]));

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive_response(&mut stdout, 3)["id"], 3);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(root).unwrap();
}

fn start_server() -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mux"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start mux lsp");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    (child, stdin, stdout)
}

#[test]
fn workspace_references_rename_hints_and_folding_use_compiler_analysis() {
    let root = std::env::temp_dir().join(format!("mux-lsp-workspace-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let tools_path = root.join("tools.mux");
    let main_path = root.join("main.mux");
    let other_path = root.join("other.mux");
    std::fs::write(
        &tools_path,
        "class Tool {\n    int value = 0\n    func get() returns int { return self.value }\n}\nfunc helper(int value) returns int { return value }\n",
    )
    .unwrap();
    let main_source = "import tools.(helper as assist, Tool)\nfunc example() returns int {\n    auto answer = assist(1)\n    auto tool = Tool.new()\n    return tool.get() + answer\n}\n";
    std::fs::write(
        &other_path,
        "import tools.(helper as assist, Tool)\nfunc other() returns int {\n    auto tool = Tool.new()\n    return tool.get() + assist(2)\n}\n",
    )
    .unwrap();
    let main_uri = url::Url::from_file_path(&main_path).unwrap().to_string();
    let tools_uri = url::Url::from_file_path(&tools_path).unwrap().to_string();
    let other_uri = url::Url::from_file_path(&other_path).unwrap().to_string();
    let root_uri = url::Url::from_directory_path(&root).unwrap().to_string();
    let (mut child, mut stdin, mut stdout) = start_server();

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "workspace": {"workspaceEdit": {"documentChanges": true}}
                },
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": "workspace"}]
            }
        }),
    );
    let initialized = receive_response(&mut stdout, 1);
    let capabilities = &initialized["result"]["capabilities"];
    assert_eq!(capabilities["referencesProvider"], true);
    assert_eq!(capabilities["renameProvider"], true);
    assert_eq!(capabilities["inlayHintProvider"], true);
    assert_eq!(capabilities["foldingRangeProvider"], true);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":main_uri,"languageId":"mux","version":1,"text":main_source}}
        }),
    );
    let _diagnostics = receive(&mut stdout);

    let helper_character = main_source.lines().next().unwrap().find("helper").unwrap();
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"textDocument/references",
            "params":{
                "textDocument":{"uri":main_uri},
                "position":{"line":0,"character":helper_character},
                "context":{"includeDeclaration":true}
            }
        }),
    );
    let references = receive_response(&mut stdout, 2)["result"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(references.len(), 5, "{references:?}");
    assert!(
        references
            .iter()
            .any(|location| location["uri"] == tools_uri)
    );
    assert!(
        references
            .iter()
            .any(|location| location["uri"] == other_uri)
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"textDocument/rename",
            "params":{
                "textDocument":{"uri":main_uri},
                "position":{"line":0,"character":helper_character},
                "newName":"compute"
            }
        }),
    );
    let rename = receive_response(&mut stdout, 3)["result"]["documentChanges"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rename.len(), 3, "{rename:?}");
    for uri in [&tools_uri, &main_uri, &other_uri] {
        assert!(
            rename
                .iter()
                .any(|change| change["textDocument"]["uri"] == uri.as_str())
        );
    }

    let alias_character = main_source.lines().next().unwrap().find("assist").unwrap();
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":8,
            "method":"textDocument/references",
            "params":{
                "textDocument":{"uri":main_uri},
                "position":{"line":0,"character":alias_character},
                "context":{"includeDeclaration":true}
            }
        }),
    );
    let alias_references = receive_response(&mut stdout, 8)["result"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(alias_references.len(), 2, "{alias_references:?}");

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":9,
            "method":"textDocument/rename",
            "params":{
                "textDocument":{"uri":main_uri},
                "position":{"line":0,"character":alias_character},
                "newName":"run_helper"
            }
        }),
    );
    let alias_rename = receive_response(&mut stdout, 9)["result"]["documentChanges"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(alias_rename.len(), 1, "{alias_rename:?}");
    assert_eq!(alias_rename[0]["textDocument"]["uri"], main_uri);

    let get_character = main_source.lines().nth(4).unwrap().find("get").unwrap();
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":6,
            "method":"textDocument/references",
            "params":{
                "textDocument":{"uri":main_uri},
                "position":{"line":4,"character":get_character},
                "context":{"includeDeclaration":true}
            }
        }),
    );
    let member_references = receive_response(&mut stdout, 6)["result"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(member_references.len(), 3, "{member_references:?}");

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":7,
            "method":"textDocument/rename",
            "params":{
                "textDocument":{"uri":main_uri},
                "position":{"line":4,"character":get_character},
                "newName":"read_value"
            }
        }),
    );
    let member_rename = receive_response(&mut stdout, 7)["result"]["documentChanges"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(member_rename.len(), 3, "{member_rename:?}");

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":4,
            "method":"textDocument/inlayHint",
            "params":{
                "textDocument":{"uri":main_uri},
                "range":{"start":{"line":0,"character":0},"end":{"line":4,"character":0}}
            }
        }),
    );
    let hints = receive_response(&mut stdout, 4)["result"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        hints.iter().any(|hint| hint["label"] == ": int"),
        "{hints:?}"
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":5,
            "method":"textDocument/foldingRange",
            "params":{"textDocument":{"uri":main_uri}}
        }),
    );
    let folds = receive_response(&mut stdout, 5)["result"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        !folds.is_empty(),
        "expected a fold for the multiline function: {folds:?}"
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":6, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive_response(&mut stdout, 6)["id"], 6);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn binding_diagnostics_count_where_reads_and_select_the_binding_name() {
    let (mut child, mut stdin, mut stdout) = start_server();
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"capabilities": {}}
        }),
    );
    assert_eq!(receive_response(&mut stdout, 1)["id"], 1);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );

    let path =
        std::env::temp_dir().join(format!("mux-lsp-binding-spans-{}.mux", std::process::id()));
    let uri = url::Url::from_file_path(path).unwrap().to_string();
    let source = "func constrained(int retries) where { retries >= 0 } returns void { return }\nfunc unused(int value) returns void { return }\nfunc local_unused() returns void {\n    int unused_local = 1\n    return\n}\n";
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "mux",
                    "version": 1,
                    "text": source
                }
            }
        }),
    );
    let published = receive(&mut stdout);
    let diagnostics = published["params"]["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics.iter().all(|diagnostic| {
            !diagnostic["message"]
                .as_str()
                .unwrap_or_default()
                .contains("retries")
        }),
        "where-clause references must count as reads: {diagnostics:?}"
    );
    let unused = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["message"] == "binding 'value' is never read")
        .expect("unused parameter warning");
    assert_eq!(unused["range"]["start"]["line"], 1);
    assert_eq!(
        unused["range"]["start"]["character"],
        source.lines().nth(1).unwrap().find("value").unwrap()
    );
    assert_eq!(
        unused["range"]["end"]["character"],
        source.lines().nth(1).unwrap().find("value").unwrap() + "value".len()
    );
    let local = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["message"] == "binding 'unused_local' is never read")
        .expect("unused local binding warning");
    assert_eq!(local["range"]["start"]["line"], 3);
    assert_eq!(
        local["range"]["start"]["character"],
        source.lines().nth(3).unwrap().find("unused_local").unwrap()
    );
    assert_eq!(
        local["range"]["end"]["character"],
        source.lines().nth(3).unwrap().find("unused_local").unwrap() + "unused_local".len()
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":2, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive_response(&mut stdout, 2)["id"], 2);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn type_hovers_show_signatures_and_adjacent_documentation() {
    let (mut child, mut stdin, mut stdout) = start_server();
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"capabilities": {}}
        }),
    );
    assert_eq!(receive_response(&mut stdout, 1)["id"], 1);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );

    let path = std::env::temp_dir().join(format!("mux-lsp-type-hover-{}.mux", std::process::id()));
    let uri = url::Url::from_file_path(path).unwrap().to_string();
    let source = "/// A calculator for arithmetic.\nclass Calculator {\n    int value\n}\nfunc consume(Calculator value) returns void { return }\n";
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {"uri": uri, "languageId": "mux", "version": 1, "text": source}
            }
        }),
    );
    let _diagnostics = receive(&mut stdout);

    let class_line = 1;
    let class_character = source
        .lines()
        .nth(class_line)
        .unwrap()
        .find("Calculator")
        .unwrap();
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": class_line, "character": class_character}
            }
        }),
    );
    let class_hover = receive_response(&mut stdout, 2);
    let class_hover = class_hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(class_hover.contains("A calculator for arithmetic."));
    assert!(class_hover.contains("class Calculator"));
    assert!(!class_hover.contains("int value"));

    let type_line = 4;
    let type_character = source
        .lines()
        .nth(type_line)
        .unwrap()
        .find("Calculator")
        .unwrap();
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/hover",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": type_line, "character": type_character}
            }
        }),
    );
    let type_hover = receive_response(&mut stdout, 3);
    assert!(
        type_hover["result"]["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("A calculator for arithmetic.")
    );

    let builtin_character = source.lines().nth(2).unwrap().find("int").unwrap();
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/hover",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": builtin_character}
            }
        }),
    );
    let builtin_hover = receive_response(&mut stdout, 4);
    assert!(
        builtin_hover["result"]["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("Built-in type `int`")
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":5, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive_response(&mut stdout, 5)["id"], 5);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn stdio_server_publishes_diagnostics_and_shuts_down_cleanly() {
    let (mut child, mut stdin, mut stdout) = start_server();
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"capabilities": {}}
        }),
    );
    let initialized = receive(&mut stdout);
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["capabilities"]["textDocumentSync"], 1);
    assert_eq!(
        initialized["result"]["capabilities"]["documentSymbolProvider"],
        true
    );
    assert_eq!(
        initialized["result"]["capabilities"]["definitionProvider"],
        true
    );
    assert_eq!(initialized["result"]["capabilities"]["hoverProvider"], true);
    assert_eq!(
        initialized["result"]["capabilities"]["workspace"]["workspaceFolders"]["supported"],
        true
    );
    assert!(initialized["result"]["capabilities"]["completionProvider"].is_object());
    assert!(initialized["result"]["capabilities"]["semanticTokensProvider"].is_object());
    assert!(initialized["result"]["capabilities"]["signatureHelpProvider"].is_object());

    let path = std::env::temp_dir().join("mux-lsp-smoke.mux");
    let uri = url::Url::from_file_path(path).unwrap().to_string();
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "mux",
                    "version": 1,
                    "text": "func main() returns void {\n    auto value = true\n"
                }
            }
        }),
    );
    let diagnostics = receive(&mut stdout);
    assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
    assert!(
        !diagnostics["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "textDocument/documentSymbol",
            "params": {"textDocument": {"uri": uri}}
        }),
    );
    let symbols = receive(&mut stdout);
    assert_eq!(symbols["id"], 5);
    assert_eq!(symbols["result"][0]["name"], "value");

    let source = "func main() returns void {\n auto value = true\n if value && true {\n  return\n } else {\n  return\n }\n}\n";
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": source}]
            }
        }),
    );
    let warning = receive(&mut stdout);
    let warning_diagnostics = warning["params"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        warning_diagnostics
            .iter()
            .any(|diagnostic| diagnostic["severity"] == 2)
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {"uri": uri, "version": 1},
                "contentChanges": [{"text": "func stale() returns void { return }\n"}]
            }
        }),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "textDocument/documentSymbol",
            "params": {"textDocument": {"uri": uri}}
        }),
    );
    let symbols = receive(&mut stdout);
    assert_eq!(symbols["id"], 6);
    assert_eq!(symbols["result"][0]["name"], "main");
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 6}
            }
        }),
    );
    let definition = receive(&mut stdout);
    assert_eq!(definition["id"], 7);
    assert_eq!(definition["result"]["range"]["start"]["line"], 1);
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "textDocument/hover",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 6}
            }
        }),
    );
    let hover = receive(&mut stdout);
    assert_eq!(hover["id"], 8);
    assert!(
        hover["result"]["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("bool")
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 8}
            }
        }),
    );
    let completions = receive(&mut stdout);
    assert_eq!(completions["id"], 9);
    assert!(
        completions["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "value")
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "textDocument/semanticTokens/full",
            "params": {"textDocument": {"uri": uri}}
        }),
    );
    let semantic_tokens = receive_response(&mut stdout, 10);
    assert_eq!(semantic_tokens["id"], 10);
    assert!(
        !semantic_tokens["result"]["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/codeAction",
            "params": {
                "textDocument": {"uri": uri},
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 6, "character": 0}},
                "context": {"diagnostics": warning_diagnostics}
            }
        }),
    );
    let actions = receive(&mut stdout);
    assert_eq!(actions["id"], 3);
    assert!(!actions["result"].as_array().unwrap().is_empty());
    assert!(
        actions["result"][0]["edit"]["changes"]
            .to_string()
            .contains("value")
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/formatting",
            "params": {
                "textDocument": {"uri": uri},
                "options": {"tabSize": 4, "insertSpaces": true}
            }
        }),
    );
    let formatting = receive(&mut stdout);
    assert_eq!(formatting["id"], 4);
    assert!(!formatting["result"].as_array().unwrap().is_empty());

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {"uri": uri, "version": 3},
                "contentChanges": [{"text": "func main() returns void { return }\n"}]
            }
        }),
    );
    let cleared_diagnostics = receive(&mut stdout);
    assert_eq!(cleared_diagnostics["params"]["version"], 3);
    assert!(
        cleared_diagnostics["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": uri}}
        }),
    );
    let closed_diagnostics = receive(&mut stdout);
    assert_eq!(
        closed_diagnostics["method"],
        "textDocument/publishDiagnostics"
    );
    assert!(
        closed_diagnostics["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":2, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive(&mut stdout)["id"], 2);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn untitled_documents_publish_diagnostics_and_keep_their_uri_for_definitions() {
    let (mut child, mut stdin, mut stdout) = start_server();
    let uri = "untitled:Untitled-1";
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"capabilities":{}}}),
    );
    assert_eq!(receive(&mut stdout)["id"], 1);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "mux",
                    "version": 1,
                    "text": "func main() returns void {\n    auto value = true\n    print(value)\n    return\n}\n"
                }
            }
        }),
    );
    let diagnostics = receive(&mut stdout);
    assert_eq!(diagnostics["params"]["uri"], uri);

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 12}
            }
        }),
    );
    let definition = receive_response(&mut stdout, 2);
    assert_eq!(definition["result"]["uri"], uri);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive_response(&mut stdout, 3)["id"], 3);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn queued_requests_can_be_cancelled_without_blocking_transport() {
    let (mut child, mut stdin, mut stdout) = start_server();
    let path = std::env::temp_dir().join("mux-lsp-cancellation.mux");
    let uri = url::Url::from_file_path(path).unwrap().to_string();
    let mut source = String::from("func main() returns void {\n");
    for index in 0..400 {
        source.push_str(&format!("    auto local_{index} = {index}\n"));
    }
    source.push_str("    \n}\n");

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"capabilities":{}}}),
    );
    assert_eq!(receive(&mut stdout)["id"], 1);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "mux",
                    "version": 1,
                    "text": source
                }
            }
        }),
    );
    let _diagnostics = receive(&mut stdout);

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/documentSymbol",
            "params": {"textDocument": {"uri": uri}}
        }),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 401, "character": 4}
            }
        }),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "$/cancelRequest",
            "params": {"id": 3}
        }),
    );
    let cancelled = receive_response(&mut stdout, 3);
    assert_eq!(cancelled["error"]["code"], -32800);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":4, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive_response(&mut stdout, 4)["id"], 4);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn watched_import_creation_refreshes_open_document_diagnostics() {
    let (mut child, mut stdin, mut stdout) = start_server();
    let root = std::env::temp_dir().join("mux-lsp-watch-smoke/main.mux");
    let imported = root.parent().unwrap().join("tools.mux");
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&imported);
    let root_uri = url::Url::from_file_path(&root).unwrap().to_string();
    let imported_uri = url::Url::from_file_path(&imported).unwrap().to_string();

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{"capabilities":{"workspace":{"didChangeWatchedFiles":{"dynamicRegistration":true}}}}
        }),
    );
    assert_eq!(receive(&mut stdout)["id"], 1);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );
    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "client/registerCapability");
    assert_eq!(
        registration["params"]["registrations"][0]["method"],
        "workspace/didChangeWatchedFiles"
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":"mux-register-mux-file-watcher",
            "result":null
        }),
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":root_uri,"languageId":"mux","version":1,"text":"import tools\nfunc main() returns void { return }\n"}}
        }),
    );
    let missing_import = receive(&mut stdout);
    assert_eq!(missing_import["method"], "textDocument/publishDiagnostics");
    assert!(
        !missing_import["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    std::fs::write(&imported, "func available() returns void { return }\n").unwrap();
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "method":"workspace/didChangeWatchedFiles",
            "params":{"changes":[{"uri":imported_uri,"type":1}]}
        }),
    );
    let refreshed = receive(&mut stdout);
    assert_eq!(refreshed["method"], "textDocument/publishDiagnostics");
    assert!(
        refreshed["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    std::fs::remove_file(&imported).unwrap();
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "method":"workspace/didChangeWatchedFiles",
            "params":{"changes":[{"uri":imported_uri,"type":3}]}
        }),
    );
    let removed = receive(&mut stdout);
    assert_eq!(removed["method"], "textDocument/publishDiagnostics");
    assert!(
        !removed["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":2, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive_response(&mut stdout, 2)["id"], 2);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let _ = std::fs::remove_file(imported);
    let _ = std::fs::remove_dir(root.parent().unwrap());
}

#[test]
fn definition_uses_open_imported_module_contents() {
    let (mut child, mut stdin, mut stdout) = start_server();
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"capabilities":{}}}),
    );
    assert_eq!(receive(&mut stdout)["id"], 1);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    );

    let root = std::env::temp_dir().join("mux-lsp-import-smoke/main.mux");
    let imported = root.parent().unwrap().join("tools.mux");
    let root_uri = url::Url::from_file_path(root).unwrap().to_string();
    let imported_uri = url::Url::from_file_path(imported).unwrap().to_string();
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": imported_uri,
                    "languageId": "mux",
                    "version": 1,
                    "text": "class User {\n    string name = \"\"\n    func greet() returns string { return self.name }\n}\nfunc helper() returns void { return }\n"
                }
            }
        }),
    );
    let _ = receive(&mut stdout);
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": root_uri,
                    "languageId": "mux",
                    "version": 1,
                    "text": "import tools.*\nfunc main() returns void {\n    helper()\n    auto user = User.new()\n    user.na\n    return\n}\n"
                }
            }
        }),
    );
    let _ = receive(&mut stdout);

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": root_uri},
                "position": {"line": 4, "character": 9}
            }
        }),
    );
    let completions = receive_response(&mut stdout, 5);
    let labels = completions["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"name"), "{labels:?}");
    assert!(labels.contains(&"greet"), "{labels:?}");

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": root_uri},
                "position": {"line": 3, "character": 0}
            }
        }),
    );
    let imported_completions = receive_response(&mut stdout, 6);
    let labels = imported_completions["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"User"), "{labels:?}");
    assert!(labels.contains(&"helper"), "{labels:?}");
    assert!(!labels.contains(&"name"), "{labels:?}");

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": {"uri": root_uri},
                "position": {"line": 2, "character": 6}
            }
        }),
    );
    let signature = receive_response(&mut stdout, 4);
    assert!(
        signature["result"]["signatures"][0]["label"]
            .as_str()
            .unwrap_or_default()
            .contains("helper()"),
        "{signature}"
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": root_uri},
                "position": {"line": 2, "character": 6}
            }
        }),
    );
    let definition = receive_response(&mut stdout, 2);
    assert_eq!(definition["result"]["uri"], imported_uri);
    assert_eq!(definition["result"]["range"]["start"]["line"], 4);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"shutdown", "params":null}),
    );
    assert_eq!(receive_response(&mut stdout, 3)["id"], 3);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"exit", "params":null}),
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
