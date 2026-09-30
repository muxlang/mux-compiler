use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types as lsp;
use url::Url;

#[derive(Clone)]
struct OpenDocument {
    uri: lsp::Uri,
    path: PathBuf,
    source: String,
    version: i32,
    revision: u64,
}

struct RequestTask {
    request: Request,
    documents: HashMap<String, OpenDocument>,
    workspace_folders: Vec<PathBuf>,
}

struct DiagnosticTask {
    documents: HashMap<String, OpenDocument>,
    revisions: HashMap<String, u64>,
    published_uris: std::collections::HashSet<String>,
}

#[derive(Default)]
struct PendingDiagnostics {
    latest: Option<DiagnosticTask>,
    signal_queued: bool,
}

enum WorkerTask {
    Request(RequestTask),
    Diagnostics,
}

struct RequestResult {
    id: RequestId,
    message: Message,
}

struct DiagnosticResult {
    revisions: HashMap<String, u64>,
    messages: Vec<Message>,
    published_uris: std::collections::HashSet<String>,
}

enum WorkerResult {
    Request(RequestResult),
    Diagnostics(DiagnosticResult),
}

struct ServerState {
    documents: HashMap<String, OpenDocument>,
    published_uris: std::collections::HashSet<String>,
    shutdown_requested: bool,
    outstanding: HashMap<RequestId, HashMap<String, u64>>,
    next_revision: u64,
    exiting: bool,
    task_sender: std::sync::mpsc::Sender<WorkerTask>,
    result_receiver: std::sync::mpsc::Receiver<WorkerResult>,
    cancelled: Arc<Mutex<std::collections::HashSet<RequestId>>>,
    current_revisions: Arc<Mutex<HashMap<String, u64>>>,
    pending_diagnostics: Arc<Mutex<PendingDiagnostics>>,
    workspace_folders: Vec<PathBuf>,
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (connection, io_threads) = Connection::stdio();
    let workspace_folders = initialize(&connection)?;
    let (task_sender, task_receiver) = std::sync::mpsc::channel::<WorkerTask>();
    let (result_sender, result_receiver) = std::sync::mpsc::channel::<WorkerResult>();
    let cancelled = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let current_revisions = Arc::new(Mutex::new(HashMap::new()));
    let pending_diagnostics = Arc::new(Mutex::new(PendingDiagnostics::default()));
    let stop_worker = Arc::new(AtomicBool::new(false));
    let worker = {
        let cancelled = Arc::clone(&cancelled);
        let current_revisions = Arc::clone(&current_revisions);
        let pending_diagnostics = Arc::clone(&pending_diagnostics);
        let stop_worker = Arc::clone(&stop_worker);
        thread::spawn(move || {
            analysis_worker(
                task_receiver,
                result_sender,
                cancelled,
                current_revisions,
                pending_diagnostics,
                stop_worker,
            )
        })
    };
    let mut state = ServerState {
        documents: HashMap::new(),
        published_uris: std::collections::HashSet::new(),
        shutdown_requested: false,
        outstanding: HashMap::new(),
        next_revision: 0,
        exiting: false,
        task_sender,
        result_receiver,
        cancelled,
        current_revisions,
        pending_diagnostics,
        workspace_folders,
    };
    let event_loop_result = server_event_loop(&connection, &mut state);
    let shutdown_requested = state.shutdown_requested;
    stop_worker.store(true, Ordering::Release);
    drop(state);
    let worker_result = worker
        .join()
        .map_err(|_| "language server request worker panicked");
    drop(connection);
    io_threads.join()?;
    event_loop_result?;
    worker_result?;
    if !shutdown_requested {
        return Err("received exit before shutdown".into());
    }
    Ok(())
}

fn initialize(connection: &Connection) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    let initialize_params = serde_json::from_value::<lsp::InitializeParams>(initialize_params)?;
    let watch_mux_files = initialize_params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files)
        .and_then(|watched_files| watched_files.dynamic_registration)
        == Some(true);
    let legacy_root = initialize_root_path(&initialize_params);
    let mut workspace_folders = initialize_params
        .workspace_folders
        .unwrap_or_default()
        .iter()
        .filter_map(|folder| workspace_uri_to_path(&folder.uri))
        .collect::<Vec<_>>();
    if workspace_folders.is_empty()
        && let Some(root) = legacy_root
    {
        workspace_folders.push(root);
    }
    let capabilities = lsp::ServerCapabilities {
        text_document_sync: Some(lsp::TextDocumentSyncCapability::Kind(
            lsp::TextDocumentSyncKind::FULL,
        )),
        document_formatting_provider: Some(lsp::OneOf::Left(true)),
        document_symbol_provider: Some(lsp::OneOf::Left(true)),
        definition_provider: Some(lsp::OneOf::Left(true)),
        hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
        completion_provider: Some(lsp::CompletionOptions::default()),
        signature_help_provider: Some(lsp::SignatureHelpOptions::default()),
        code_action_provider: Some(lsp::CodeActionProviderCapability::Simple(true)),
        workspace: Some(lsp::WorkspaceServerCapabilities {
            workspace_folders: Some(lsp::WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(lsp::OneOf::Left(true)),
            }),
            file_operations: None,
        }),
        ..lsp::ServerCapabilities::default()
    };
    let initialize_result = lsp::InitializeResult {
        capabilities,
        server_info: Some(lsp::ServerInfo {
            name: "mux-lsp".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    connection.initialize_finish(initialize_id, serde_json::to_value(initialize_result)?)?;
    if watch_mux_files {
        let watcher = lsp::FileSystemWatcher {
            glob_pattern: lsp::GlobPattern::String("**/*.mux".to_owned()),
            kind: Some(lsp::WatchKind::Create | lsp::WatchKind::Change | lsp::WatchKind::Delete),
        };
        let registration = lsp::Registration {
            id: "mux-watch-mux-files".to_owned(),
            method: "workspace/didChangeWatchedFiles".to_owned(),
            register_options: Some(serde_json::to_value(
                lsp::DidChangeWatchedFilesRegistrationOptions {
                    watchers: vec![watcher],
                },
            )?),
        };
        connection.sender.send(
            Request::new(
                "mux-register-mux-file-watcher".to_owned().into(),
                "client/registerCapability".to_owned(),
                lsp::RegistrationParams {
                    registrations: vec![registration],
                },
            )
            .into(),
        )?;
    }
    Ok(workspace_folders)
}

fn server_event_loop(
    connection: &Connection,
    state: &mut ServerState,
) -> Result<(), Box<dyn std::error::Error>> {
    while !state.exiting {
        match connection.receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(message) => handle_protocol_message(connection, state, message)?,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        send_worker_results(connection, state)?;
    }
    Ok(())
}

fn handle_protocol_message(
    connection: &Connection,
    state: &mut ServerState,
    message: Message,
) -> Result<(), Box<dyn std::error::Error>> {
    match message {
        Message::Request(request) => handle_request(connection, state, request),
        Message::Notification(notification) => handle_notification(connection, state, notification),
        Message::Response(_) => Ok(()),
    }
}

fn handle_request(
    connection: &Connection,
    state: &mut ServerState,
    request: Request,
) -> Result<(), Box<dyn std::error::Error>> {
    if request.method == "shutdown" {
        state.shutdown_requested = true;
        connection
            .sender
            .send(Response::new_ok(request.id, serde_json::Value::Null).into())?;
        return Ok(());
    }
    if !supported_request(&request.method) {
        connection.sender.send(
            Response::new_err(
                request.id,
                lsp_server::ErrorCode::MethodNotFound as i32,
                format!("unsupported request: {}", request.method),
            )
            .into(),
        )?;
        return Ok(());
    }

    state
        .outstanding
        .insert(request.id.clone(), document_versions(&state.documents));
    state
        .task_sender
        .send(WorkerTask::Request(RequestTask {
            request,
            documents: state.documents.clone(),
            workspace_folders: state.workspace_folders.clone(),
        }))
        .map_err(|_| "language server request worker stopped".into())
}

fn handle_notification(
    connection: &Connection,
    state: &mut ServerState,
    notification: Notification,
) -> Result<(), Box<dyn std::error::Error>> {
    match notification.method.as_str() {
        "exit" => state.exiting = true,
        "$/cancelRequest" => handle_cancellation(connection, state, notification)?,
        "workspace/didChangeWorkspaceFolders" => {
            if let Ok(params) =
                serde_json::from_value::<lsp::DidChangeWorkspaceFoldersParams>(notification.params)
            {
                update_workspace_folders(&mut state.workspace_folders, params);
            }
        }
        "workspace/didChangeWatchedFiles" => {
            if let Ok(params) =
                serde_json::from_value::<lsp::DidChangeWatchedFilesParams>(notification.params)
                && params.changes.iter().any(|change| {
                    uri_to_path(&change.uri)
                        .is_some_and(|path| path.extension().is_some_and(|ext| ext == "mux"))
                })
            {
                state.next_revision = state.next_revision.wrapping_add(1);
                for document in state.documents.values_mut() {
                    document.revision = state.next_revision;
                }
                schedule_state_diagnostics(state)?;
            }
        }
        "textDocument/didOpen" => {
            if let Ok(params) =
                serde_json::from_value::<lsp::DidOpenTextDocumentParams>(notification.params)
            {
                let uri = params.text_document.uri.clone();
                if let Some(path) = uri_to_path(&uri) {
                    state.next_revision = state.next_revision.wrapping_add(1);
                    state.documents.insert(
                        uri.as_str().to_owned(),
                        OpenDocument {
                            uri,
                            path,
                            source: params.text_document.text,
                            version: params.text_document.version,
                            revision: state.next_revision,
                        },
                    );
                    schedule_state_diagnostics(state)?;
                }
            }
        }
        "textDocument/didChange" => {
            if let Ok(params) =
                serde_json::from_value::<lsp::DidChangeTextDocumentParams>(notification.params)
                && let Some(change) = params.content_changes.last()
                && let Some(document) = state.documents.get_mut(params.text_document.uri.as_str())
                && params.text_document.version > document.version
            {
                state.next_revision = state.next_revision.wrapping_add(1);
                document.source.clone_from(&change.text);
                document.version = params.text_document.version;
                document.revision = state.next_revision;
                schedule_state_diagnostics(state)?;
            }
        }
        "textDocument/didClose" => {
            if let Ok(params) =
                serde_json::from_value::<lsp::DidCloseTextDocumentParams>(notification.params)
            {
                state.documents.remove(params.text_document.uri.as_str());
                schedule_state_diagnostics(state)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_cancellation(
    connection: &Connection,
    state: &mut ServerState,
    notification: Notification,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(id) = notification
        .params
        .get("id")
        .cloned()
        .and_then(|id| serde_json::from_value::<RequestId>(id).ok())
    else {
        return Ok(());
    };
    if state.outstanding.remove(&id).is_some() {
        lock_or_recover(&state.cancelled, "request cancellation state").insert(id.clone());
        connection
            .sender
            .send(Response::new_err(id, -32800, "Request cancelled".to_owned()).into())?;
    }
    Ok(())
}

fn schedule_state_diagnostics(state: &ServerState) -> Result<(), Box<dyn std::error::Error>> {
    schedule_diagnostics(
        &state.task_sender,
        &state.current_revisions,
        &state.pending_diagnostics,
        &state.documents,
        &state.published_uris,
    )
}

fn send_worker_results(
    connection: &Connection,
    state: &mut ServerState,
) -> Result<(), Box<dyn std::error::Error>> {
    while let Ok(result) = state.result_receiver.try_recv() {
        match result {
            WorkerResult::Request(result) => {
                let Some(expected_revisions) = state.outstanding.remove(&result.id) else {
                    lock_or_recover(&state.cancelled, "request cancellation state")
                        .remove(&result.id);
                    continue;
                };
                let response = if expected_revisions != document_versions(&state.documents) {
                    Response::new_err(
                        result.id,
                        -32801,
                        "Document changed while request was running".to_owned(),
                    )
                    .into()
                } else {
                    result.message
                };
                connection.sender.send(response)?;
            }
            WorkerResult::Diagnostics(result) => {
                if result.revisions == document_versions(&state.documents) {
                    state.published_uris = result.published_uris;
                    for message in result.messages {
                        connection.sender.send(message)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn supported_request(method: &str) -> bool {
    matches!(
        method,
        "textDocument/formatting"
            | "textDocument/documentSymbol"
            | "textDocument/definition"
            | "textDocument/hover"
            | "textDocument/completion"
            | "textDocument/signatureHelp"
            | "textDocument/codeAction"
    )
}

fn document_versions(documents: &HashMap<String, OpenDocument>) -> HashMap<String, u64> {
    documents
        .iter()
        .map(|(uri, document)| (uri.clone(), document.revision))
        .collect()
}

fn schedule_diagnostics(
    tasks: &std::sync::mpsc::Sender<WorkerTask>,
    current_revisions: &Arc<Mutex<HashMap<String, u64>>>,
    pending: &Arc<Mutex<PendingDiagnostics>>,
    documents: &HashMap<String, OpenDocument>,
    published_uris: &std::collections::HashSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let revisions = document_revisions(documents);
    *lock_or_recover(current_revisions, "document revision map") = revisions.clone();
    let task = DiagnosticTask {
        documents: documents.clone(),
        revisions,
        published_uris: published_uris.clone(),
    };
    let should_signal = {
        let mut pending = lock_or_recover(pending, "pending diagnostics");
        pending.latest = Some(task);
        if pending.signal_queued {
            false
        } else {
            pending.signal_queued = true;
            true
        }
    };
    if should_signal {
        tasks.send(WorkerTask::Diagnostics)?;
    }
    Ok(())
}

fn document_revisions(documents: &HashMap<String, OpenDocument>) -> HashMap<String, u64> {
    documents
        .iter()
        .map(|(uri, document)| (uri.clone(), document.revision))
        .collect()
}

fn analysis_worker(
    tasks: std::sync::mpsc::Receiver<WorkerTask>,
    results: std::sync::mpsc::Sender<WorkerResult>,
    cancelled: Arc<Mutex<std::collections::HashSet<RequestId>>>,
    current_revisions: Arc<Mutex<HashMap<String, u64>>>,
    pending_diagnostics: Arc<Mutex<PendingDiagnostics>>,
    stop: Arc<AtomicBool>,
) {
    while let Ok(task) = tasks.recv() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match task {
            WorkerTask::Request(task) => {
                run_request_task(task, &results, &cancelled);
            }
            WorkerTask::Diagnostics => {
                let task = {
                    let mut pending = lock_or_recover(&pending_diagnostics, "pending diagnostics");
                    pending.signal_queued = false;
                    pending.latest.take()
                };
                let Some(task) = task else {
                    continue;
                };
                if task.revisions != *lock_or_recover(&current_revisions, "document revision map") {
                    continue;
                }
                let (worker_connection, client_connection) = Connection::memory();
                let mut published_uris = task.published_uris;
                if let Err(error) = publish_all_diagnostics(
                    &worker_connection,
                    &task.documents,
                    &mut published_uris,
                ) {
                    eprintln!("mux lsp: failed to publish diagnostics: {error}");
                    continue;
                }
                let messages = client_connection.receiver.try_iter().collect();
                let _ = results.send(WorkerResult::Diagnostics(DiagnosticResult {
                    revisions: task.revisions,
                    messages,
                    published_uris,
                }));
            }
        }
    }
}

fn run_request_task(
    task: RequestTask,
    results: &std::sync::mpsc::Sender<WorkerResult>,
    cancelled: &Arc<Mutex<std::collections::HashSet<RequestId>>>,
) {
    let request_id = task.request.id.clone();
    if lock_or_recover(cancelled, "request cancellation state").remove(&request_id) {
        return;
    }

    let (worker_connection, client_connection) = Connection::memory();
    let method = task.request.method.clone();
    let response = match method.as_str() {
        "textDocument/formatting" => handle_formatting(
            &worker_connection,
            task.request,
            &task.documents,
            &task.workspace_folders,
        ),
        "textDocument/documentSymbol" => {
            handle_document_symbols(&worker_connection, task.request, &task.documents)
        }
        "textDocument/definition" => {
            handle_definition(&worker_connection, task.request, &task.documents)
        }
        "textDocument/hover" => handle_hover(&worker_connection, task.request, &task.documents),
        "textDocument/completion" => {
            handle_completion(&worker_connection, task.request, &task.documents)
        }
        "textDocument/signatureHelp" => {
            handle_signature_help(&worker_connection, task.request, &task.documents)
        }
        "textDocument/codeAction" => {
            handle_code_action(&worker_connection, task.request, &task.documents)
        }
        _ => Err(format!("unsupported request: {method}").into()),
    };
    if let Err(error) = response {
        let code = if supported_request(&method) {
            -32603
        } else {
            lsp_server::ErrorCode::MethodNotFound as i32
        };
        let _ = worker_connection
            .sender
            .send(Response::new_err(request_id.clone(), code, error.to_string()).into());
    }
    if let Ok(message) = client_connection.receiver.recv() {
        let _ = results.send(WorkerResult::Request(RequestResult {
            id: request_id,
            message,
        }));
    }
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!("mux lsp: recovering poisoned {label}");
            let guard = poisoned.into_inner();
            mutex.clear_poison();
            guard
        }
    }
}

fn handle_formatting(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
    workspace_folders: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::DocumentFormattingParams>(request.params)?;
    let edits = documents
        .get(params.text_document.uri.as_str())
        .and_then(|document| {
            let (mut options, warnings) = crate::format_config::load_from_directory(
                &formatting_config_directory(document, workspace_folders),
            );
            for warning in warnings {
                eprintln!("warning: {warning}");
            }
            options.indent_type = if params.options.insert_spaces {
                mux_lang::formatter::IndentType::Space
            } else {
                mux_lang::formatter::IndentType::Tab
            };
            options.indent_count = params.options.tab_size.max(1) as usize;
            mux_lang::formatter::format_source_with_options(&document.source, options)
                .ok()
                .map(|formatted| (document, formatted))
        })
        .filter(|(document, formatted)| document.source != *formatted)
        .map(|(document, formatted)| {
            vec![lsp::TextEdit {
                range: lsp::Range::new(
                    lsp::Position::new(0, 0),
                    offset_to_position(&document.source, document.source.len()),
                ),
                new_text: formatted,
            }]
        });
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(edits)?).into())?;
    Ok(())
}

fn formatting_config_directory(document: &OpenDocument, workspace_folders: &[PathBuf]) -> PathBuf {
    if document.uri.as_str().starts_with("untitled:")
        && let [workspace] = workspace_folders
    {
        return workspace.clone();
    }
    document
        .path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf()
}

fn handle_document_symbols(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::DocumentSymbolParams>(request.params)?;
    let symbols = documents
        .get(params.text_document.uri.as_str())
        .map(|document| document_symbols(&document.source))
        .unwrap_or_default();
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(symbols)?).into())?;
    Ok(())
}

fn handle_definition(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::GotoDefinitionParams>(request.params)?;
    let key = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let definition = documents.get(key).and_then(|document| {
        let offset = position_to_offset(
            &document.source,
            params.text_document_position_params.position,
        );
        let overlays = documents
            .values()
            .map(|open| (open.path.clone(), open.source.clone()))
            .collect::<HashMap<_, _>>();
        let analysis = mux_lang::analysis::analyze_source_with_overlays(
            &document.path,
            &document.source,
            &overlays,
        );
        let reference = analysis
            .resolved_identifiers
            .iter()
            .filter(|reference| {
                reference
                    .usage
                    .byte_range
                    .is_some_and(|range| range.start <= offset && offset < range.end)
            })
            .min_by_key(|reference| {
                reference
                    .usage
                    .byte_range
                    .map_or(usize::MAX, |range| range.end - range.start)
            })?;
        let target_file = reference
            .source_path
            .as_deref()
            .and_then(|path| find_file_id(&analysis.files, path))
            .unwrap_or(analysis.root_file);
        let target_source = analysis.files.source(target_file)?;
        let declaration = reference.declaration?.byte_range?;
        let declaration_text = target_source.get(declaration.start..declaration.end)?;
        let name_offset = declaration_text.find(&reference.name)?;
        let start = declaration.start + name_offset;
        let range = lsp::Range::new(
            offset_to_position(target_source, start),
            offset_to_position(target_source, start + reference.name.len()),
        );
        let target_path = analysis.files.path(target_file)?;
        let target_uri = uri_for_path(documents, target_path)?;
        Some(lsp::Location::new(target_uri, range))
    });
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(definition)?).into())?;
    Ok(())
}

fn find_file_id(
    files: &mux_lang::diagnostic::Files,
    path: &std::path::Path,
) -> Option<mux_lang::diagnostic::FileId> {
    let wanted = mux_lang::analysis::absolute_path(path);
    files.iter().find_map(|(file_id, file_path, _)| {
        let actual = mux_lang::analysis::absolute_path(file_path);
        let same_path = actual == wanted
            || matches!((actual.canonicalize(), wanted.canonicalize()), (Ok(left), Ok(right)) if left == right);
        same_path.then_some(file_id)
    })
}

fn handle_hover(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::HoverParams>(request.params)?;
    let key = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let hover = documents.get(key).and_then(|document| {
        let offset = position_to_offset(
            &document.source,
            params.text_document_position_params.position,
        );
        let overlays = documents
            .values()
            .map(|open| (open.path.clone(), open.source.clone()))
            .collect::<HashMap<_, _>>();
        let analysis = mux_lang::analysis::analyze_source_with_overlays(
            &document.path,
            &document.source,
            &overlays,
        );
        let reference = analysis.resolved_identifiers.iter().find(|reference| {
            reference
                .usage
                .byte_range
                .is_some_and(|range| range.start <= offset && offset < range.end)
        })?;
        let type_ = reference.type_.as_ref()?;
        let range = reference.usage.byte_range?;
        Some(lsp::Hover {
            contents: lsp::HoverContents::Markup(lsp::MarkupContent {
                kind: lsp::MarkupKind::Markdown,
                value: format!("```mux\n{}: {}\n```", reference.name, display_type(type_)),
            }),
            range: lsp_range(&document.source, range),
        })
    });
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(hover)?).into())?;
    Ok(())
}

fn display_type(type_: &mux_lang::semantics::types::Type) -> String {
    use mux_lang::semantics::types::Type;

    match type_ {
        Type::Primitive(primitive) => format!("{primitive:?}").to_ascii_lowercase(),
        Type::List(inner) => format!("list<{}>", display_type(inner)),
        Type::Map(key, value) => format!("map<{}, {}>", display_type(key), display_type(value)),
        Type::Set(inner) => format!("set<{}>", display_type(inner)),
        Type::Tuple(first, second) => {
            format!("tuple<{}, {}>", display_type(first), display_type(second))
        }
        Type::Optional(inner) => format!("optional<{}>", display_type(inner)),
        Type::Result(ok, error) => {
            format!("result<{}, {}>", display_type(ok), display_type(error))
        }
        Type::Reference(inner) => format!("&{}", display_type(inner)),
        Type::TraitObject(inner) => format!("dyn {}", display_type(inner)),
        Type::Void => "void".to_owned(),
        Type::Never => "never".to_owned(),
        Type::EmptyList => "list<_>".to_owned(),
        Type::EmptyMap => "map<_, _>".to_owned(),
        Type::EmptySet => "set<_>".to_owned(),
        Type::Function {
            params,
            returns,
            default_count: _,
        } => format!(
            "func({}) -> {}",
            params
                .iter()
                .map(display_type)
                .collect::<Vec<_>>()
                .join(", "),
            display_type(returns)
        ),
        Type::Named(name, arguments) | Type::Instantiated(name, arguments) => {
            if arguments.is_empty() {
                name.clone()
            } else {
                format!(
                    "{}<{}>",
                    name,
                    arguments
                        .iter()
                        .map(display_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        Type::Variable(name) | Type::Generic(name) => name.clone(),
        Type::Module(name) => format!("module {name}"),
    }
}

fn handle_completion(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::CompletionParams>(request.params)?;
    let key = params.text_document_position.text_document.uri.as_str();
    let items = documents
        .get(key)
        .map(|document| {
            let overlays = documents
                .values()
                .map(|open| (open.path.clone(), open.source.clone()))
                .collect::<HashMap<_, _>>();
            complete(
                &document.source,
                params.text_document_position.position,
                &document.path,
                &overlays,
            )
        })
        .unwrap_or_default();
    connection.sender.send(
        Response::new_ok(
            request.id,
            serde_json::to_value(lsp::CompletionResponse::Array(items))?,
        )
        .into(),
    )?;
    Ok(())
}

fn complete(
    source: &str,
    position: lsp::Position,
    path: &std::path::Path,
    overlays: &HashMap<PathBuf, String>,
) -> Vec<lsp::CompletionItem> {
    let symbols = document_symbols(source);
    let offset = position_to_offset(source, position);
    let prefix = source[..offset]
        .rsplit(|character: char| !(character.is_alphanumeric() || character == '_'))
        .next()
        .unwrap_or_default();
    let mut candidates =
        std::collections::BTreeMap::<String, Option<lsp::CompletionItemKind>>::new();
    collect_completions(&symbols, position, false, &mut candidates);
    collect_visible_syntax_completions(source, offset, &mut candidates);
    let imports = collect_imports(source);
    let parsed = mux_lang::syntax::parse_source(source);
    let member_base = find_member_access_base(parsed.tree.root(), offset);
    if !imports.is_empty() || member_base.is_some() {
        let analysis = mux_lang::analysis::analyze_source_with_overlays(path, source, overlays);
        collect_import_completions(source, path, &analysis, &imports, &mut candidates);
        collect_member_completions(source, path, &analysis, member_base, &mut candidates);
    }
    for keyword in [
        "auto",
        "break",
        "class",
        "common",
        "const",
        "continue",
        "else",
        "enum",
        "false",
        "for",
        "func",
        "if",
        "import",
        "in",
        "interface",
        "is",
        "match",
        "none",
        "return",
        "returns",
        "true",
        "while",
    ] {
        candidates
            .entry(keyword.to_owned())
            .or_insert(Some(lsp::CompletionItemKind::KEYWORD));
    }
    candidates
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, kind)| lsp::CompletionItem {
            label: name,
            kind,
            detail: Some("Mux symbol".to_owned()),
            ..lsp::CompletionItem::default()
        })
        .collect()
}

fn collect_imports(source: &str) -> Vec<(String, mux_lang::syntax::SyntaxImportSpec)> {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};

    let parsed = mux_lang::syntax::parse_source(source);
    let mut imports = Vec::new();
    let mut pending = vec![parsed.tree.root()];
    while let Some(node) = pending.pop() {
        if let Some(Data::Import {
            module_path, spec, ..
        }) = node.lowering_data()
            && let Some(module) = source.get(module_path.start..module_path.end)
        {
            imports.push((module.to_owned(), spec.clone()));
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                pending.push(child);
            }
        }
    }
    imports
}

fn collect_import_completions(
    source: &str,
    path: &std::path::Path,
    analysis: &mux_lang::analysis::Analysis,
    imports: &[(String, mux_lang::syntax::SyntaxImportSpec)],
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    for (module, spec) in imports {
        let module_source = module_source_for_import(analysis, path, module);
        let module_symbols = module_source.map(document_symbols).unwrap_or_default();
        let imported_symbols = analysis.imported_modules.get(module);
        match spec {
            mux_lang::syntax::SyntaxImportSpec::Module { alias } => {
                let label = match alias {
                    mux_lang::syntax::SyntaxModuleAlias::Default => {
                        module.rsplit('.').next().unwrap_or(module)
                    }
                    mux_lang::syntax::SyntaxModuleAlias::Hidden => continue,
                    mux_lang::syntax::SyntaxModuleAlias::Explicit(range) => {
                        let Some(alias) = source.get(range.start..range.end) else {
                            continue;
                        };
                        alias
                    }
                };
                candidates.insert(label.to_owned(), Some(lsp::CompletionItemKind::MODULE));
            }
            mux_lang::syntax::SyntaxImportSpec::Item { item, alias } => {
                insert_imported_item(
                    source,
                    *item,
                    *alias,
                    imported_symbols,
                    &module_symbols,
                    candidates,
                );
            }
            mux_lang::syntax::SyntaxImportSpec::Items { items } => {
                for (item, alias) in items {
                    insert_imported_item(
                        source,
                        *item,
                        *alias,
                        imported_symbols,
                        &module_symbols,
                        candidates,
                    );
                }
            }
            mux_lang::syntax::SyntaxImportSpec::Wildcard => {
                if let Some(imported_symbols) = imported_symbols {
                    collect_imported_symbols(imported_symbols, candidates);
                } else if module_source.is_some() {
                    for symbol in &module_symbols {
                        candidates.insert(symbol.name.clone(), Some(completion_kind(symbol.kind)));
                    }
                } else if let Some(directory) = module_directory_for_import(path, module) {
                    for (_, imported_path, _) in analysis.files.iter() {
                        let imported_path = mux_lang::analysis::absolute_path(imported_path);
                        let same_directory = imported_path.parent().is_some_and(|parent| {
                            parent == directory
                                || matches!((parent.canonicalize(), directory.canonicalize()), (Ok(left), Ok(right)) if left == right)
                        });
                        if same_directory
                            && let Some(name) =
                                imported_path.file_stem().and_then(std::ffi::OsStr::to_str)
                        {
                            candidates
                                .insert(name.to_owned(), Some(lsp::CompletionItemKind::MODULE));
                        }
                    }
                }
            }
        }
    }
}

fn insert_imported_item(
    source: &str,
    item: mux_lang::lexer::ByteRange,
    alias: Option<mux_lang::lexer::ByteRange>,
    imported_symbols: Option<&std::collections::HashMap<String, mux_lang::semantics::SymbolKind>>,
    module_symbols: &[lsp::DocumentSymbol],
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    let Some(original_name) = source.get(item.start..item.end) else {
        return;
    };
    let label = alias
        .and_then(|range| source.get(range.start..range.end))
        .unwrap_or(original_name);
    let kind = imported_symbols
        .and_then(|symbols| symbols.get(original_name))
        .map(semantic_completion_kind)
        .or_else(|| find_symbol_kind(module_symbols, original_name).map(completion_kind))
        .unwrap_or(lsp::CompletionItemKind::VARIABLE);
    candidates.insert(label.to_owned(), Some(kind));
}

fn collect_imported_symbols(
    symbols: &std::collections::HashMap<String, mux_lang::semantics::SymbolKind>,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    for (name, kind) in symbols {
        candidates.insert(name.clone(), Some(semantic_completion_kind(kind)));
    }
}

fn semantic_completion_kind(kind: &mux_lang::semantics::SymbolKind) -> lsp::CompletionItemKind {
    use mux_lang::semantics::SymbolKind;

    match kind {
        SymbolKind::Function => lsp::CompletionItemKind::FUNCTION,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum => {
            lsp::CompletionItemKind::CLASS
        }
        SymbolKind::Constant => lsp::CompletionItemKind::CONSTANT,
        SymbolKind::Variable | SymbolKind::Import | SymbolKind::Type => {
            lsp::CompletionItemKind::VARIABLE
        }
    }
}

fn find_symbol_kind(symbols: &[lsp::DocumentSymbol], name: &str) -> Option<lsp::SymbolKind> {
    for symbol in symbols {
        if symbol.name == name {
            return Some(symbol.kind);
        }
        if let Some(children) = &symbol.children
            && let Some(kind) = find_symbol_kind(children, name)
        {
            return Some(kind);
        }
    }
    None
}

fn module_source_for_import<'a>(
    analysis: &'a mux_lang::analysis::Analysis,
    root_path: &std::path::Path,
    module: &str,
) -> Option<&'a str> {
    let mut imported_path = module_directory_for_import(root_path, module)?;
    imported_path.set_extension("mux");
    let imported_path = mux_lang::analysis::absolute_path(&imported_path);
    analysis.files.iter().find_map(|(_, path, source)| {
        let candidate = mux_lang::analysis::absolute_path(path);
        let same_path = candidate == imported_path
            || matches!((candidate.canonicalize(), imported_path.canonicalize()), (Ok(left), Ok(right)) if left == right);
        same_path.then_some(source)
    })
}

fn module_directory_for_import(root_path: &std::path::Path, module: &str) -> Option<PathBuf> {
    let mut directory = if module.starts_with("./") || module.starts_with("../") {
        let relative = module.trim_start_matches("./");
        let mut path = root_path.parent()?.to_path_buf();
        for part in relative.split('/') {
            if part == ".." {
                path.pop();
            } else if !part.is_empty() {
                path.push(part);
            }
        }
        path
    } else if module.starts_with('/') {
        PathBuf::from(module)
    } else {
        let mut path = root_path.parent()?.to_path_buf();
        for part in module.split('.') {
            path.push(part);
        }
        path
    };
    directory = mux_lang::analysis::absolute_path(&directory);
    Some(directory)
}

fn collect_member_completions(
    source: &str,
    path: &std::path::Path,
    analysis: &mux_lang::analysis::Analysis,
    base_range: Option<mux_lang::lexer::ByteRange>,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    let Some(base_range) = base_range else {
        return;
    };
    let Some(reference) = analysis
        .resolved_identifiers
        .iter()
        .find(|reference| reference.usage.byte_range == Some(base_range))
    else {
        return;
    };
    let Some(base_type) = reference.type_.as_ref() else {
        return;
    };
    for method in &reference.bound_methods {
        candidates.insert(method.clone(), Some(lsp::CompletionItemKind::METHOD));
    }
    for method in mux_lang::semantics::SemanticAnalyzer::new().builtin_method_names(base_type) {
        candidates.insert(method.to_owned(), Some(lsp::CompletionItemKind::METHOD));
    }
    if let mux_lang::semantics::types::Type::Module(namespace) = base_type {
        collect_module_namespace_members(source, path, analysis, namespace, candidates);
        return;
    }
    let type_name = match base_type {
        mux_lang::semantics::types::Type::Named(name, _)
        | mux_lang::semantics::types::Type::Instantiated(name, _) => name,
        _ => return,
    };
    for (_, _, module_source) in analysis.files.iter() {
        let symbols = document_symbols(module_source);
        collect_named_type_members(&symbols, type_name, candidates);
    }
}

fn collect_module_namespace_members(
    source: &str,
    root_path: &std::path::Path,
    analysis: &mux_lang::analysis::Analysis,
    namespace: &str,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    let module = module_path_for_namespace(source, namespace).unwrap_or(namespace);
    if let Some(symbols) = analysis.imported_modules.get(module) {
        collect_imported_symbols(symbols, candidates);
        return;
    }
    if let Some(module_source) = module_source_for_import(analysis, root_path, module) {
        for symbol in document_symbols(module_source) {
            candidates.insert(symbol.name, Some(completion_kind(symbol.kind)));
        }
        return;
    }
    let Some(directory) = module_directory_for_import(root_path, module) else {
        return;
    };
    for (_, path, _) in analysis.files.iter() {
        let path = mux_lang::analysis::absolute_path(path);
        let same_directory = path.parent().is_some_and(|parent| {
            parent == directory
                || matches!((parent.canonicalize(), directory.canonicalize()), (Ok(left), Ok(right)) if left == right)
        });
        if same_directory && let Some(name) = path.file_stem().and_then(std::ffi::OsStr::to_str) {
            candidates.insert(name.to_owned(), Some(lsp::CompletionItemKind::MODULE));
        }
    }
}

fn module_path_for_namespace<'a>(source: &'a str, namespace: &str) -> Option<&'a str> {
    use mux_lang::syntax::{
        AstLoweringData as Data, SyntaxElement, SyntaxImportSpec, SyntaxModuleAlias,
    };

    let parsed = mux_lang::syntax::parse_source(source);
    let mut pending = vec![parsed.tree.root()];
    while let Some(node) = pending.pop() {
        if let Some(Data::Import {
            module_path, spec, ..
        }) = node.lowering_data()
            && let Some(module) = source.get(module_path.start..module_path.end)
            && let SyntaxImportSpec::Module { alias } = spec
        {
            let bound_name = match alias {
                SyntaxModuleAlias::Default => module.rsplit('.').next().unwrap_or(module),
                SyntaxModuleAlias::Hidden => continue,
                SyntaxModuleAlias::Explicit(range) => {
                    let Some(alias) = source.get(range.start..range.end) else {
                        continue;
                    };
                    alias
                }
            };
            if bound_name == namespace {
                return Some(module);
            }
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                pending.push(child);
            }
        }
    }
    None
}

fn find_member_access_base(
    node: &mux_lang::syntax::SyntaxNode,
    offset: usize,
) -> Option<mux_lang::lexer::ByteRange> {
    use mux_lang::syntax::{AstLoweringData, SyntaxElement};

    if let Some(AstLoweringData::FieldAccess { base, field }) = node.lowering_data()
        && field.start <= offset
        && offset <= field.end
    {
        return Some(*base);
    }
    for child in node.children() {
        if let SyntaxElement::Node(child) = child
            && let Some(base) = find_member_access_base(child, offset)
        {
            return Some(base);
        }
    }
    None
}

fn collect_named_type_members(
    symbols: &[lsp::DocumentSymbol],
    type_name: &str,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    for symbol in symbols {
        if symbol.name == type_name
            && matches!(
                symbol.kind,
                lsp::SymbolKind::CLASS | lsp::SymbolKind::INTERFACE | lsp::SymbolKind::ENUM
            )
        {
            if let Some(members) = &symbol.children {
                for member in members {
                    candidates.insert(member.name.clone(), Some(completion_kind(member.kind)));
                }
            }
            return;
        }
        if let Some(children) = &symbol.children {
            collect_named_type_members(children, type_name, candidates);
        }
    }
}

fn collect_completions(
    symbols: &[lsp::DocumentSymbol],
    position: lsp::Position,
    inside_function: bool,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    for symbol in symbols {
        // Local declarations are collected from syntax below so that source
        // order and active block scope are respected.
        let is_local = matches!(
            symbol.kind,
            lsp::SymbolKind::VARIABLE | lsp::SymbolKind::CONSTANT
        ) && inside_function;
        if !is_local {
            candidates.insert(symbol.name.clone(), Some(completion_kind(symbol.kind)));
        }
        if range_contains(symbol.range, position)
            && let Some(children) = &symbol.children
        {
            let child_is_inside_function = inside_function
                || matches!(
                    symbol.kind,
                    lsp::SymbolKind::FUNCTION | lsp::SymbolKind::METHOD
                );
            collect_completions(children, position, child_is_inside_function, candidates);
        }
    }
}

fn collect_visible_syntax_completions(
    source: &str,
    offset: usize,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    let parsed = mux_lang::syntax::parse_source(source);
    collect_visible_syntax_node(source, parsed.tree.root(), offset, true, candidates);
}

fn collect_visible_syntax_node(
    source: &str,
    node: &mux_lang::syntax::SyntaxNode,
    offset: usize,
    enclosing_blocks_contain_cursor: bool,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};

    let parameters_and_body = match node.lowering_data() {
        Some(Data::Function {
            parameters, body, ..
        }) => body.map(|body| (parameters, body)),
        Some(Data::Lambda {
            parameters, body, ..
        }) => Some((parameters, *body)),
        _ => None,
    };
    if let Some((parameters, body)) = parameters_and_body
        && body.start <= offset
        && offset <= body.end
    {
        for parameter in parameters {
            if let Some(parameter_node) = find_syntax_node(node, *parameter)
                && let Some(Data::Parameter { name, .. }) = parameter_node.lowering_data()
            {
                insert_syntax_name(source, *name, lsp::CompletionItemKind::VARIABLE, candidates);
            }
        }
    }

    let enclosing_blocks_contain_cursor = enclosing_blocks_contain_cursor
        && (!matches!(node.lowering_data(), Some(Data::Block))
            || (node.range().start <= offset && offset <= node.range().end));
    if let Some(Data::VariableDeclaration {
        kind,
        name,
        ast_span,
        ..
    }) = node.lowering_data()
        && enclosing_blocks_contain_cursor
        && ast_span.end <= offset
    {
        let item_kind = if matches!(kind, mux_lang::syntax::VariableDeclarationKind::Const) {
            lsp::CompletionItemKind::CONSTANT
        } else {
            lsp::CompletionItemKind::VARIABLE
        };
        insert_syntax_name(source, *name, item_kind, candidates);
    }
    for child in node.children() {
        if let SyntaxElement::Node(child) = child {
            collect_visible_syntax_node(
                source,
                child,
                offset,
                enclosing_blocks_contain_cursor,
                candidates,
            );
        }
    }
}

fn find_syntax_node(
    node: &mux_lang::syntax::SyntaxNode,
    range: mux_lang::lexer::ByteRange,
) -> Option<&mux_lang::syntax::SyntaxNode> {
    use mux_lang::syntax::SyntaxElement;

    if node.range() == range {
        return Some(node);
    }
    node.children().iter().find_map(|child| match child {
        SyntaxElement::Node(child) => find_syntax_node(child, range),
        SyntaxElement::Token(_) => None,
    })
}

fn insert_syntax_name(
    source: &str,
    range: mux_lang::lexer::ByteRange,
    kind: lsp::CompletionItemKind,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    if let Some(name) = source.get(range.start..range.end) {
        candidates.insert(name.to_owned(), Some(kind));
    }
}

fn completion_kind(kind: lsp::SymbolKind) -> lsp::CompletionItemKind {
    match kind {
        lsp::SymbolKind::FUNCTION | lsp::SymbolKind::METHOD => lsp::CompletionItemKind::FUNCTION,
        lsp::SymbolKind::CLASS | lsp::SymbolKind::INTERFACE | lsp::SymbolKind::ENUM => {
            lsp::CompletionItemKind::CLASS
        }
        lsp::SymbolKind::CONSTANT => lsp::CompletionItemKind::CONSTANT,
        lsp::SymbolKind::FIELD | lsp::SymbolKind::ENUM_MEMBER => lsp::CompletionItemKind::FIELD,
        _ => lsp::CompletionItemKind::VARIABLE,
    }
}

fn range_contains(range: lsp::Range, position: lsp::Position) -> bool {
    range.start <= position && position <= range.end
}

fn handle_signature_help(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::SignatureHelpParams>(request.params)?;
    let key = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let signature = documents.get(key).and_then(|document| {
        let offset = position_to_offset(
            &document.source,
            params.text_document_position_params.position,
        );
        let parsed = mux_lang::syntax::parse_source(&document.source);
        let (callee, arguments, _) = find_call_context(parsed.tree.root(), offset)?;
        let callee_name = document.source.get(callee.start..callee.end)?;
        let overlays = documents
            .values()
            .map(|open| (open.path.clone(), open.source.clone()))
            .collect::<HashMap<_, _>>();
        let analysis = mux_lang::analysis::analyze_source_with_overlays(
            &document.path,
            &document.source,
            &overlays,
        );
        let reference = analysis.resolved_identifiers.iter().find(|reference| {
            reference.name == callee_name
                && reference
                    .usage
                    .byte_range
                    .is_some_and(|usage| callee.start <= usage.start && usage.end <= callee.end)
        })?;
        let mux_lang::semantics::types::Type::Function {
            params, returns, ..
        } = reference.type_.as_ref()?
        else {
            return None;
        };
        let parameter_labels = params
            .iter()
            .enumerate()
            .map(|(index, type_)| format!("param{}: {}", index + 1, display_type(type_)))
            .collect::<Vec<_>>();
        let label = format!(
            "{}({}) -> {}",
            reference.name,
            parameter_labels.join(", "),
            display_type(returns)
        );
        let active_parameter = (!params.is_empty()).then(|| {
            arguments
                .iter()
                .filter(|argument| argument.end <= offset)
                .count()
                .min(params.len().saturating_sub(1)) as u32
        });
        Some(lsp::SignatureHelp {
            signatures: vec![lsp::SignatureInformation {
                label,
                documentation: None,
                parameters: Some(
                    parameter_labels
                        .into_iter()
                        .map(|label| lsp::ParameterInformation {
                            label: lsp::ParameterLabel::Simple(label),
                            documentation: None,
                        })
                        .collect(),
                ),
                active_parameter,
            }],
            active_signature: Some(0),
            active_parameter,
        })
    });
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(signature)?).into())?;
    Ok(())
}

fn find_call_context(
    node: &mux_lang::syntax::SyntaxNode,
    offset: usize,
) -> Option<(
    mux_lang::lexer::ByteRange,
    Vec<mux_lang::lexer::ByteRange>,
    mux_lang::lexer::ByteRange,
)> {
    use mux_lang::syntax::{AstLoweringData, SyntaxElement};

    let containing_call = match node.lowering_data() {
        Some(AstLoweringData::Call { callee, arguments })
            if node.range().start <= offset && offset <= node.range().end =>
        {
            Some((*callee, arguments.clone(), node.range()))
        }
        _ => None,
    };
    for child in node.children() {
        if let SyntaxElement::Node(child) = child
            && let Some(nested) = find_call_context(child, offset)
        {
            return Some(nested);
        }
    }
    containing_call
}

fn document_symbols(source: &str) -> Vec<lsp::DocumentSymbol> {
    let parsed = mux_lang::syntax::parse_source(source);
    let mut symbols = Vec::new();
    collect_syntax_symbols(source, parsed.tree.root(), &mut symbols);
    symbols
}

fn collect_syntax_symbols(
    source: &str,
    node: &mux_lang::syntax::SyntaxNode,
    symbols: &mut Vec<lsp::DocumentSymbol>,
) {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement, VariableDeclarationKind};

    let symbol_data = match node.lowering_data() {
        Some(Data::Function { name, ast_span, .. }) => {
            Some((*name, *ast_span, lsp::SymbolKind::FUNCTION))
        }
        Some(Data::Class { name, ast_span, .. }) => {
            Some((*name, *ast_span, lsp::SymbolKind::CLASS))
        }
        Some(Data::Interface { name, ast_span, .. }) => {
            Some((*name, *ast_span, lsp::SymbolKind::INTERFACE))
        }
        Some(Data::Enum { name, ast_span, .. }) => Some((*name, *ast_span, lsp::SymbolKind::ENUM)),
        Some(Data::Test { name, ast_span, .. }) => {
            Some((*name, *ast_span, lsp::SymbolKind::FUNCTION))
        }
        Some(Data::VariableDeclaration {
            kind,
            name,
            ast_span,
            ..
        }) => Some((
            *name,
            *ast_span,
            if *kind == VariableDeclarationKind::Const {
                lsp::SymbolKind::CONSTANT
            } else {
                lsp::SymbolKind::VARIABLE
            },
        )),
        Some(Data::Field { name, .. }) => Some((*name, node.range(), lsp::SymbolKind::FIELD)),
        Some(Data::EnumVariant { name, .. }) => {
            Some((*name, node.range(), lsp::SymbolKind::ENUM_MEMBER))
        }
        _ => None,
    };
    if let Some((name_range, declaration_range, kind)) = symbol_data
        && let (Some(name), Some(selection)) = (
            source.get(name_range.start..name_range.end),
            lsp_range(source, name_range),
        )
    {
        let children = collect_child_symbols(source, node);
        symbols.push(lsp::DocumentSymbol {
            name: name.to_owned(),
            detail: None,
            kind,
            tags: None,
            #[allow(deprecated)] // lsp-types retains this field for backwards compatibility.
            deprecated: None,
            range: lsp_range(source, declaration_range).unwrap_or(selection),
            selection_range: selection,
            children: (!children.is_empty()).then_some(children),
        });
    } else {
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                collect_syntax_symbols(source, child, symbols);
            }
        }
    }
}

fn collect_child_symbols(
    source: &str,
    node: &mux_lang::syntax::SyntaxNode,
) -> Vec<lsp::DocumentSymbol> {
    use mux_lang::syntax::SyntaxElement;

    let mut children = Vec::new();
    for child in node.children() {
        if let SyntaxElement::Node(child) = child {
            collect_syntax_symbols(source, child, &mut children);
        }
    }
    children
}

fn lsp_range(source: &str, range: mux_lang::lexer::ByteRange) -> Option<lsp::Range> {
    if range.start > range.end || range.end > source.len() {
        return None;
    }
    Some(lsp::Range::new(
        offset_to_position(source, range.start),
        offset_to_position(source, range.end),
    ))
}

fn handle_code_action(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::CodeActionParams>(request.params)?;
    let Some(document) = documents.get(params.text_document.uri.as_str()) else {
        connection
            .sender
            .send(Response::new_ok(request.id, serde_json::json!([])).into())?;
        return Ok(());
    };
    let overlays = documents
        .values()
        .map(|document| (document.path.clone(), document.source.clone()))
        .collect::<HashMap<_, _>>();
    let analysis = mux_lang::analysis::analyze_source_with_overlays(
        &document.path,
        &document.source,
        &overlays,
    );
    let mut sources = HashMap::new();
    for (file_id, path, source) in analysis.files.iter() {
        let uri = uri_for_path(documents, path);
        sources.insert(file_id, (uri, source.to_owned()));
    }
    let requested_codes = params
        .context
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code.as_ref())
        .map(|code| match code {
            lsp::NumberOrString::String(code) => code.clone(),
            lsp::NumberOrString::Number(code) => code.to_string(),
        })
        .collect::<std::collections::HashSet<_>>();
    let actions = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| requested_codes.contains(&diagnostic.code.to_string()))
        .flat_map(|diagnostic| {
            code_actions_for_diagnostic(diagnostic, &analysis, &document.path, &sources)
        })
        .collect::<Vec<_>>();
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(actions)?).into())?;
    Ok(())
}

#[allow(clippy::mutable_key_type)] // LSP requires a URI-keyed workspace edit; URI hashing uses its stable serialized value.
fn code_actions_for_diagnostic(
    diagnostic: &mux_lang::diagnostic::Diagnostic,
    analysis: &mux_lang::analysis::Analysis,
    root_path: &std::path::Path,
    sources: &HashMap<mux_lang::diagnostic::FileId, (Option<lsp::Uri>, String)>,
) -> Vec<lsp::CodeAction> {
    use mux_lang::diagnostic::{Applicability, EditReplacement, fix};
    let mut edits = Vec::<mux_lang::diagnostic::TextEdit>::new();
    edits.extend(
        diagnostic
            .edits
            .iter()
            .filter(|edit| edit.is_machine_applicable())
            .cloned(),
    );
    for span_edit in &diagnostic.span_edits {
        if span_edit.applicability != Applicability::MachineApplicable {
            continue;
        }
        let Some(file_id) = span_edit.target_file.or(diagnostic.file_id) else {
            continue;
        };
        let Some(source) = analysis.files.source(file_id) else {
            continue;
        };
        let Ok(range) = fix::source_range_for_span(source, span_edit.target) else {
            continue;
        };
        let replacement = match &span_edit.replacement {
            EditReplacement::Text(text) => text.clone(),
            EditReplacement::Source(span) => {
                let source_file = span_edit.replacement_file.unwrap_or(file_id);
                let Some(source_text) = analysis.files.source(source_file) else {
                    continue;
                };
                let Ok(source_range) = fix::source_range_for_span(source_text, *span) else {
                    continue;
                };
                let Some(text) = source_text.get(source_range.start_byte..source_range.end_byte)
                else {
                    continue;
                };
                text.to_owned()
            }
        };
        edits.push(
            mux_lang::diagnostic::TextEdit::machine_applicable(
                file_id,
                range,
                replacement,
                span_edit.diagnostic_code,
            )
            .with_solution(span_edit.solution_id),
        );
    }

    let mut alternatives = std::collections::BTreeMap::<Option<usize>, Vec<_>>::new();
    for edit in edits {
        alternatives.entry(edit.solution_id).or_default().push(edit);
    }
    let common = alternatives.remove(&None).unwrap_or_default();
    let solution_ids = alternatives
        .keys()
        .filter_map(|solution_id| *solution_id)
        .collect::<Vec<_>>();
    if solution_ids.is_empty() && common.is_empty() {
        return Vec::new();
    }
    let groups = if solution_ids.is_empty() {
        vec![common]
    } else {
        solution_ids
            .into_iter()
            .map(|id| {
                let mut group = common.clone();
                group.extend(alternatives.remove(&Some(id)).unwrap_or_default());
                group
            })
            .collect()
    };
    groups
        .into_iter()
        .filter_map(|group| {
            mux_lang::analysis::apply_fixes_and_validate(analysis, root_path, &group).ok()?;
            let mut changes = HashMap::<lsp::Uri, Vec<lsp::TextEdit>>::new();
            for edit in group {
                let (Some(uri), Some((_, source))) = (
                    sources.get(&edit.file_id)?.0.clone(),
                    sources.get(&edit.file_id),
                ) else {
                    return None;
                };
                changes.entry(uri).or_default().push(lsp::TextEdit {
                    range: lsp::Range::new(
                        offset_to_position(source, edit.range.start_byte),
                        offset_to_position(source, edit.range.end_byte),
                    ),
                    new_text: edit.replacement,
                });
            }
            let title = format!("Apply {} fix", diagnostic.code);
            Some(lsp::CodeAction {
                title,
                kind: Some(lsp::CodeActionKind::QUICKFIX),
                diagnostics: None,
                edit: Some(lsp::WorkspaceEdit {
                    changes: Some(changes),
                    ..lsp::WorkspaceEdit::default()
                }),
                command: None,
                is_preferred: Some(true),
                disabled: None,
                data: None,
            })
        })
        .collect()
}

fn publish_all_diagnostics(
    connection: &Connection,
    documents: &HashMap<String, OpenDocument>,
    published_uris: &mut std::collections::HashSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let overlays = documents
        .values()
        .map(|document| (document.path.clone(), document.source.clone()))
        .collect::<HashMap<_, _>>();
    let mut by_uri = HashMap::<String, (lsp::Uri, Vec<lsp::Diagnostic>)>::new();
    let mut seen = std::collections::HashSet::new();
    for document in documents.values() {
        by_uri
            .entry(document.uri.as_str().to_owned())
            .or_insert_with(|| (document.uri.clone(), Vec::new()));
        let analysis = mux_lang::analysis::analyze_source_with_overlays(
            &document.path,
            &document.source,
            &overlays,
        );
        let sources = analysis
            .files
            .iter()
            .map(|(id, path, source)| {
                let uri = if id == analysis.root_file {
                    Some(document.uri.clone())
                } else {
                    uri_for_path(documents, path)
                };
                (id, (uri, source.to_owned()))
            })
            .collect::<HashMap<_, _>>();
        for diagnostic in &analysis.diagnostics {
            let Some((uri, source)) = diagnostic
                .file_id
                .and_then(|id| sources.get(&id))
                .and_then(|(uri, source)| uri.as_ref().map(|uri| (uri, source.as_str())))
            else {
                continue;
            };
            let range = diagnostic
                .labels
                .first()
                .and_then(|label| label.span.byte_range)
                .map_or_else(
                    || lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(0, 0)),
                    |range| {
                        lsp::Range::new(
                            offset_to_position(source, range.start),
                            offset_to_position(source, range.end),
                        )
                    },
                );
            let severity = match diagnostic.level {
                mux_lang::diagnostic::Level::Error => lsp::DiagnosticSeverity::ERROR,
                mux_lang::diagnostic::Level::Warning => lsp::DiagnosticSeverity::WARNING,
            };
            let converted = lsp::Diagnostic::new(
                range,
                Some(severity),
                Some(lsp::NumberOrString::String(diagnostic.code.to_string())),
                Some("mux".to_owned()),
                diagnostic.message.clone(),
                None,
                None,
            );
            let key = format!(
                "{}:{}:{}:{}:{}:{}:{}:{}",
                uri.as_str(),
                converted.range.start.line,
                converted.range.start.character,
                converted.range.end.line,
                converted.range.end.character,
                converted.severity.map_or(0, |severity| {
                    if severity == lsp::DiagnosticSeverity::ERROR {
                        1
                    } else {
                        2
                    }
                }),
                converted
                    .code
                    .as_ref()
                    .map_or_else(String::new, |code| format!("{code:?}")),
                converted.message
            );
            if seen.insert(key) {
                by_uri
                    .entry(uri.as_str().to_owned())
                    .or_insert_with(|| (uri.clone(), Vec::new()))
                    .1
                    .push(converted);
            }
        }
    }
    let current_uris = by_uri
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    for uri in published_uris
        .difference(&current_uris)
        .cloned()
        .collect::<Vec<_>>()
    {
        if let Ok(uri) = lsp::Uri::from_str(&uri) {
            send_empty_diagnostics(connection, uri)?;
        }
    }
    for (uri_key, (uri, diagnostics)) in by_uri {
        let version = documents.get(&uri_key).map(|document| document.version);
        send_diagnostics(connection, uri, diagnostics, version)?;
    }
    *published_uris = current_uris;
    Ok(())
}

fn send_diagnostics(
    connection: &Connection,
    uri: lsp::Uri,
    diagnostics: Vec<lsp::Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = lsp::PublishDiagnosticsParams::new(uri, diagnostics, version);
    connection.sender.send(
        Notification::new(
            "textDocument/publishDiagnostics".to_owned(),
            serde_json::to_value(params)?,
        )
        .into(),
    )?;
    Ok(())
}

fn send_empty_diagnostics(
    connection: &Connection,
    uri: lsp::Uri,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = lsp::PublishDiagnosticsParams::new(uri, Vec::new(), None);
    connection.sender.send(
        Notification::new(
            "textDocument/publishDiagnostics".to_owned(),
            serde_json::to_value(params)?,
        )
        .into(),
    )?;
    Ok(())
}

fn uri_to_path(uri: &lsp::Uri) -> Option<PathBuf> {
    let url = Url::parse(uri.as_str()).ok()?;
    if url.scheme() == "file" {
        return url.to_file_path().ok();
    }
    if url.scheme() != "untitled" {
        return None;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    uri.as_str().hash(&mut hasher);
    Some(
        std::env::temp_dir()
            .join("mux-lsp-untitled")
            .join(format!("{:016x}", hasher.finish()))
            .join("untitled.mux"),
    )
}

fn update_workspace_folders(
    folders: &mut Vec<PathBuf>,
    params: lsp::DidChangeWorkspaceFoldersParams,
) {
    for removed in params.event.removed {
        let Some(path) = workspace_uri_to_path(&removed.uri) else {
            continue;
        };
        folders.retain(|folder| !same_path(folder, &path));
    }
    for added in params.event.added {
        if let Some(path) = workspace_uri_to_path(&added.uri)
            && !folders.iter().any(|folder| same_path(folder, &path))
        {
            folders.push(path);
        }
    }
}

fn workspace_uri_to_path(uri: &lsp::Uri) -> Option<PathBuf> {
    let url = Url::parse(uri.as_str()).ok()?;
    (url.scheme() == "file")
        .then(|| url.to_file_path().ok())
        .flatten()
}

#[allow(deprecated)]
fn initialize_root_path(params: &lsp::InitializeParams) -> Option<PathBuf> {
    params.root_uri.as_ref().and_then(uri_to_path)
}

fn same_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    let left = mux_lang::analysis::absolute_path(left);
    let right = mux_lang::analysis::absolute_path(right);
    left == right
        || matches!((left.canonicalize(), right.canonicalize()), (Ok(left), Ok(right)) if left == right)
}

fn uri_for_path(
    documents: &HashMap<String, OpenDocument>,
    path: &std::path::Path,
) -> Option<lsp::Uri> {
    let wanted = mux_lang::analysis::absolute_path(path);
    if let Some(document) = documents.values().find(|document| {
        let actual = mux_lang::analysis::absolute_path(&document.path);
        actual == wanted
            || matches!((actual.canonicalize(), wanted.canonicalize()), (Ok(left), Ok(right)) if left == right)
    }) {
        return Some(document.uri.clone());
    }
    Url::from_file_path(wanted)
        .ok()
        .and_then(|url| lsp::Uri::from_str(url.as_str()).ok())
}

fn offset_to_position(source: &str, offset: usize) -> lsp::Position {
    let offset = floor_char_boundary(source, offset.min(source.len()));
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line = source[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let character = source[line_start..offset].encode_utf16().count();
    lsp::Position::new(line as u32, character as u32)
}

fn position_to_offset(source: &str, position: lsp::Position) -> usize {
    let mut line_start = 0;
    for (line, text) in source.split_inclusive('\n').enumerate() {
        if line == position.line as usize {
            let content = text
                .strip_suffix('\n')
                .unwrap_or(text)
                .strip_suffix('\r')
                .unwrap_or_else(|| text.strip_suffix('\n').unwrap_or(text));
            let mut utf16_offset = 0;
            for (byte_offset, character) in content.char_indices() {
                let width = character.len_utf16() as u32;
                if utf16_offset + width > position.character {
                    return line_start + byte_offset;
                }
                utf16_offset += width;
                if utf16_offset == position.character {
                    return line_start + byte_offset + character.len_utf8();
                }
            }
            return line_start + content.len();
        }
        line_start += text.len();
    }
    source.len()
}

fn floor_char_boundary(source: &str, mut offset: usize) -> usize {
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::{
        OpenDocument, PendingDiagnostics, RequestTask, WorkerResult, WorkerTask, complete,
        find_member_access_base, formatting_config_directory, lock_or_recover, lsp,
        offset_to_position, position_to_offset, run_request_task, schedule_diagnostics,
        update_workspace_folders,
    };
    use lsp_server::{Message, Request};
    use lsp_types::Position;
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};

    #[test]
    fn positions_use_utf16_units_and_handle_crlf() {
        let source = "a🌟\r\n界";
        assert_eq!(offset_to_position(source, "a🌟".len()), Position::new(0, 3));
        assert_eq!(
            offset_to_position(source, source.len()),
            Position::new(1, 1)
        );
    }

    #[test]
    fn unexpected_worker_requests_return_method_not_found() {
        let request = Request::new(
            "unexpected-request".to_owned().into(),
            "textDocument/unknown".to_owned(),
            serde_json::Value::Null,
        );
        let task = RequestTask {
            request,
            documents: HashMap::new(),
            workspace_folders: Vec::new(),
        };
        let cancelled = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let (sender, receiver) = std::sync::mpsc::channel();

        run_request_task(task, &sender, &cancelled);

        let WorkerResult::Request(result) = receiver.recv().expect("worker should reply") else {
            panic!("worker should return a request result");
        };
        let Message::Response(response) = result.message else {
            panic!("worker should send a JSON-RPC response");
        };
        let error = response
            .response_result
            .expect_err("unsupported request should be an error");
        assert_eq!(error.code, lsp_server::ErrorCode::MethodNotFound as i32);
    }

    #[test]
    fn utf16_positions_map_back_to_utf8_byte_offsets() {
        let source = "a🌟\r\nvalue";
        assert_eq!(position_to_offset(source, Position::new(0, 3)), "a🌟".len());
        assert_eq!(
            position_to_offset(source, Position::new(1, 2)),
            "a🌟\r\nva".len()
        );
        assert_eq!(
            position_to_offset(source, Position::new(1, 99)),
            source.len()
        );
    }

    #[test]
    fn completion_respects_declaration_order_and_nested_blocks() {
        let source = "auto module_value = 0\nfunc main(int parameter) returns void {\n    auto outer = 1\n    if true {\n        auto inner = 2\n        \n    }\n    \n    auto later = 3\n}\n";
        let inner_cursor = source.find("\n        \n").unwrap() + "\n        ".len();
        let outer_cursor = source.find("\n    \n").unwrap() + "\n    ".len();
        let labels_at = |offset| {
            complete(
                source,
                offset_to_position(source, offset),
                std::path::Path::new("/tmp/mux-completion.mux"),
                &HashMap::new(),
            )
            .into_iter()
            .map(|item| item.label)
            .collect::<std::collections::HashSet<_>>()
        };
        let inner_labels = labels_at(inner_cursor);
        assert!(inner_labels.contains("outer"));
        assert!(inner_labels.contains("inner"), "{inner_labels:?}");
        assert!(inner_labels.contains("parameter"));
        assert!(inner_labels.contains("module_value"));
        assert!(!inner_labels.contains("later"));

        let outer_labels = labels_at(outer_cursor);
        assert!(outer_labels.contains("outer"));
        assert!(outer_labels.contains("parameter"));
        assert!(outer_labels.contains("module_value"));
        assert!(!outer_labels.contains("inner"));
        assert!(!outer_labels.contains("later"));
    }

    #[test]
    fn completion_keeps_recovered_locals_in_their_function_and_block_scope() {
        let source = "func first() returns void {\n    auto from_first = 1\n}\nfunc main() returns void {\n    if true {\n        auto from_closed_block = 2\n    }\n    auto visible = 3\n    auto broken = \n    \n}\n";
        let cursor = source.rfind("    \n}").unwrap() + 4;
        assert!(mux_lang::syntax::parse_source(source).has_errors());
        let labels = complete(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion.mux"),
            &HashMap::new(),
        )
        .into_iter()
        .map(|item| item.label)
        .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("visible"), "{labels:?}");
        assert!(!labels.contains("from_first"), "{labels:?}");
        assert!(!labels.contains("from_closed_block"), "{labels:?}");
    }

    #[test]
    fn module_completion_includes_runtime_backed_standard_library_symbols() {
        let source = "import std.io as output\nfunc main() returns void {\n    output.st\n}\n";
        let cursor = source.rfind("output.st").unwrap() + "output.".len();
        let parsed = mux_lang::syntax::parse_source(source);
        let base = find_member_access_base(parsed.tree.root(), cursor);
        assert!(base.is_some());
        let labels = complete(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion.mux"),
            &HashMap::new(),
        )
        .into_iter()
        .map(|item| item.label)
        .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("stdout"), "{labels:?}");
    }

    #[test]
    fn wildcard_import_completion_includes_runtime_backed_standard_library_symbols() {
        let source = "import std.io.*\nfunc main() returns void {\n    st\n}\n";
        let cursor = source.rfind("    st").unwrap() + 6;
        let labels = complete(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion.mux"),
            &HashMap::new(),
        )
        .into_iter()
        .map(|item| item.label)
        .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("stdout"), "{labels:?}");
    }

    #[test]
    fn completion_includes_lambda_parameters_and_captured_locals() {
        let source = "func main() returns void {\n    auto outer = 1\n    auto callback = func(int lambda_arg) returns void {\n        \n    }\n}\n";
        let cursor = source.find("\n        \n").unwrap() + "\n        ".len();
        let labels = complete(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion.mux"),
            &HashMap::new(),
        )
        .into_iter()
        .map(|item| item.label)
        .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("outer"));
        assert!(labels.contains("lambda_arg"));
    }

    #[test]
    fn member_completion_uses_the_resolved_base_type() {
        let source = "class User {\n    string name = \"\"\n    func greet() returns string { return self.name }\n}\nfunc main() returns void {\n    auto user = User.new()\n    user.na\n}\n";
        let cursor = source.rfind("user.na").unwrap() + "user.".len();
        let labels = complete(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion.mux"),
            &HashMap::new(),
        )
        .into_iter()
        .map(|item| item.label)
        .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("name"), "{labels:?}");
        assert!(labels.contains("greet"), "{labels:?}");
    }

    #[test]
    fn member_completion_includes_type_checked_builtin_methods() {
        let source = "func main() returns void {\n    auto count = 1\n    count.to_\n}\n";
        let cursor = source.rfind("count.to_").unwrap() + "count.".len();
        let labels = complete(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion.mux"),
            &HashMap::new(),
        )
        .into_iter()
        .map(|item| item.label)
        .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("to_float"), "{labels:?}");
        assert!(labels.contains("to_string"), "{labels:?}");
        assert!(!labels.contains("push"), "{labels:?}");
    }

    #[test]
    fn member_completion_includes_generic_interface_bound_methods() {
        let source = "interface Drawable {\n    func draw() returns void\n}\nfunc render<T is Drawable & Stringable>(T value) returns void {\n    value.dr\n}\n";
        let cursor = source.rfind("value.dr").unwrap() + "value.".len();
        let parsed = mux_lang::syntax::parse_source(source);
        let base = find_member_access_base(parsed.tree.root(), cursor).unwrap();
        let analysis = mux_lang::analysis::analyze_source(
            std::path::Path::new("/tmp/mux-completion.mux"),
            source,
        );
        let reference = analysis
            .resolved_identifiers
            .iter()
            .find(|reference| reference.usage.byte_range == Some(base))
            .unwrap();
        assert!(
            reference.bound_methods.contains(&"draw".to_owned()),
            "{reference:#?}"
        );
        let labels = complete(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion.mux"),
            &HashMap::new(),
        )
        .into_iter()
        .map(|item| item.label)
        .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("draw"), "{labels:?}");
        assert!(labels.contains("to_string"), "{labels:?}");
    }

    #[test]
    fn member_completion_reads_open_imported_module_overlays() {
        let source = "import tools.*\nfunc main() returns void {\n    auto user = User.new()\n    user.na\n}\n";
        let cursor = source.rfind("user.na").unwrap() + "user.".len();
        let path = std::path::Path::new("/tmp/mux-completion/main.mux");
        let overlays = HashMap::from([(
            std::path::PathBuf::from("/tmp/mux-completion/tools.mux"),
            "class User {\n    string name = \"\"\n    func greet() returns string { return self.name }\n}\n".to_owned(),
        )]);
        let labels = complete(source, offset_to_position(source, cursor), path, &overlays)
            .into_iter()
            .map(|item| item.label)
            .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("name"), "{labels:?}");
        assert!(labels.contains("greet"), "{labels:?}");
    }

    #[test]
    fn member_completion_resolves_imported_module_aliases() {
        let source = "import tools as t\nfunc main() returns void {\n    t.he\n}\n";
        let cursor = source.rfind("t.he").unwrap() + "t.".len();
        let path = std::path::Path::new("/tmp/mux-completion/main.mux");
        let overlays = HashMap::from([(
            std::path::PathBuf::from("/tmp/mux-completion/tools.mux"),
            "func helper() returns void { return }\nfunc other() returns void { return }\n"
                .to_owned(),
        )]);
        let labels = complete(source, offset_to_position(source, cursor), path, &overlays)
            .into_iter()
            .map(|item| item.label)
            .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("helper"), "{labels:?}");
        assert!(labels.contains("other"), "{labels:?}");
    }

    #[test]
    fn completion_includes_only_explicit_import_bindings_and_aliases() {
        let source = "import tools.helper as assist\nimport tools as t\nfunc main() returns void {\n    \n}\n";
        let cursor = source.find("    \n").unwrap() + "    ".len();
        let path = std::path::Path::new("/tmp/mux-completion/main.mux");
        let overlays = HashMap::from([(
            std::path::PathBuf::from("/tmp/mux-completion/tools.mux"),
            "func helper() returns void { return }\nfunc other() returns void { return }\n"
                .to_owned(),
        )]);
        let labels = complete(source, offset_to_position(source, cursor), path, &overlays)
            .into_iter()
            .map(|item| item.label)
            .collect::<std::collections::HashSet<_>>();

        assert!(labels.contains("assist"), "{labels:?}");
        assert!(labels.contains("t"), "{labels:?}");
        assert!(!labels.contains("helper"));
        assert!(!labels.contains("other"));
    }

    #[test]
    fn queued_diagnostics_keep_only_the_latest_snapshot() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let revisions = Arc::new(Mutex::new(HashMap::new()));
        let pending = Arc::new(Mutex::new(PendingDiagnostics::default()));
        let published = std::collections::HashSet::new();
        let path = std::path::PathBuf::from("/tmp/mux-diagnostics/main.mux");
        let uri = lsp::Uri::from_str("untitled:coalesced.mux").unwrap();

        schedule_diagnostics(&sender, &revisions, &pending, &HashMap::new(), &published).unwrap();
        let documents = HashMap::from([(
            uri.as_str().to_owned(),
            OpenDocument {
                uri,
                path,
                source: "func main() returns void { return }\n".to_owned(),
                version: 1,
                revision: 7,
            },
        )]);
        schedule_diagnostics(&sender, &revisions, &pending, &documents, &published).unwrap();

        assert!(matches!(receiver.try_recv(), Ok(WorkerTask::Diagnostics)));
        assert!(receiver.try_recv().is_err());
        let pending = pending.lock().unwrap();
        let latest = pending.latest.as_ref().unwrap();
        assert_eq!(latest.documents.len(), 1);
        assert_eq!(latest.revisions.values().copied().collect::<Vec<_>>(), [7]);
    }

    #[test]
    fn poisoned_server_state_is_recovered_without_panicking() {
        let state = Arc::new(Mutex::new(false));
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            let _guard = worker_state.lock().unwrap();
            panic!("poison test mutex");
        });
        assert!(worker.join().is_err());

        assert!(!*lock_or_recover(&state, "test state"));
        assert!(!state.is_poisoned());
    }

    #[test]
    fn workspace_folder_changes_replace_and_deduplicate_paths() {
        let mut folders = vec![std::path::PathBuf::from("/tmp/mux-workspace-before")];
        let folder = |uri: &str| lsp::WorkspaceFolder {
            uri: lsp::Uri::from_str(uri).unwrap(),
            name: "workspace".to_owned(),
        };
        update_workspace_folders(
            &mut folders,
            lsp::DidChangeWorkspaceFoldersParams {
                event: lsp::WorkspaceFoldersChangeEvent {
                    added: vec![folder("file:///tmp/mux-workspace-after")],
                    removed: vec![folder("file:///tmp/mux-workspace-before")],
                },
            },
        );
        update_workspace_folders(
            &mut folders,
            lsp::DidChangeWorkspaceFoldersParams {
                event: lsp::WorkspaceFoldersChangeEvent {
                    added: vec![folder("file:///tmp/mux-workspace-after")],
                    removed: Vec::new(),
                },
            },
        );

        assert_eq!(
            folders,
            [std::path::PathBuf::from("/tmp/mux-workspace-after")]
        );
    }

    #[test]
    fn untitled_formatting_uses_the_only_workspace_folder() {
        let document = OpenDocument {
            uri: lsp::Uri::from_str("untitled:Untitled-1").unwrap(),
            path: std::path::PathBuf::from("/tmp/mux-lsp-untitled/placeholder/untitled.mux"),
            source: String::new(),
            version: 1,
            revision: 1,
        };
        let folders = vec![std::path::PathBuf::from("/tmp/mux-project")];

        assert_eq!(
            formatting_config_directory(&document, &folders),
            std::path::PathBuf::from("/tmp/mux-project")
        );
        assert_eq!(
            formatting_config_directory(
                &document,
                &[
                    std::path::PathBuf::from("/tmp/mux-project"),
                    std::path::PathBuf::from("/tmp/other-project"),
                ],
            ),
            std::path::PathBuf::from("/tmp/mux-lsp-untitled/placeholder")
        );
    }
}
