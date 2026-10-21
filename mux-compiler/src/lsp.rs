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
    supports_document_changes: bool,
    supports_snippets: bool,
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
    supports_document_changes: bool,
    supports_snippets: bool,
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (connection, io_threads) = Connection::stdio();
    let (workspace_folders, supports_document_changes, supports_snippets) =
        initialize(&connection)?;
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
        supports_document_changes,
        supports_snippets,
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

fn initialize(
    connection: &Connection,
) -> Result<(Vec<PathBuf>, bool, bool), Box<dyn std::error::Error>> {
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    let initialize_params = serde_json::from_value::<lsp::InitializeParams>(initialize_params)?;
    let watch_mux_files = initialize_params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files)
        .and_then(|watched_files| watched_files.dynamic_registration)
        == Some(true);
    let supports_document_changes = initialize_params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.workspace_edit.as_ref())
        .and_then(|edit| edit.document_changes)
        .unwrap_or(false);
    let supports_snippets = initialize_params
        .capabilities
        .text_document
        .as_ref()
        .and_then(|text_document| text_document.completion.as_ref())
        .and_then(|completion| completion.completion_item.as_ref())
        .and_then(|item| item.snippet_support)
        .unwrap_or(false);
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
        references_provider: Some(lsp::OneOf::Left(true)),
        rename_provider: Some(lsp::OneOf::Left(true)),
        hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
        completion_provider: Some(lsp::CompletionOptions {
            trigger_characters: Some(vec![".".to_owned()]),
            resolve_provider: Some(false),
            ..lsp::CompletionOptions::default()
        }),
        semantic_tokens_provider: Some(
            lsp::SemanticTokensServerCapabilities::SemanticTokensOptions(
                lsp::SemanticTokensOptions {
                    work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                    legend: semantic_token_legend(),
                    range: Some(false),
                    full: Some(lsp::SemanticTokensFullOptions::Bool(true)),
                },
            ),
        ),
        signature_help_provider: Some(lsp::SignatureHelpOptions::default()),
        code_action_provider: Some(lsp::CodeActionProviderCapability::Simple(true)),
        inlay_hint_provider: Some(lsp::OneOf::Left(true)),
        folding_range_provider: Some(lsp::FoldingRangeProviderCapability::Simple(true)),
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
    Ok((
        workspace_folders,
        supports_document_changes,
        supports_snippets,
    ))
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
            supports_document_changes: state.supports_document_changes,
            supports_snippets: state.supports_snippets,
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
            | "textDocument/references"
            | "textDocument/rename"
            | "textDocument/hover"
            | "textDocument/completion"
            | "textDocument/semanticTokens/full"
            | "textDocument/signatureHelp"
            | "textDocument/codeAction"
            | "textDocument/inlayHint"
            | "textDocument/foldingRange"
    )
}

const SEMANTIC_TOKEN_TYPE: u32 = 1;
const SEMANTIC_TOKEN_CLASS: u32 = 2;
const SEMANTIC_TOKEN_ENUM: u32 = 3;
const SEMANTIC_TOKEN_INTERFACE: u32 = 4;
const SEMANTIC_TOKEN_METHOD: u32 = 6;
const SEMANTIC_TOKEN_PARAMETER: u32 = 7;
const SEMANTIC_TOKEN_VARIABLE: u32 = 8;
const SEMANTIC_TOKEN_PROPERTY: u32 = 9;

fn semantic_token_legend() -> lsp::SemanticTokensLegend {
    lsp::SemanticTokensLegend {
        token_types: [
            "namespace",
            "type",
            "class",
            "enum",
            "interface",
            "function",
            "method",
            "parameter",
            "variable",
            "property",
            "enumMember",
            "keyword",
            "number",
            "string",
            "comment",
            "operator",
        ]
        .into_iter()
        .map(lsp::SemanticTokenType::new)
        .collect(),
        token_modifiers: ["declaration", "readonly", "defaultLibrary"]
            .into_iter()
            .map(lsp::SemanticTokenModifier::new)
            .collect(),
    }
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
    let workspace_folders = task.workspace_folders.clone();
    let supports_document_changes = task.supports_document_changes;
    let supports_snippets = task.supports_snippets;
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
        "textDocument/references" => handle_references(
            &worker_connection,
            task.request,
            &task.documents,
            &workspace_folders,
        ),
        "textDocument/rename" => handle_rename(
            &worker_connection,
            task.request,
            &task.documents,
            &workspace_folders,
            supports_document_changes,
        ),
        "textDocument/hover" => handle_hover(&worker_connection, task.request, &task.documents),
        "textDocument/completion" => handle_completion(
            &worker_connection,
            task.request,
            &task.documents,
            supports_snippets,
        ),
        "textDocument/semanticTokens/full" => {
            handle_semantic_tokens(&worker_connection, task.request, &task.documents)
        }
        "textDocument/signatureHelp" => {
            handle_signature_help(&worker_connection, task.request, &task.documents)
        }
        "textDocument/codeAction" => handle_code_action(
            &worker_connection,
            task.request,
            &task.documents,
            supports_document_changes,
        ),
        "textDocument/inlayHint" => {
            handle_inlay_hints(&worker_connection, task.request, &task.documents)
        }
        "textDocument/foldingRange" => {
            handle_folding_ranges(&worker_connection, task.request, &task.documents)
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
            let (mut config, warnings) = crate::format_config::load_from_directory(
                &formatting_config_directory(document, workspace_folders),
            );
            for warning in warnings {
                eprintln!("warning: {warning}");
            }
            if !config.enabled {
                return None;
            }
            config.options.indent_type = if params.options.insert_spaces {
                mux_lang::formatter::IndentType::Space
            } else {
                mux_lang::formatter::IndentType::Tab
            };
            config.options.indent_count = params.options.tab_size.max(1) as usize;
            mux_lang::formatter::format_source_with_options(&document.source, config.options)
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
        })
        .unwrap_or_default();
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
                    .usage_path
                    .as_deref()
                    .is_some_and(|path| same_path(path, &document.path))
                    && resolved_identifier_usage_range(reference, &document.source)
                        .is_some_and(|range| range.start <= offset && offset < range.end)
            })
            .min_by_key(|reference| {
                resolved_identifier_usage_range(reference, &document.source)
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
        let name_offset = declaration_text.find(&reference.source_name)?;
        let start = declaration.start + name_offset;
        let range = lsp::Range::new(
            offset_to_position(target_source, start),
            offset_to_position(target_source, start + reference.source_name.len()),
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

#[derive(Clone)]
struct WorkspaceSource {
    path: PathBuf,
    uri: lsp::Uri,
    source: String,
}

#[derive(Clone)]
struct SymbolTarget {
    path: PathBuf,
    source_name: String,
    name_range: mux_lang::lexer::ByteRange,
    uri: lsp::Uri,
    member_owner_name: Option<String>,
    import_alias: Option<ImportAliasTarget>,
}

#[derive(Clone)]
struct ImportAliasTarget {
    imported_source_name: String,
    local_name: String,
    imported_source_path: PathBuf,
}

fn handle_references(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
    workspace_folders: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::ReferenceParams>(request.params)?;
    let key = params.text_document_position.text_document.uri.as_str();
    let references = documents
        .get(key)
        .and_then(|document| {
            let target =
                symbol_target_at(document, params.text_document_position.position, documents)?;
            workspace_references(&target, documents, workspace_folders, true).map(
                |mut locations| {
                    if params.context.include_declaration
                        && let Some(range) =
                            lsp_range_for_bytes(&target.path, &target.name_range, documents)
                    {
                        locations.push(lsp::Location::new(target.uri.clone(), range));
                    }
                    locations.sort_by(|left, right| {
                        left.uri
                            .as_str()
                            .cmp(right.uri.as_str())
                            .then_with(|| left.range.start.cmp(&right.range.start))
                    });
                    locations.dedup();
                    locations
                },
            )
        })
        .unwrap_or_default();
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(references)?).into())?;
    Ok(())
}

fn handle_rename(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
    workspace_folders: &[PathBuf],
    supports_document_changes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::RenameParams>(request.params)?;
    if !valid_mux_identifier(&params.new_name) {
        connection.sender.send(
            Response::new_err(
                request.id,
                lsp_server::ErrorCode::InvalidParams as i32,
                "new name is not a valid Mux identifier".to_owned(),
            )
            .into(),
        )?;
        return Ok(());
    }
    let key = params.text_document_position.text_document.uri.as_str();
    let edit = documents.get(key).and_then(|document| {
        let target = symbol_target_at(document, params.text_document_position.position, documents)?;
        let references = workspace_references(&target, documents, workspace_folders, false)?;
        let mut changes = HashMap::<String, (lsp::Uri, Vec<lsp::TextEdit>)>::new();
        let declaration_range = lsp_range_for_bytes(&target.path, &target.name_range, documents)?;
        changes
            .entry(target.uri.as_str().to_owned())
            .or_insert_with(|| (target.uri.clone(), Vec::new()))
            .1
            .push(lsp::TextEdit::new(
                declaration_range,
                params.new_name.clone(),
            ));
        for location in references {
            let uri_key = location.uri.as_str().to_owned();
            changes
                .entry(uri_key)
                .or_insert_with(|| (location.uri, Vec::new()))
                .1
                .push(lsp::TextEdit::new(location.range, params.new_name.clone()));
        }
        for (_, edits) in changes.values_mut() {
            edits.sort_by(|left, right| right.range.start.cmp(&left.range.start));
            edits.dedup_by(|left, right| left.range == right.range);
        }
        Some(versioned_workspace_edit(
            changes,
            documents,
            supports_document_changes,
        ))
    });
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(edit)?).into())?;
    Ok(())
}

fn versioned_workspace_edit(
    changes: HashMap<String, (lsp::Uri, Vec<lsp::TextEdit>)>,
    documents: &HashMap<String, OpenDocument>,
    supports_document_changes: bool,
) -> lsp::WorkspaceEdit {
    if !supports_document_changes {
        return lsp::WorkspaceEdit {
            changes: Some(changes.into_values().collect()),
            ..lsp::WorkspaceEdit::default()
        };
    }
    let versions = documents
        .values()
        .map(|document| (document.uri.as_str().to_owned(), document.version))
        .collect::<HashMap<_, _>>();
    let mut changes = changes.into_values().collect::<Vec<_>>();
    changes.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    let document_changes = changes
        .into_iter()
        .map(|(uri, edits)| {
            let version = versions.get(uri.as_str()).copied();
            lsp::TextDocumentEdit {
                text_document: lsp::OptionalVersionedTextDocumentIdentifier { version, uri },
                edits: edits.into_iter().map(lsp::OneOf::Left).collect(),
            }
        })
        .collect();
    lsp::WorkspaceEdit {
        document_changes: Some(lsp::DocumentChanges::Edits(document_changes)),
        ..lsp::WorkspaceEdit::default()
    }
}

fn valid_mux_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
        && mux_lang::syntax::parse_source(&format!("auto {name} = 0"))
            .errors
            .is_empty()
}

fn symbol_target_at(
    document: &OpenDocument,
    position: lsp::Position,
    documents: &HashMap<String, OpenDocument>,
) -> Option<SymbolTarget> {
    let offset = position_to_offset(&document.source, position);
    let overlays = documents
        .values()
        .map(|open| (open.path.clone(), open.source.clone()))
        .collect::<HashMap<_, _>>();
    let analysis = mux_lang::analysis::analyze_source_with_overlays(
        &document.path,
        &document.source,
        &overlays,
    );
    let selected = analysis
        .resolved_identifiers
        .iter()
        .filter(|reference| {
            reference
                .usage_path
                .as_deref()
                .is_some_and(|path| same_path(path, &document.path))
                && resolved_identifier_range_at(reference, &document.source, offset)
                    .is_some_and(|range| range.start <= offset && offset < range.end)
        })
        .min_by_key(|reference| {
            resolved_identifier_range_at(reference, &document.source, offset)
                .map_or(usize::MAX, |range| range.end - range.start)
        })
        .or_else(|| {
            analysis
                .resolved_identifiers
                .iter()
                .filter(|reference| {
                    let owner = reference.source_path.as_deref().unwrap_or(&document.path);
                    same_path(owner, &document.path)
                        && reference
                            .declaration
                            .and_then(|span| span.byte_range)
                            .and_then(|range| {
                                declaration_name_range(
                                    &document.source,
                                    range,
                                    &reference.source_name,
                                )
                            })
                            .is_some_and(|range| range.start <= offset && offset < range.end)
                })
                .min_by_key(|reference| {
                    reference
                        .declaration
                        .and_then(|span| span.byte_range)
                        .map_or(usize::MAX, |range| range.end - range.start)
                })
        });
    if let Some(reference) = selected {
        let alias_reference = if reference.is_import {
            Some(reference)
        } else if reference.name != reference.source_name {
            analysis.resolved_identifiers.iter().find(|import| {
                import.is_import
                    && import.name == reference.name
                    && import.source_name == reference.source_name
                    && import
                        .usage_path
                        .as_deref()
                        .is_some_and(|path| same_path(path, &document.path))
                    && import.source_path.as_deref().is_some_and(|path| {
                        reference
                            .source_path
                            .as_deref()
                            .is_some_and(|target| same_path(path, target))
                    })
            })
        } else {
            None
        };
        if let Some(import) = alias_reference
            && let Some(alias_range) = import_alias_name_range(
                &document.source,
                import.usage.byte_range?,
                &import.source_name,
                &import.name,
            )
            && ((reference.is_import && alias_range.start <= offset && offset < alias_range.end)
                || (!reference.is_import && reference.name != reference.source_name))
        {
            return symbol_target_from_alias(document, reference, alias_range);
        }
        return symbol_target_from_reference(reference, &analysis, documents, &document.uri);
    }

    if let Some(member) = analysis.resolved_members.iter().find(|member| {
        member
            .usage_path
            .as_deref()
            .is_some_and(|path| same_path(path, &document.path))
            && member.usage.start <= offset
            && offset < member.usage.end
    }) {
        return symbol_target_from_member(member, &analysis, documents);
    }

    let (symbol, owner_name) = document_symbols(&document.source)
        .iter()
        .find_map(|symbol| document_symbol_at_with_owner(symbol, position, None))?;
    let start = position_to_offset(&document.source, symbol.selection_range.start);
    let end = position_to_offset(&document.source, symbol.selection_range.end);
    Some(SymbolTarget {
        path: document.path.clone(),
        source_name: symbol.name,
        name_range: mux_lang::lexer::ByteRange::new(start, end),
        uri: document.uri.clone(),
        member_owner_name: owner_name,
        import_alias: None,
    })
}

fn document_symbol_at_with_owner(
    symbol: &lsp::DocumentSymbol,
    position: lsp::Position,
    owner_name: Option<String>,
) -> Option<(lsp::DocumentSymbol, Option<String>)> {
    if range_contains(symbol.selection_range, position) {
        let member_owner = matches!(
            symbol.kind,
            lsp::SymbolKind::FIELD | lsp::SymbolKind::METHOD | lsp::SymbolKind::ENUM_MEMBER
        )
        .then_some(owner_name)
        .flatten();
        return Some((symbol.clone(), member_owner));
    }
    let next_owner = matches!(
        symbol.kind,
        lsp::SymbolKind::CLASS | lsp::SymbolKind::ENUM | lsp::SymbolKind::INTERFACE
    )
    .then(|| symbol.name.clone())
    .or(owner_name);
    symbol
        .children
        .as_deref()?
        .iter()
        .find_map(|child| document_symbol_at_with_owner(child, position, next_owner.clone()))
}

fn symbol_target_from_reference(
    reference: &mux_lang::semantics::ResolvedIdentifier,
    analysis: &mux_lang::analysis::Analysis,
    documents: &HashMap<String, OpenDocument>,
    current_uri: &lsp::Uri,
) -> Option<SymbolTarget> {
    let declaration = reference.declaration?.byte_range?;
    let file_id = reference
        .source_path
        .as_deref()
        .and_then(|path| find_file_id(&analysis.files, path))
        .unwrap_or(analysis.root_file);
    let path = analysis.files.path(file_id)?.to_path_buf();
    let source = analysis.files.source(file_id)?;
    let name_range = declaration_name_range(source, declaration, &reference.source_name)?;
    let uri = if same_path(&path, analysis.files.path(analysis.root_file)?) {
        current_uri.clone()
    } else {
        uri_for_path(documents, &path)?
    };
    Some(SymbolTarget {
        path,
        source_name: reference.source_name.clone(),
        name_range,
        uri,
        member_owner_name: None,
        import_alias: None,
    })
}

fn symbol_target_from_alias(
    document: &OpenDocument,
    reference: &mux_lang::semantics::ResolvedIdentifier,
    name_range: mux_lang::lexer::ByteRange,
) -> Option<SymbolTarget> {
    Some(SymbolTarget {
        path: document.path.clone(),
        source_name: reference.name.clone(),
        name_range,
        uri: document.uri.clone(),
        member_owner_name: None,
        import_alias: Some(ImportAliasTarget {
            imported_source_name: reference.source_name.clone(),
            local_name: reference.name.clone(),
            imported_source_path: reference.source_path.clone()?,
        }),
    })
}

fn symbol_target_from_member(
    member: &mux_lang::semantics::ResolvedMember,
    analysis: &mux_lang::analysis::Analysis,
    documents: &HashMap<String, OpenDocument>,
) -> Option<SymbolTarget> {
    let file_id = find_file_id(&analysis.files, &member.source_path)?;
    let source = analysis.files.source(file_id)?;
    let name_range = member_name_range(source, &member.owner_name, &member.name)?;
    let path = analysis.files.path(file_id)?.to_path_buf();
    let uri = uri_for_path(documents, &path)?;
    Some(SymbolTarget {
        path,
        source_name: member.name.clone(),
        name_range,
        uri,
        member_owner_name: Some(member.owner_name.clone()),
        import_alias: None,
    })
}

fn member_name_range(
    source: &str,
    owner_name: &str,
    member_name: &str,
) -> Option<mux_lang::lexer::ByteRange> {
    fn find_owner<'a>(
        symbols: &'a [lsp::DocumentSymbol],
        owner_name: &str,
    ) -> Option<&'a lsp::DocumentSymbol> {
        for symbol in symbols {
            if symbol.name == owner_name
                && matches!(
                    symbol.kind,
                    lsp::SymbolKind::CLASS | lsp::SymbolKind::ENUM | lsp::SymbolKind::INTERFACE
                )
            {
                return Some(symbol);
            }
            if let Some(owner) = symbol
                .children
                .as_deref()
                .and_then(|children| find_owner(children, owner_name))
            {
                return Some(owner);
            }
        }
        None
    }
    let symbols = document_symbols(source);
    let owner = find_owner(&symbols, owner_name)?;
    let selection = owner
        .children
        .as_deref()?
        .iter()
        .find(|member| member.name == member_name)?
        .selection_range;
    Some(mux_lang::lexer::ByteRange::new(
        position_to_offset(source, selection.start),
        position_to_offset(source, selection.end),
    ))
}

fn resolved_identifier_usage_range(
    reference: &mux_lang::semantics::ResolvedIdentifier,
    source: &str,
) -> Option<mux_lang::lexer::ByteRange> {
    let usage = reference.usage.byte_range?;
    if !reference.is_import {
        return Some(usage);
    }
    import_item_name_range(source, usage, &reference.source_name)
}

fn resolved_identifier_range_at(
    reference: &mux_lang::semantics::ResolvedIdentifier,
    source: &str,
    offset: usize,
) -> Option<mux_lang::lexer::ByteRange> {
    let usage = reference.usage.byte_range?;
    if !reference.is_import {
        return Some(usage);
    }
    let alias = import_alias_name_range(source, usage, &reference.source_name, &reference.name);
    alias
        .filter(|range| range.start <= offset && offset < range.end)
        .or_else(|| import_item_name_range(source, usage, &reference.source_name))
}

fn import_item_name_range(
    source: &str,
    statement: mux_lang::lexer::ByteRange,
    expected_name: &str,
) -> Option<mux_lang::lexer::ByteRange> {
    import_name_ranges(source, statement)?
        .into_iter()
        .find_map(|(item, _)| {
            (source.get(item.start..item.end) == Some(expected_name)).then_some(item)
        })
}

fn import_alias_name_range(
    source: &str,
    statement: mux_lang::lexer::ByteRange,
    imported_name: &str,
    alias_name: &str,
) -> Option<mux_lang::lexer::ByteRange> {
    import_name_ranges(source, statement)?
        .into_iter()
        .find_map(|(item, alias)| {
            let alias = alias?;
            (source.get(item.start..item.end) == Some(imported_name)
                && source.get(alias.start..alias.end) == Some(alias_name))
            .then_some(alias)
        })
}

fn import_name_ranges(
    source: &str,
    statement: mux_lang::lexer::ByteRange,
) -> Option<
    Vec<(
        mux_lang::lexer::ByteRange,
        Option<mux_lang::lexer::ByteRange>,
    )>,
> {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement, SyntaxImportSpec};

    fn visit(
        node: &mux_lang::syntax::SyntaxNode,
        statement: mux_lang::lexer::ByteRange,
    ) -> Option<
        Vec<(
            mux_lang::lexer::ByteRange,
            Option<mux_lang::lexer::ByteRange>,
        )>,
    > {
        if let Some(Data::Import { spec, ast_span, .. }) = node.lowering_data()
            && *ast_span == statement
        {
            let names = match spec {
                SyntaxImportSpec::Item { item, alias } => vec![(*item, alias.as_ref().copied())],
                SyntaxImportSpec::Items { items } => items
                    .iter()
                    .map(|(item, alias)| (*item, alias.as_ref().copied()))
                    .collect(),
                SyntaxImportSpec::Module { .. } | SyntaxImportSpec::Wildcard => Vec::new(),
            };
            return Some(names);
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child
                && let Some(ranges) = visit(child, statement)
            {
                return Some(ranges);
            }
        }
        None
    }

    let parsed = mux_lang::syntax::parse_source(source);
    visit(parsed.tree.root(), statement)
}

fn declaration_name_range(
    source: &str,
    declaration: mux_lang::lexer::ByteRange,
    expected_name: &str,
) -> Option<mux_lang::lexer::ByteRange> {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};

    fn visit(
        source: &str,
        node: &mux_lang::syntax::SyntaxNode,
        declaration: mux_lang::lexer::ByteRange,
        expected_name: &str,
        candidates: &mut Vec<(usize, mux_lang::lexer::ByteRange)>,
    ) {
        let candidate = match node.lowering_data() {
            Some(
                Data::VariableDeclaration { name, .. }
                | Data::Function { name, .. }
                | Data::Class { name, .. }
                | Data::Interface { name, .. }
                | Data::Enum { name, .. },
            ) => Some(*name),
            Some(
                Data::Parameter { name, .. }
                | Data::Field { name, .. }
                | Data::TypeParameter { name, .. }
                | Data::EnumVariant { name, .. },
            ) => Some(*name),
            Some(Data::ForStatement { variable, .. }) => Some(*variable),
            _ => None,
        };
        let node_range = node.range();
        if let Some(range) = candidate
            && source.get(range.start..range.end) == Some(expected_name)
            && node_range.start <= declaration.start
            && declaration.end <= node_range.end
        {
            candidates.push((node_range.end - node_range.start, range));
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                visit(source, child, declaration, expected_name, candidates);
            }
        }
    }

    let parsed = mux_lang::syntax::parse_source(source);
    let mut candidates = Vec::new();
    visit(
        source,
        parsed.tree.root(),
        declaration,
        expected_name,
        &mut candidates,
    );
    candidates
        .into_iter()
        .min_by_key(|(weight, range)| (*weight, range.start))
        .map(|(_, range)| range)
}

fn workspace_references(
    target: &SymbolTarget,
    documents: &HashMap<String, OpenDocument>,
    workspace_folders: &[PathBuf],
    include_aliased_uses: bool,
) -> Option<Vec<lsp::Location>> {
    let sources = collect_workspace_sources(documents, workspace_folders);
    let overlays = sources
        .iter()
        .map(|source| (source.path.clone(), source.source.clone()))
        .collect::<HashMap<_, _>>();
    let mut locations = Vec::new();
    for source in sources {
        let analysis = mux_lang::analysis::analyze_source_with_overlays(
            &source.path,
            &source.source,
            &overlays,
        );
        if let Some(alias) = &target.import_alias {
            locations.extend(alias_references_for_source(
                &source, &analysis, target, alias,
            ));
            continue;
        }
        locations.extend(member_references_for_source(&source, &analysis, target));
        if target.member_owner_name.is_some() {
            continue;
        }
        locations.extend(identifier_references_for_source(
            &source,
            &analysis,
            target,
            include_aliased_uses,
        ));
    }
    Some(locations)
}

fn alias_references_for_source(
    source: &WorkspaceSource,
    analysis: &mux_lang::analysis::Analysis,
    target: &SymbolTarget,
    alias: &ImportAliasTarget,
) -> Vec<lsp::Location> {
    if !same_path(&source.path, &target.path) {
        return Vec::new();
    }
    analysis
        .resolved_identifiers
        .iter()
        .filter(|reference| {
            !reference.is_import
                && reference.name == alias.local_name
                && reference.source_name == alias.imported_source_name
                && reference
                    .source_path
                    .as_deref()
                    .is_some_and(|path| same_path(path, &alias.imported_source_path))
                && reference
                    .usage_path
                    .as_deref()
                    .is_some_and(|path| same_path(path, &source.path))
        })
        .filter_map(|reference| {
            let usage = reference.usage.byte_range?;
            let range = lsp_range(&source.source, usage)?;
            Some(lsp::Location::new(source.uri.clone(), range))
        })
        .collect()
}

fn member_references_for_source(
    source: &WorkspaceSource,
    analysis: &mux_lang::analysis::Analysis,
    target: &SymbolTarget,
) -> Vec<lsp::Location> {
    analysis
        .resolved_members
        .iter()
        .filter(|member| {
            target.member_owner_name.as_deref() == Some(member.owner_name.as_str())
                && same_path(&member.source_path, &target.path)
                && member.name == target.source_name
                && member
                    .usage_path
                    .as_deref()
                    .is_some_and(|path| same_path(path, &source.path))
        })
        .filter_map(|member| {
            let range = lsp_range(&source.source, member.usage)?;
            Some(lsp::Location::new(source.uri.clone(), range))
        })
        .collect()
}

fn identifier_references_for_source(
    source: &WorkspaceSource,
    analysis: &mux_lang::analysis::Analysis,
    target: &SymbolTarget,
    include_aliased_uses: bool,
) -> Vec<lsp::Location> {
    analysis
        .resolved_identifiers
        .iter()
        .filter_map(|reference| {
            identifier_reference_range(source, analysis, target, reference, include_aliased_uses)
                .map(|range| lsp::Location::new(source.uri.clone(), range))
        })
        .collect()
}

fn identifier_reference_range(
    source: &WorkspaceSource,
    analysis: &mux_lang::analysis::Analysis,
    target: &SymbolTarget,
    reference: &mux_lang::semantics::ResolvedIdentifier,
    include_aliased_uses: bool,
) -> Option<lsp::Range> {
    let declaration = reference.declaration.and_then(|span| span.byte_range)?;
    if !include_aliased_uses && !reference.is_import && reference.name != reference.source_name {
        return None;
    }
    let owner = reference.source_path.as_deref().unwrap_or(&source.path);
    let usage_path = reference.usage_path.as_deref()?;
    if !same_path(usage_path, &source.path)
        || !same_path(owner, &target.path)
        || reference.source_name != target.source_name
    {
        return None;
    }
    let owner_id = reference
        .source_path
        .as_deref()
        .and_then(|path| find_file_id(&analysis.files, path))
        .unwrap_or(analysis.root_file);
    let owner_source = analysis.files.source(owner_id)?;
    let name_range = declaration_name_range(owner_source, declaration, &reference.source_name)?;
    if name_range.start != target.name_range.start {
        return None;
    }
    let usage = resolved_identifier_usage_range(reference, &source.source)?;
    lsp_range(&source.source, usage)
}

fn collect_workspace_sources(
    documents: &HashMap<String, OpenDocument>,
    workspace_folders: &[PathBuf],
) -> Vec<WorkspaceSource> {
    let mut sources = HashMap::<PathBuf, WorkspaceSource>::new();
    for document in documents.values() {
        let key = normalized_path(&document.path);
        sources.insert(
            key,
            WorkspaceSource {
                path: document.path.clone(),
                uri: document.uri.clone(),
                source: document.source.clone(),
            },
        );
    }
    let mut pending = Vec::new();
    for folder in workspace_folders {
        pending.push(folder.clone());
    }
    let mut visited = std::collections::HashSet::new();
    while let Some(directory) = pending.pop() {
        let key = normalized_path(&directory);
        if !visited.insert(key) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            if name == ".git" || name == "target" || name == "node_modules" || name == ".worktrees"
            {
                continue;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file()
                && path.extension().is_some_and(|extension| extension == "mux")
            {
                let key = normalized_path(&path);
                if sources.contains_key(&key) {
                    continue;
                }
                let Ok(source) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(url) = Url::from_file_path(&path) else {
                    continue;
                };
                let Ok(uri) = lsp::Uri::from_str(url.as_str()) else {
                    continue;
                };
                sources.insert(key, WorkspaceSource { path, uri, source });
            }
        }
    }
    let mut result = sources.into_values().collect::<Vec<_>>();
    result.sort_by(|left, right| left.path.cmp(&right.path));
    result
}

fn normalized_path(path: &std::path::Path) -> PathBuf {
    let absolute = mux_lang::analysis::absolute_path(path);
    absolute.canonicalize().unwrap_or(absolute)
}

fn lsp_range_for_bytes(
    path: &std::path::Path,
    range: &mux_lang::lexer::ByteRange,
    documents: &HashMap<String, OpenDocument>,
) -> Option<lsp::Range> {
    let source = documents
        .values()
        .find(|document| same_path(&document.path, path))
        .map(|document| document.source.clone());
    if let Some(source) = source {
        return lsp_range(&source, *range);
    }
    let source = std::fs::read_to_string(path).ok()?;
    lsp_range(&source, *range)
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
        let resolved_hover = analysis
            .resolved_identifiers
            .iter()
            .find_map(|reference| {
                if !reference
                    .usage_path
                    .as_deref()
                    .is_some_and(|path| same_path(path, &document.path))
                {
                    return None;
                }
                let usage = resolved_identifier_usage_range(reference, &document.source)?;
                (usage.start <= offset && offset < usage.end).then_some((reference, usage))
            })
            .or_else(|| {
                analysis.resolved_identifiers.iter().find_map(|reference| {
                    let declaration = reference.declaration?.byte_range?;
                    let text = document.source.get(declaration.start..declaration.end)?;
                    let start = declaration.start + text.find(&reference.name)?;
                    let name_range =
                        mux_lang::lexer::ByteRange::new(start, start + reference.name.len());
                    (name_range.start <= offset && offset < name_range.end)
                        .then_some((reference, name_range))
                })
            })
            .and_then(|(reference, symbol_range)| {
                let type_ = reference.type_.as_ref()?;
                let range = symbol_range;
                let kind = symbol_kind_name(&reference.kind);
                let declaration_file = reference
                    .source_path
                    .as_deref()
                    .and_then(|path| find_file_id(&analysis.files, path))
                    .unwrap_or(analysis.root_file);
                let declaration_source = analysis
                    .files
                    .source(declaration_file)
                    .unwrap_or(&document.source);
                let declaration_range = reference.declaration.and_then(|span| span.byte_range);
                let type_detail = display_callable_type(
                    declaration_source,
                    &reference.name,
                    type_,
                    reference.declaration.and_then(|span| span.byte_range),
                );
                let mut value = format!("```mux\n{}: {type_detail}\n```", reference.name);
                value.push_str(&format!("\n\n**{kind}**"));
                if matches!(
                    &reference.kind,
                    mux_lang::semantics::SymbolKind::Class
                        | mux_lang::semantics::SymbolKind::Interface
                        | mux_lang::semantics::SymbolKind::Enum
                ) && let Some(header) = declaration_range.and_then(|declaration| {
                    declaration_header(declaration_source, declaration.start)
                }) {
                    value.push_str(&format!("\n\n```mux\n{header}\n```"));
                }
                if let Some(documentation) = declaration_range.and_then(|declaration| {
                    documentation_comment_before(declaration_source, declaration.start)
                }) {
                    value.push_str("\n\n");
                    value.push_str(&documentation);
                }
                Some(lsp::Hover {
                    contents: lsp::HoverContents::Markup(lsp::MarkupContent {
                        kind: lsp::MarkupKind::Markdown,
                        value,
                    }),
                    range: lsp_range(&document.source, range),
                })
            });
        resolved_hover.or_else(|| type_hover_at(&document.source, offset))
    });
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(hover)?).into())?;
    Ok(())
}

fn documentation_comment_before(source: &str, declaration_start: usize) -> Option<String> {
    let line_start = source[..declaration_start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    if !source.get(line_start..declaration_start)?.trim().is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    let mut cursor = line_start;
    while cursor > 0 {
        let previous_end = cursor - 1;
        let previous_start = source[..previous_end]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let line = source[previous_start..previous_end].trim();
        if line.is_empty() || !line.starts_with("//") {
            break;
        }
        lines.push(line.trim_start_matches('/').trim().to_owned());
        cursor = previous_start;
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
}

fn declaration_header(source: &str, declaration_start: usize) -> Option<&str> {
    let declaration = source.get(declaration_start..)?;
    let end = declaration
        .find('{')
        .unwrap_or_else(|| declaration.find(['\n', '\r']).unwrap_or(declaration.len()));
    let header = declaration.get(..end)?.trim();
    (!header.is_empty()).then_some(header)
}

fn type_hover_at(source: &str, offset: usize) -> Option<lsp::Hover> {
    fn contains_type_name(node: &mux_lang::syntax::SyntaxNode, offset: usize) -> bool {
        use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
        let in_type = match node.lowering_data() {
            Some(Data::TypeName { name, .. } | Data::TraitBound { name, .. }) => {
                name.start <= offset && offset < name.end
            }
            Some(
                Data::Class { name, .. }
                | Data::Interface { name, .. }
                | Data::Enum { name, .. }
                | Data::TypeParameter { name, .. },
            ) => name.start <= offset && offset < name.end,
            _ => false,
        };
        in_type
            || node.children().iter().any(|child| match child {
                SyntaxElement::Node(child) => contains_type_name(child, offset),
                SyntaxElement::Token(_) => false,
            })
    }

    fn find_type_parameter(
        node: &mux_lang::syntax::SyntaxNode,
        source: &str,
        name: &str,
    ) -> Option<(mux_lang::lexer::ByteRange, String)> {
        use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
        if let Some(Data::TypeParameter {
            name: name_range,
            bounds,
        }) = node.lowering_data()
            && source.get(name_range.start..name_range.end) == Some(name)
        {
            let bounds = bounds
                .iter()
                .filter_map(|range| source.get(range.start..range.end))
                .collect::<Vec<_>>();
            let detail = if bounds.is_empty() {
                format!("Type parameter `{name}`")
            } else {
                format!("Type parameter `{name}`\n\nBounds: {}", bounds.join(", "))
            };
            return Some((*name_range, detail));
        }
        node.children().iter().find_map(|child| match child {
            SyntaxElement::Node(child) => find_type_parameter(child, source, name),
            SyntaxElement::Token(_) => None,
        })
    }

    let parsed = mux_lang::syntax::parse_source(source);
    if !contains_type_name(parsed.tree.root(), offset) {
        return None;
    }
    let (name, range) = identifier_at(source, offset)?;
    let (detail, docs) =
        if let Some((range, detail)) = find_type_parameter(parsed.tree.root(), source, name) {
            (detail, documentation_comment_before(source, range.start))
        } else if BUILTIN_TYPE_NAMES.contains(&name) {
            (format!("Built-in type `{name}`"), None)
        } else {
            let symbols = document_symbols(source);
            let symbol = find_document_symbol(&symbols, name)?;
            if !matches!(
                symbol.kind,
                lsp::SymbolKind::CLASS | lsp::SymbolKind::INTERFACE | lsp::SymbolKind::ENUM
            ) {
                return None;
            }
            let start = position_to_offset(source, symbol.range.start);
            let header = declaration_header(source, start)?;
            let kind = match symbol.kind {
                lsp::SymbolKind::CLASS => "Class",
                lsp::SymbolKind::INTERFACE => "Interface",
                _ => "Enum",
            };
            (
                format!("{kind} `{name}`\n\n```mux\n{header}\n```"),
                documentation_comment_before(source, start),
            )
        };
    let value = docs.map_or(detail.clone(), |docs| format!("{docs}\n\n{detail}"));
    Some(lsp::Hover {
        contents: lsp::HoverContents::Markup(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value,
        }),
        range: lsp_range(source, range),
    })
}

fn identifier_at(source: &str, offset: usize) -> Option<(&str, mux_lang::lexer::ByteRange)> {
    let bytes = source.as_bytes();
    if offset >= bytes.len() || !is_identifier_byte(bytes[offset]) {
        return None;
    }
    let start = (0..offset)
        .rev()
        .find(|index| !is_identifier_byte(bytes[*index]))
        .map_or(0, |index| index + 1);
    let end = (offset..bytes.len())
        .find(|index| !is_identifier_byte(bytes[*index]))
        .unwrap_or(bytes.len());
    Some((
        source.get(start..end)?,
        mux_lang::lexer::ByteRange::new(start, end),
    ))
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn find_document_symbol<'a>(
    symbols: &'a [lsp::DocumentSymbol],
    name: &str,
) -> Option<&'a lsp::DocumentSymbol> {
    symbols.iter().find_map(|symbol| {
        if symbol.name == name {
            Some(symbol)
        } else {
            symbol
                .children
                .as_deref()
                .and_then(|children| find_document_symbol(children, name))
        }
    })
}

const BUILTIN_TYPE_NAMES: &[&str] = &[
    "string",
    "bool",
    "void",
    "int",
    "float",
    "char",
    "byte",
    "bytes",
    "optional",
    "result",
    "list",
    "map",
    "tuple",
    "set",
    "range",
    "Stringable",
    "Hashable",
    "Thread",
    "Error",
    "Self",
];

fn display_type(type_: &mux_lang::semantics::types::Type) -> String {
    use mux_lang::semantics::types::Type;

    match type_ {
        Type::Primitive(primitive) => match primitive {
            mux_lang::ast::PrimitiveType::Int => "int",
            mux_lang::ast::PrimitiveType::Float => "float",
            mux_lang::ast::PrimitiveType::Bool => "bool",
            mux_lang::ast::PrimitiveType::Char => "char",
            mux_lang::ast::PrimitiveType::Byte => "byte",
            mux_lang::ast::PrimitiveType::Bytes => "bytes",
            mux_lang::ast::PrimitiveType::Str => "string",
            mux_lang::ast::PrimitiveType::Void => "void",
            mux_lang::ast::PrimitiveType::Auto => "auto",
        }
        .to_owned(),
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

fn display_callable_type(
    source: &str,
    name: &str,
    type_: &mux_lang::semantics::types::Type,
    declaration: Option<mux_lang::lexer::ByteRange>,
) -> String {
    use mux_lang::semantics::types::Type;
    let Type::Function {
        params, returns, ..
    } = type_
    else {
        return display_type(type_);
    };
    let parameter_names = function_parameter_names(source, name, declaration);
    let parameters = params
        .iter()
        .enumerate()
        .map(|(index, type_)| {
            let parameter = parameter_names
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("param{}", index + 1));
            format!("{parameter}: {}", display_type(type_))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("func({parameters}) -> {}", display_type(returns))
}

fn function_parameter_names(
    source: &str,
    function_name: &str,
    declaration: Option<mux_lang::lexer::ByteRange>,
) -> Vec<String> {
    let parsed = mux_lang::syntax::parse_source(source);
    find_function_parameter_names(parsed.tree.root(), source, function_name, declaration)
        .unwrap_or_default()
}

fn find_function_parameter_names(
    node: &mux_lang::syntax::SyntaxNode,
    source: &str,
    function_name: &str,
    declaration: Option<mux_lang::lexer::ByteRange>,
) -> Option<Vec<String>> {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
    if let Some(Data::Function {
        name, parameters, ..
    }) = node.lowering_data()
        && source.get(name.start..name.end) == Some(function_name)
        && declaration
            .is_none_or(|range| range.start <= node.range().end && node.range().start <= range.end)
    {
        return Some(
            parameters
                .iter()
                .enumerate()
                .map(|(index, range)| {
                    find_parameter_name(node, source, *range)
                        .unwrap_or_else(|| format!("param{}", index + 1))
                })
                .collect(),
        );
    }
    node.children().iter().find_map(|child| match child {
        SyntaxElement::Node(child) => {
            find_function_parameter_names(child, source, function_name, declaration)
        }
        SyntaxElement::Token(_) => None,
    })
}

fn find_parameter_name(
    node: &mux_lang::syntax::SyntaxNode,
    source: &str,
    range: mux_lang::lexer::ByteRange,
) -> Option<String> {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
    if node.range().start <= range.start
        && range.end <= node.range().end
        && let Some(Data::Parameter { name, .. }) = node.lowering_data()
    {
        return source.get(name.start..name.end).map(str::to_owned);
    }
    node.children().iter().find_map(|child| match child {
        SyntaxElement::Node(child)
            if child.range().start <= range.start && range.end <= child.range().end =>
        {
            find_parameter_name(child, source, range)
        }
        SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
    })
}

fn symbol_kind_name(kind: &mux_lang::semantics::SymbolKind) -> &'static str {
    use mux_lang::semantics::SymbolKind;
    match kind {
        SymbolKind::Function => "Function",
        SymbolKind::Variable => "Variable",
        SymbolKind::Class => "Class",
        SymbolKind::Interface => "Interface",
        SymbolKind::Enum => "Enum",
        SymbolKind::Constant => "Constant",
        SymbolKind::Import => "Module",
        SymbolKind::Type => "Type parameter",
    }
}

fn handle_completion(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
    supports_snippets: bool,
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
            complete_with_snippet_support(
                &document.source,
                params.text_document_position.position,
                &document.path,
                &overlays,
                supports_snippets,
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

fn handle_semantic_tokens(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::SemanticTokensParams>(request.params)?;
    let tokens = documents
        .get(params.text_document.uri.as_str())
        .map(|document| semantic_tokens(document, documents))
        .unwrap_or_default();
    connection.sender.send(
        Response::new_ok(
            request.id,
            serde_json::to_value(lsp::SemanticTokensResult::Tokens(tokens))?,
        )
        .into(),
    )?;
    Ok(())
}

fn semantic_tokens(
    document: &OpenDocument,
    documents: &HashMap<String, OpenDocument>,
) -> lsp::SemanticTokens {
    let overlays = documents
        .values()
        .map(|open| (open.path.clone(), open.source.clone()))
        .collect::<HashMap<_, _>>();
    let analysis = mux_lang::analysis::analyze_source_with_overlays(
        &document.path,
        &document.source,
        &overlays,
    );
    let parsed = mux_lang::syntax::parse_source(&document.source);
    let mut semantic = HashMap::<(usize, usize), (u32, u32)>::new();
    collect_declaration_semantics(parsed.tree.root(), &mut semantic);
    collect_type_reference_semantics(
        &document.source,
        parsed.tree.root(),
        parsed.tree.tokens(),
        &mut semantic,
    );
    collect_optional_some_semantics(parsed.tree.root(), &document.source, &mut semantic);
    let parameter_type_ranges = collect_parameter_type_ranges(parsed.tree.root());

    for reference in &analysis.resolved_identifiers {
        add_resolved_identifier_semantic(
            document,
            &parsed.tree,
            &parameter_type_ranges,
            reference,
            &mut semantic,
        );
    }

    collect_member_access_semantics(parsed.tree.root(), &mut semantic);
    encode_semantic_tokens(&document.source, parsed.tree.tokens(), semantic)
}

fn add_resolved_identifier_semantic(
    document: &OpenDocument,
    tree: &mux_lang::syntax::SyntaxTree,
    parameter_type_ranges: &std::collections::HashSet<(usize, usize)>,
    reference: &mux_lang::semantics::ResolvedIdentifier,
    semantic: &mut HashMap<(usize, usize), (u32, u32)>,
) {
    if !reference
        .usage_path
        .as_deref()
        .is_some_and(|path| same_path(path, &document.path))
    {
        return;
    }
    let Some(range) = resolved_identifier_usage_range(reference, &document.source) else {
        return;
    };
    let Some(kind) = document.source.get(range.start..range.end) else {
        return;
    };
    let declaration_is_local = reference
        .source_path
        .as_deref()
        .is_none_or(|path| same_path(path, &document.path));
    let declaration_range = declaration_is_local
        .then(|| reference.declaration.and_then(|span| span.byte_range))
        .flatten();
    let declared_semantic = declaration_range.and_then(|declaration| {
        semantic
            .iter()
            .filter(|((start, end), _)| declaration.start <= *start && *end <= declaration.end)
            .filter(|((start, end), _)| {
                document.source.get(*start..*end) == Some(reference.name.as_str())
            })
            .min_by_key(|((start, end), _)| end - start)
    });
    let declared_token_type = declared_semantic.map(|(_, (token_type, _))| *token_type);
    let is_parameter = declaration_range
        .is_some_and(|range| parameter_type_ranges.contains(&(range.start, range.end)));
    let type_reference = semantic
        .get(&(range.start, range.end))
        .map(|(token_type, _)| *token_type)
        .filter(|token_type| {
            matches!(
                *token_type,
                SEMANTIC_TOKEN_TYPE
                    | SEMANTIC_TOKEN_CLASS
                    | SEMANTIC_TOKEN_ENUM
                    | SEMANTIC_TOKEN_INTERFACE
            )
        });
    let token_type = if reference.name == "some"
        && reference.declaration.is_none()
        && matches!(&reference.kind, mux_lang::semantics::SymbolKind::Function)
    {
        SEMANTIC_TOKEN_TYPE
    } else {
        type_reference.unwrap_or_else(|| {
            declared_token_type.unwrap_or_else(|| {
                if is_parameter {
                    SEMANTIC_TOKEN_PARAMETER
                } else {
                    semantic_type_for_symbol(&reference.kind, kind, tree, range.start)
                }
            })
        })
    };
    let modifiers = if reference.declaration.is_none() {
        4
    } else if matches!(&reference.kind, mux_lang::semantics::SymbolKind::Constant)
        || declared_semantic.is_some_and(|(_, (_, modifiers))| *modifiers & 2 != 0)
    {
        2
    } else {
        0
    };
    semantic.insert((range.start, range.end), (token_type, modifiers));
    add_declaration_semantic_modifier(
        document,
        reference,
        declaration_is_local,
        token_type,
        semantic,
    );
}

fn add_declaration_semantic_modifier(
    document: &OpenDocument,
    reference: &mux_lang::semantics::ResolvedIdentifier,
    declaration_is_local: bool,
    token_type: u32,
    semantic: &mut HashMap<(usize, usize), (u32, u32)>,
) {
    let Some(declaration) = reference
        .declaration
        .and_then(|span| span.byte_range)
        .filter(|_| declaration_is_local)
    else {
        return;
    };
    let Some(declaration_text) = document.source.get(declaration.start..declaration.end) else {
        return;
    };
    let Some(name_offset) = declaration_text.find(&reference.name) else {
        return;
    };
    let start = declaration.start + name_offset;
    let end = start + reference.name.len();
    let existing_modifiers = semantic
        .get(&(start, end))
        .map_or(0, |(_, modifiers)| *modifiers);
    semantic.insert((start, end), (token_type, existing_modifiers | 1));
}

fn encode_semantic_tokens(
    source: &str,
    tokens: &[mux_lang::syntax::SyntaxToken],
    semantic: HashMap<(usize, usize), (u32, u32)>,
) -> lsp::SemanticTokens {
    let mut raw = Vec::<(u32, u32, u32, u32, u32)>::new();
    for token in tokens {
        let range = token.range();
        if range.start == range.end {
            continue;
        }
        let Some(text) = source.get(range.start..range.end) else {
            continue;
        };
        let Some(token_type) = lexical_semantic_type(token.kind(), text) else {
            continue;
        };
        let (token_type, modifiers) = semantic
            .get(&(range.start, range.end))
            .copied()
            .unwrap_or((token_type, 0));

        // LSP semantic tokens may not cross line boundaries. Split multiline
        // comments/strings into UTF-16-length segments on each source line.
        let mut segment_start = range.start;
        for (relative, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                let mut segment_end = range.start + relative;
                if source.as_bytes().get(segment_end.saturating_sub(1)) == Some(&b'\r') {
                    segment_end -= 1;
                }
                if segment_start < segment_end {
                    push_semantic_segment(
                        source,
                        segment_start,
                        segment_end,
                        token_type,
                        modifiers,
                        &mut raw,
                    );
                }
                segment_start = range.start + relative + 1;
            }
        }
        if segment_start < range.end {
            push_semantic_segment(
                source,
                segment_start,
                range.end,
                token_type,
                modifiers,
                &mut raw,
            );
        }
    }
    raw.sort_unstable_by_key(|(line, start, _, _, _)| (*line, *start));
    raw.dedup_by_key(|(line, start, _, _, _)| (*line, *start));
    let mut previous_line = 0;
    let mut previous_start = 0;
    let data = raw
        .into_iter()
        .map(|(line, start, length, token_type, modifiers)| {
            let delta_line = line - previous_line;
            let delta_start = if delta_line == 0 {
                start - previous_start
            } else {
                start
            };
            previous_line = line;
            previous_start = start;
            lsp::SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type,
                token_modifiers_bitset: modifiers,
            }
        })
        .collect();
    lsp::SemanticTokens {
        result_id: None,
        data,
    }
}

fn collect_declaration_semantics(
    node: &mux_lang::syntax::SyntaxNode,
    semantic: &mut HashMap<(usize, usize), (u32, u32)>,
) {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
    let declaration = match node.lowering_data() {
        Some(Data::Function { name, .. }) => Some((*name, 5, 1)),
        Some(Data::Class { name, .. }) => Some((*name, SEMANTIC_TOKEN_CLASS, 1)),
        Some(Data::Interface { name, .. }) => Some((*name, SEMANTIC_TOKEN_INTERFACE, 1)),
        Some(Data::Enum { name, .. }) => Some((*name, SEMANTIC_TOKEN_ENUM, 1)),
        Some(Data::Parameter { name, .. }) => Some((*name, SEMANTIC_TOKEN_PARAMETER, 1)),
        Some(Data::TypeParameter { name, .. }) => Some((*name, SEMANTIC_TOKEN_TYPE, 1)),
        Some(Data::VariableDeclaration { kind, name, .. }) => Some((
            *name,
            SEMANTIC_TOKEN_VARIABLE,
            if *kind == mux_lang::syntax::VariableDeclarationKind::Const {
                3
            } else {
                1
            },
        )),
        Some(Data::Field { name, is_const, .. }) => Some((
            *name,
            SEMANTIC_TOKEN_PROPERTY,
            if *is_const { 3 } else { 1 },
        )),
        Some(Data::EnumVariant { name, .. }) => Some((*name, 10, 1)),
        _ => None,
    };
    if let Some((range, token_type, modifiers)) = declaration {
        semantic.insert((range.start, range.end), (token_type, modifiers));
    }
    for child in node.children() {
        if let SyntaxElement::Node(child) = child {
            collect_declaration_semantics(child, semantic);
        }
    }
}

fn collect_parameter_type_ranges(
    node: &mux_lang::syntax::SyntaxNode,
) -> std::collections::HashSet<(usize, usize)> {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
    let mut ranges = std::collections::HashSet::new();
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if let Some(Data::Parameter { type_range, .. }) = node.lowering_data() {
            ranges.insert((type_range.start, type_range.end));
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                pending.push(child);
            }
        }
    }
    ranges
}

fn collect_type_reference_semantics(
    source: &str,
    root: &mux_lang::syntax::SyntaxNode,
    tokens: &[mux_lang::syntax::SyntaxToken],
    semantic: &mut HashMap<(usize, usize), (u32, u32)>,
) {
    fn collect_type_kinds(
        source: &str,
        node: &mux_lang::syntax::SyntaxNode,
        names: &mut HashMap<String, u32>,
    ) {
        use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
        let declared = match node.lowering_data() {
            Some(Data::Class { name, .. }) => Some((*name, SEMANTIC_TOKEN_CLASS)),
            Some(Data::Interface { name, .. }) => Some((*name, SEMANTIC_TOKEN_INTERFACE)),
            Some(Data::Enum { name, .. }) => Some((*name, SEMANTIC_TOKEN_ENUM)),
            Some(Data::TypeParameter { name, .. }) => Some((*name, SEMANTIC_TOKEN_TYPE)),
            _ => None,
        };
        if let Some((range, token_type)) = declared
            && let Some(name) = source.get(range.start..range.end)
        {
            names.insert(name.to_owned(), token_type);
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                collect_type_kinds(source, child, names);
            }
        }
    }

    fn collect_type_uses(
        source: &str,
        node: &mux_lang::syntax::SyntaxNode,
        tokens: &[mux_lang::syntax::SyntaxToken],
        names: &HashMap<String, u32>,
        semantic: &mut HashMap<(usize, usize), (u32, u32)>,
    ) {
        use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
        let range = match node.lowering_data() {
            Some(Data::TypeName { name, .. } | Data::TraitBound { name, .. }) => Some(*name),
            _ => None,
        };
        if let Some(range) = range {
            for token in tokens.iter().filter(|token| {
                range.start <= token.range().start
                    && token.range().end <= range.end
                    && matches!(token.kind(), mux_lang::lexer::TokenType::Id(_))
            }) {
                let token_range = token.range();
                if let Some(name) = source.get(token_range.start..token_range.end) {
                    let token_type = names.get(name).copied().unwrap_or(SEMANTIC_TOKEN_TYPE);
                    semantic.insert((token_range.start, token_range.end), (token_type, 0));
                }
            }
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                collect_type_uses(source, child, tokens, names, semantic);
            }
        }
    }

    let mut names = HashMap::new();
    collect_type_kinds(source, root, &mut names);
    collect_type_uses(source, root, tokens, &names, semantic);
}

fn collect_optional_some_semantics(
    node: &mux_lang::syntax::SyntaxNode,
    source: &str,
    semantic: &mut HashMap<(usize, usize), (u32, u32)>,
) {
    use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement, SyntaxPattern};

    if let Some(Data::Pattern(SyntaxPattern::EnumVariant { name, .. })) = node.lowering_data()
        && source.get(name.start..name.end) == Some("some")
    {
        semantic.insert((name.start, name.end), (SEMANTIC_TOKEN_TYPE, 0));
    }
    for child in node.children() {
        if let SyntaxElement::Node(child) = child {
            collect_optional_some_semantics(child, source, semantic);
        }
    }
}

fn collect_member_access_semantics(
    root: &mux_lang::syntax::SyntaxNode,
    semantic: &mut HashMap<(usize, usize), (u32, u32)>,
) {
    fn collect_call_callees(
        node: &mux_lang::syntax::SyntaxNode,
        call_callees: &mut std::collections::HashSet<mux_lang::lexer::ByteRange>,
    ) {
        use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
        if let Some(Data::Call { callee, .. }) = node.lowering_data() {
            call_callees.insert(*callee);
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                collect_call_callees(child, call_callees);
            }
        }
    }

    fn classify_member_accesses(
        node: &mux_lang::syntax::SyntaxNode,
        call_callees: &std::collections::HashSet<mux_lang::lexer::ByteRange>,
        semantic: &mut HashMap<(usize, usize), (u32, u32)>,
    ) {
        use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};
        if let Some(Data::FieldAccess { field, .. }) = node.lowering_data() {
            let token_type = if call_callees.contains(&node.range()) {
                SEMANTIC_TOKEN_METHOD
            } else {
                SEMANTIC_TOKEN_PROPERTY
            };
            semantic.insert((field.start, field.end), (token_type, 0));
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child {
                classify_member_accesses(child, call_callees, semantic);
            }
        }
    }

    let mut call_callees = std::collections::HashSet::new();
    collect_call_callees(root, &mut call_callees);
    classify_member_accesses(root, &call_callees, semantic);
}

fn push_semantic_segment(
    source: &str,
    start: usize,
    end: usize,
    token_type: u32,
    modifiers: u32,
    output: &mut Vec<(u32, u32, u32, u32, u32)>,
) {
    let start_position = offset_to_position(source, start);
    let end_position = offset_to_position(source, end);
    let length = end_position
        .character
        .saturating_sub(start_position.character);
    if length > 0 {
        output.push((
            start_position.line,
            start_position.character,
            length,
            token_type,
            modifiers,
        ));
    }
}

fn lexical_semantic_type(kind: &mux_lang::lexer::TokenType, text: &str) -> Option<u32> {
    use mux_lang::lexer::TokenType;
    Some(match kind {
        TokenType::Func
        | TokenType::Returns
        | TokenType::Return
        | TokenType::Use
        | TokenType::If
        | TokenType::Else
        | TokenType::For
        | TokenType::While
        | TokenType::Match
        | TokenType::Const
        | TokenType::Class
        | TokenType::Interface
        | TokenType::Enum
        | TokenType::Import
        | TokenType::Is
        | TokenType::As
        | TokenType::In
        | TokenType::Break
        | TokenType::Continue
        | TokenType::Common
        | TokenType::Where
        | TokenType::Test => 11,
        TokenType::Auto | TokenType::None => SEMANTIC_TOKEN_TYPE,
        TokenType::LineComment(_) | TokenType::MultilineComment(_) => 14,
        TokenType::Str(_) | TokenType::Bytes(_) | TokenType::Char(_) => 13,
        TokenType::Int(_) | TokenType::Float(_) => 12,
        TokenType::Bool(_) => 11,
        TokenType::Id(_)
            if matches!(
                text,
                "int"
                    | "float"
                    | "bool"
                    | "char"
                    | "byte"
                    | "bytes"
                    | "string"
                    | "void"
                    | "optional"
                    | "result"
                    | "list"
                    | "map"
                    | "tuple"
                    | "set"
                    | "range"
                    | "Stringable"
                    | "Hashable"
                    | "Thread"
                    | "Error"
                    | "Self"
            ) =>
        {
            SEMANTIC_TOKEN_TYPE
        }
        TokenType::Id(_)
            if matches!(
                text,
                "func"
                    | "returns"
                    | "return"
                    | "use"
                    | "if"
                    | "else"
                    | "for"
                    | "while"
                    | "match"
                    | "const"
                    | "class"
                    | "interface"
                    | "enum"
                    | "import"
                    | "is"
                    | "as"
                    | "in"
                    | "break"
                    | "continue"
                    | "common"
                    | "where"
                    | "test"
            ) =>
        {
            11
        }
        TokenType::Id(_) => SEMANTIC_TOKEN_VARIABLE,
        TokenType::Eq
        | TokenType::Plus
        | TokenType::Minus
        | TokenType::Star
        | TokenType::StarStar
        | TokenType::Slash
        | TokenType::Percent
        | TokenType::Lt
        | TokenType::Gt
        | TokenType::Le
        | TokenType::Ge
        | TokenType::EqEq
        | TokenType::NotEq
        | TokenType::Bang
        | TokenType::Incr
        | TokenType::Decr
        | TokenType::PlusEq
        | TokenType::MinusEq
        | TokenType::StarEq
        | TokenType::SlashEq
        | TokenType::PercentEq
        | TokenType::And
        | TokenType::Or
        | TokenType::Ref
        | TokenType::DotDot => 15,
        _ => return None,
    })
}

fn semantic_type_for_symbol(
    kind: &mux_lang::semantics::SymbolKind,
    name: &str,
    tree: &mux_lang::syntax::SyntaxTree,
    offset: usize,
) -> u32 {
    use mux_lang::semantics::SymbolKind;
    use mux_lang::syntax::SyntaxElement;
    match kind {
        SymbolKind::Function => {
            let mut previous_was_dot = false;
            for token in tree
                .tokens()
                .iter()
                .filter(|token| token.range().end <= offset)
            {
                if matches!(
                    token.kind(),
                    mux_lang::lexer::TokenType::Whitespace | mux_lang::lexer::TokenType::NewLine
                ) {
                    continue;
                }
                previous_was_dot = matches!(token.kind(), mux_lang::lexer::TokenType::Dot);
            }
            if previous_was_dot { 6 } else { 5 }
        }
        SymbolKind::Class => 2,
        SymbolKind::Interface => 4,
        SymbolKind::Enum => 3,
        SymbolKind::Type => 1,
        SymbolKind::Import => 0,
        SymbolKind::Constant => 8,
        SymbolKind::Variable => {
            let mut found_parameter = false;
            let mut stack = vec![tree.root()];
            while let Some(node) = stack.pop() {
                if node.range().start <= offset
                    && offset < node.range().end
                    && matches!(node.kind(), mux_lang::syntax::SyntaxKind::Parameter)
                {
                    found_parameter = true;
                }
                for child in node.children() {
                    if let SyntaxElement::Node(child) = child {
                        stack.push(child);
                    }
                }
            }
            let _ = name;
            if found_parameter { 7 } else { 8 }
        }
    }
}

#[cfg(test)]
fn complete(
    source: &str,
    position: lsp::Position,
    path: &std::path::Path,
    overlays: &HashMap<PathBuf, String>,
) -> Vec<lsp::CompletionItem> {
    complete_with_snippet_support(source, position, path, overlays, true)
}

fn complete_with_snippet_support(
    source: &str,
    position: lsp::Position,
    path: &std::path::Path,
    overlays: &HashMap<PathBuf, String>,
    supports_snippets: bool,
) -> Vec<lsp::CompletionItem> {
    let symbols = document_symbols(source);
    let offset = position_to_offset(source, position);
    let prefix = source[..offset]
        .rsplit(|character: char| !(character.is_alphanumeric() || character == '_'))
        .next()
        .unwrap_or_default();
    let mut candidates =
        std::collections::BTreeMap::<String, Option<lsp::CompletionItemKind>>::new();
    let imports = collect_imports(source);
    let parsed = mux_lang::syntax::parse_source(source);
    let member_base = find_member_access_base(parsed.tree.root(), offset);
    if member_base.is_none() {
        collect_completions(&symbols, position, false, &mut candidates);
        collect_visible_syntax_completions(source, offset, &mut candidates);
        collect_visible_type_parameter_completions(source, offset, &mut candidates);
    }
    let analysis = mux_lang::analysis::analyze_source_with_overlays(path, source, overlays);
    if !imports.is_empty() || member_base.is_some() {
        collect_import_completions(source, path, &analysis, &imports, &mut candidates);
        collect_member_completions(source, path, &analysis, member_base, &mut candidates);
    }
    if member_base.is_none() {
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
        for type_name in BUILTIN_TYPE_NAMES {
            candidates
                .entry((*type_name).to_owned())
                .or_insert(Some(lsp::CompletionItemKind::CLASS));
        }
    }
    candidates
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, kind)| {
            let reference = analysis
                .resolved_identifiers
                .iter()
                .find(|reference| reference.name == name);
            let type_detail = reference
                .and_then(|reference| reference.type_.as_ref().map(|type_| (reference, type_)))
                .map(|(reference, type_)| {
                    display_callable_type(
                        source,
                        &name,
                        type_,
                        reference.declaration.and_then(|span| span.byte_range),
                    )
                });
            let function = reference
                .and_then(|reference| reference.type_.as_ref())
                .is_some_and(|type_| {
                    matches!(type_, mux_lang::semantics::types::Type::Function { .. })
                });
            let function = function
                || kind == Some(lsp::CompletionItemKind::FUNCTION)
                || kind == Some(lsp::CompletionItemKind::METHOD);
            let prefix_end = offset;
            let next_is_call = source[prefix_end..]
                .trim_start_matches([' ', '\t'])
                .starts_with('(');
            let use_snippet = supports_snippets && function && !next_is_call;
            let documentation = type_detail.as_ref().map(|detail| {
                lsp::Documentation::MarkupContent(lsp::MarkupContent {
                    kind: lsp::MarkupKind::Markdown,
                    value: format!("```mux\n{name}: {detail}\n```"),
                })
            });
            lsp::CompletionItem {
                label: name.clone(),
                kind,
                detail: type_detail.or_else(|| kind.map(completion_kind_name).map(str::to_owned)),
                documentation,
                insert_text: Some(if use_snippet {
                    format!("{name}($0)")
                } else {
                    name.clone()
                }),
                insert_text_format: Some(if use_snippet {
                    lsp::InsertTextFormat::SNIPPET
                } else {
                    lsp::InsertTextFormat::PLAIN_TEXT
                }),
                ..lsp::CompletionItem::default()
            }
        })
        .collect()
}

fn completion_kind_name(kind: lsp::CompletionItemKind) -> &'static str {
    if kind == lsp::CompletionItemKind::FUNCTION {
        "Function"
    } else if kind == lsp::CompletionItemKind::METHOD {
        "Method"
    } else if kind == lsp::CompletionItemKind::CLASS {
        "Class"
    } else if kind == lsp::CompletionItemKind::MODULE {
        "Module"
    } else if kind == lsp::CompletionItemKind::CONSTANT {
        "Constant"
    } else if kind == lsp::CompletionItemKind::KEYWORD {
        "Keyword"
    } else if kind == lsp::CompletionItemKind::FIELD {
        "Field"
    } else if kind == lsp::CompletionItemKind::TYPE_PARAMETER {
        "Type"
    } else {
        "Variable"
    }
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
        SymbolKind::Type => lsp::CompletionItemKind::TYPE_PARAMETER,
        SymbolKind::Variable | SymbolKind::Import => lsp::CompletionItemKind::VARIABLE,
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

fn collect_visible_type_parameter_completions(
    source: &str,
    offset: usize,
    candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
) {
    fn visit(
        source: &str,
        node: &mux_lang::syntax::SyntaxNode,
        offset: usize,
        candidates: &mut std::collections::BTreeMap<String, Option<lsp::CompletionItemKind>>,
    ) {
        use mux_lang::syntax::{AstLoweringData as Data, SyntaxElement};

        if node.range().start <= offset && offset <= node.range().end {
            let type_parameters: &[mux_lang::lexer::ByteRange] = match node.lowering_data() {
                Some(
                    Data::Function {
                        type_parameters, ..
                    }
                    | Data::Class {
                        type_parameters, ..
                    }
                    | Data::Interface {
                        type_parameters, ..
                    }
                    | Data::Enum {
                        type_parameters, ..
                    },
                ) => type_parameters.as_slice(),
                _ => &[],
            };
            for range in type_parameters {
                if let Some(parameter) = find_syntax_node(node, *range)
                    && let Some(Data::TypeParameter { name, .. }) = parameter.lowering_data()
                {
                    insert_syntax_name(
                        source,
                        *name,
                        lsp::CompletionItemKind::TYPE_PARAMETER,
                        candidates,
                    );
                }
            }
            for child in node.children() {
                if let SyntaxElement::Node(child) = child
                    && child.range().start <= offset
                    && offset <= child.range().end
                {
                    visit(source, child, offset, candidates);
                }
            }
        }
    }

    let parsed = mux_lang::syntax::parse_source(source);
    visit(source, parsed.tree.root(), offset, candidates);
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
        let parameter_names = function_parameter_names(
            &document.source,
            &reference.name,
            reference.declaration.and_then(|span| span.byte_range),
        );
        let parameter_labels = params
            .iter()
            .enumerate()
            .map(|(index, type_)| {
                let name = parameter_names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("param{}", index + 1));
                format!("{name}: {}", display_type(type_))
            })
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
    supports_document_changes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::CodeActionParams>(request.params)?;
    let Some(document) = documents.get(params.text_document.uri.as_str()) else {
        connection
            .sender
            .send(Response::new_ok(request.id, serde_json::json!([])).into())?;
        return Ok(());
    };
    if params
        .context
        .only
        .as_ref()
        .is_some_and(|kinds| !kinds.iter().any(|kind| kind.as_str() == "quickfix"))
    {
        connection
            .sender
            .send(Response::new_ok(request.id, serde_json::json!([])).into())?;
        return Ok(());
    }
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
            code_actions_for_diagnostic(
                diagnostic,
                &analysis,
                &document.path,
                &sources,
                documents,
                supports_document_changes,
            )
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
    documents: &HashMap<String, OpenDocument>,
    supports_document_changes: bool,
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
            let mut changes = HashMap::<String, (lsp::Uri, Vec<lsp::TextEdit>)>::new();
            for edit in group {
                let (Some(uri), Some((_, source))) = (
                    sources.get(&edit.file_id)?.0.clone(),
                    sources.get(&edit.file_id),
                ) else {
                    return None;
                };
                changes
                    .entry(uri.as_str().to_owned())
                    .or_insert_with(|| (uri, Vec::new()))
                    .1
                    .push(lsp::TextEdit {
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
                edit: Some(versioned_workspace_edit(
                    changes,
                    documents,
                    supports_document_changes,
                )),
                command: None,
                is_preferred: Some(true),
                disabled: None,
                data: None,
            })
        })
        .collect()
}

fn handle_inlay_hints(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::InlayHintParams>(request.params)?;
    let key = params.text_document.uri.as_str();
    let hints = documents.get(key).map_or_else(Vec::new, |document| {
        let overlays = documents
            .values()
            .map(|open| (open.path.clone(), open.source.clone()))
            .collect::<HashMap<_, _>>();
        let analysis = mux_lang::analysis::analyze_source_with_overlays(
            &document.path,
            &document.source,
            &overlays,
        );
        analysis
            .inferred_bindings
            .iter()
            .filter(|binding| {
                binding
                    .source_path
                    .as_deref()
                    .is_some_and(|path| same_path(path, &document.path))
            })
            .filter_map(|binding| {
                let declaration = binding.span.byte_range?;
                let name_range =
                    declaration_name_range(&document.source, declaration, &binding.name)?;
                let end = name_range.end;
                if !document.source.is_char_boundary(end) {
                    return None;
                }
                let position = offset_to_position(&document.source, end);
                if position < params.range.start || position > params.range.end {
                    return None;
                }
                Some(lsp::InlayHint {
                    position,
                    label: lsp::InlayHintLabel::String(format!(
                        ": {}",
                        display_type(&binding.type_)
                    )),
                    kind: Some(lsp::InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                })
            })
            .collect()
    });
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(hints)?).into())?;
    Ok(())
}

fn handle_folding_ranges(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, OpenDocument>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = serde_json::from_value::<lsp::FoldingRangeParams>(request.params)?;
    let key = params.text_document.uri.as_str();
    let ranges = documents.get(key).map_or_else(Vec::new, |document| {
        let parsed = mux_lang::syntax::parse_source(&document.source);
        let mut folds = Vec::new();
        collect_folding_ranges(parsed.tree.root(), &document.source, &mut folds);
        folds.sort_by_key(|range| (range.start_line, range.end_line, range.start_character));
        folds.dedup_by(|left, right| {
            left.start_line == right.start_line
                && left.end_line == right.end_line
                && left.start_character == right.start_character
        });
        folds
    });
    connection
        .sender
        .send(Response::new_ok(request.id, serde_json::to_value(ranges)?).into())?;
    Ok(())
}

fn collect_folding_ranges(
    node: &mux_lang::syntax::SyntaxNode,
    source: &str,
    output: &mut Vec<lsp::FoldingRange>,
) {
    use mux_lang::syntax::{SyntaxElement, SyntaxKind};
    let range = node.range();
    let start = offset_to_position(source, range.start);
    let mut end = offset_to_position(source, range.end);
    if end.character == 0 && end.line > start.line {
        end.line -= 1;
        end.character = source
            .lines()
            .nth(end.line as usize)
            .map_or(0, |line| line.encode_utf16().count() as u32);
    }
    if end.line > start.line
        && matches!(
            node.kind(),
            SyntaxKind::FunctionDeclaration
                | SyntaxKind::ClassDeclaration
                | SyntaxKind::ClassMethod
                | SyntaxKind::InterfaceDeclaration
                | SyntaxKind::EnumDeclaration
                | SyntaxKind::TestDeclaration
                | SyntaxKind::Block
                | SyntaxKind::IfStatement
                | SyntaxKind::WhileStatement
                | SyntaxKind::ForStatement
                | SyntaxKind::MatchStatement
                | SyntaxKind::MatchExpression
        )
    {
        output.push(lsp::FoldingRange {
            start_line: start.line,
            start_character: Some(start.character),
            end_line: end.line,
            end_character: Some(end.character),
            kind: Some(lsp::FoldingRangeKind::Region),
            collapsed_text: None,
        });
    }
    for child in node.children() {
        if let SyntaxElement::Node(child) = child {
            collect_folding_ranges(child, source, output);
        }
    }
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
        complete_with_snippet_support, find_member_access_base, formatting_config_directory,
        lock_or_recover, lsp, offset_to_position, position_to_offset, run_request_task,
        schedule_diagnostics, semantic_tokens, update_workspace_folders,
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
            supports_document_changes: false,
            supports_snippets: false,
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
    fn completion_offers_primitive_types_as_types() {
        let source = "func main() returns void {\n    \n}\n";
        let offset = source.find("    \n").unwrap() + 4;
        let items = complete(
            source,
            offset_to_position(source, offset),
            std::path::Path::new("/tmp/mux-completion-types.mux"),
            &HashMap::new(),
        );
        for type_name in ["int", "string"] {
            let item = items.iter().find(|item| item.label == type_name).unwrap();
            assert_eq!(item.kind, Some(lsp::CompletionItemKind::CLASS));
            assert_eq!(item.detail.as_deref(), Some("Class"));
        }
    }

    #[test]
    fn completion_offers_in_scope_generic_type_parameters_as_types() {
        let source = "func identity<T>(T value) returns T {\n    T\n}\n";
        let offset = source.rfind("T\n").unwrap() + 1;
        let items = complete(
            source,
            offset_to_position(source, offset),
            std::path::Path::new("/tmp/mux-completion-generic-types.mux"),
            &HashMap::new(),
        );
        let parameter = items.iter().find(|item| item.label == "T").unwrap();
        assert_eq!(
            parameter.kind,
            Some(lsp::CompletionItemKind::TYPE_PARAMETER)
        );
    }

    #[test]
    fn semantic_type_names_are_classified_as_types_at_use_sites() {
        let source = "class Calculator {}\nfunc consume(Calculator value) returns Calculator { return value }\n";
        let path = std::path::PathBuf::from("/tmp/mux-semantic-types.mux");
        let uri = lsp::Uri::from_str("file:///tmp/mux-semantic-types.mux").unwrap();
        let document = OpenDocument {
            uri: uri.clone(),
            path,
            source: source.to_owned(),
            version: 1,
            revision: 1,
        };
        let tokens = semantic_tokens(
            &document,
            &HashMap::from([(uri.as_str().to_owned(), document.clone())]),
        );
        let offset = source.find("Calculator value").unwrap();
        let position = offset_to_position(source, offset);
        let mut line = 0;
        let mut character = 0;
        let actual = tokens.data.into_iter().find_map(|token| {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            (line == position.line && character == position.character).then_some(token.token_type)
        });
        assert_eq!(
            actual,
            Some(2),
            "custom type use should use the class token kind"
        );
    }

    #[test]
    fn type_hover_shows_type_summary_and_adjacent_documentation() {
        let source = "/// Performs arithmetic.\nclass Calculator {\n  int value\n}\nfunc consume(Calculator value) returns void { return }\n";
        let offset = source.find("Calculator value").unwrap();
        let hover = super::type_hover_at(source, offset).expect("type hover");
        let lsp::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover should use markdown");
        };
        assert!(contents.value.contains("Performs arithmetic."));
        assert!(contents.value.contains("class Calculator"));
        assert!(!contents.value.contains("int value"));
    }

    #[test]
    fn optional_some_uses_the_same_semantic_token_kind_as_none() {
        let source = "func main() returns void {\n    match some(15) {\n        some(value) { print(value) }\n        none { print(\"none\") }\n    }\n}\n";
        let path = std::path::PathBuf::from("/tmp/mux-some-semantic-tokens.mux");
        let uri = lsp::Uri::from_str("file:///tmp/mux-some-semantic-tokens.mux").unwrap();
        let document = OpenDocument {
            uri: uri.clone(),
            path,
            source: source.to_owned(),
            version: 1,
            revision: 1,
        };
        let documents = HashMap::from([(uri.as_str().to_owned(), document.clone())]);
        let tokens = semantic_tokens(&document, &documents);

        let mut line = 0;
        let mut character = 0;
        let mut decoded = Vec::new();
        for token in tokens.data {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            decoded.push((line, character, token.token_type));
        }

        for offset in [
            source.find("some(15)").unwrap(),
            source.find("some(value)").unwrap(),
            source.find("none {").unwrap(),
        ] {
            let position = offset_to_position(source, offset);
            let (_, _, token_type) = decoded
                .iter()
                .find(|(line, start, _)| *line == position.line && *start == position.character)
                .expect("optional variant semantic token");
            assert_eq!(*token_type, 1, "some and none should use type coloring");
        }
    }

    #[test]
    fn semantic_tokens_preserve_collection_types_and_distinguish_members() {
        let source = "class Box {\n    int value\n    func get_value() returns int { return self.value }\n}\nfunc inspect(list<int> items, map<string, int> index, set<int> seen, Box box) returns void {\n    auto current = box.get_value()\n    box.value = current\n    print(items.to_string())\n    return\n}\n";
        let path = std::path::PathBuf::from("/tmp/mux-member-semantic-tokens.mux");
        let uri = lsp::Uri::from_str("file:///tmp/mux-member-semantic-tokens.mux").unwrap();
        let document = OpenDocument {
            uri: uri.clone(),
            path,
            source: source.to_owned(),
            version: 1,
            revision: 1,
        };
        let documents = HashMap::from([(uri.as_str().to_owned(), document.clone())]);
        let tokens = semantic_tokens(&document, &documents);

        let mut line = 0;
        let mut character = 0;
        let mut decoded = Vec::new();
        for token in tokens.data {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            decoded.push((line, character, token.token_type));
        }
        let token_type_at = |offset: usize| {
            let position = offset_to_position(source, offset);
            decoded
                .iter()
                .find(|(line, start, _)| *line == position.line && *start == position.character)
                .map(|(_, _, token_type)| *token_type)
                .expect("semantic token at source identifier")
        };

        for builtin in ["list", "map", "set"] {
            assert_eq!(
                token_type_at(source.find(&format!("{builtin}<")).unwrap()),
                1,
                "{builtin} should be colored as a type"
            );
        }
        assert_eq!(token_type_at(source.rfind("get_value").unwrap()), 6);
        assert_eq!(
            token_type_at(source.find("box.value").unwrap() + "box.".len()),
            9
        );
        assert_eq!(token_type_at(source.rfind("to_string").unwrap()), 6);
        assert_eq!(token_type_at(source.rfind("current").unwrap()), 8);
    }

    #[test]
    fn semantic_tokens_overlay_resolved_symbols_on_lexical_tokens() {
        let source = "func greet(string name) returns string { return name }\nfunc main() returns void {\n    const int LIMIT = 10\n    auto result = greet(\"Mux\")\n    print(LIMIT)\n}\n";
        let path = std::path::PathBuf::from("/tmp/mux-semantic-tokens.mux");
        let uri = lsp::Uri::from_str("file:///tmp/mux-semantic-tokens.mux").unwrap();
        let document = OpenDocument {
            uri: uri.clone(),
            path,
            source: source.to_owned(),
            version: 1,
            revision: 1,
        };
        let documents = HashMap::from([(uri.as_str().to_owned(), document.clone())]);
        let tokens = semantic_tokens(&document, &documents);
        assert!(!tokens.data.is_empty());

        let mut line = 0;
        let mut character = 0;
        let mut decoded = Vec::new();
        for token in tokens.data {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            decoded.push((
                line,
                character,
                token.length,
                token.token_type,
                token.token_modifiers_bitset,
            ));
        }
        let source_token = |needle: &str| {
            let offset = source.find(needle).unwrap();
            let position = offset_to_position(source, offset);
            decoded
                .iter()
                .find(|(line, start, _, _, _)| {
                    *line == position.line && *start == position.character
                })
                .copied()
                .unwrap()
        };
        assert_eq!(
            source_token("greet").3,
            5,
            "function declaration should have function coloring"
        );
        assert_eq!(
            source_token("name").3,
            7,
            "parameter should have parameter coloring"
        );
        let name_use = offset_to_position(source, source.rfind("name").unwrap());
        let name_use_token = decoded
            .iter()
            .find(|(line, start, _, _, _)| *line == name_use.line && *start == name_use.character)
            .unwrap();
        assert_eq!(
            name_use_token.3, 7,
            "parameter references keep parameter coloring"
        );
        let result = source.rfind("result").unwrap();
        let result_position = offset_to_position(source, result);
        let result_token = decoded
            .iter()
            .find(|(line, start, _, _, _)| {
                *line == result_position.line && *start == result_position.character
            })
            .unwrap();
        assert_eq!(result_token.3, 8);
        assert_eq!(result_token.4 & 1, 1, "declaration modifier should be set");
        let limit_use = offset_to_position(source, source.rfind("LIMIT").unwrap());
        let limit_token = decoded
            .iter()
            .find(|(line, start, _, _, _)| *line == limit_use.line && *start == limit_use.character)
            .unwrap();
        assert_eq!(limit_token.3, 8);
        assert_eq!(
            limit_token.4 & 2,
            2,
            "constant modifier should be set on uses"
        );
        assert_eq!(
            source_token("\"Mux\"").3,
            13,
            "string literal should be colored"
        );
    }

    #[test]
    fn completion_provides_signature_and_safe_function_snippets() {
        let source = "func greet(string name) returns string { return name }\nfunc main() returns void {\n    greet(\"Mux\")\n    gr\n}\n";
        let cursor = source.rfind("gr").unwrap() + 2;
        let items = complete(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion-signature.mux"),
            &HashMap::new(),
        );
        let greet = items.iter().find(|item| item.label == "greet").unwrap();
        assert!(
            greet
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("func(name: string) -> string")),
            "completion detail: {:?}",
            greet.detail
        );
        assert_eq!(greet.insert_text.as_deref(), Some("greet($0)"));

        let items = complete_with_snippet_support(
            source,
            offset_to_position(source, cursor),
            std::path::Path::new("/tmp/mux-completion-signature.mux"),
            &HashMap::new(),
            false,
        );
        let greet = items.iter().find(|item| item.label == "greet").unwrap();
        assert_eq!(greet.insert_text.as_deref(), Some("greet"));
        assert_eq!(
            greet.insert_text_format,
            Some(lsp::InsertTextFormat::PLAIN_TEXT)
        );

        let source_with_call = source.replace("    gr\n", "    gr(\n");
        let cursor = source_with_call.rfind("gr(").unwrap() + 2;
        let items = complete(
            &source_with_call,
            offset_to_position(&source_with_call, cursor),
            std::path::Path::new("/tmp/mux-completion-signature.mux"),
            &HashMap::new(),
        );
        let greet = items.iter().find(|item| item.label == "greet").unwrap();
        assert_eq!(greet.insert_text.as_deref(), Some("greet"));
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
        let source = "class User {\n    string name = \"\"\n    func greet() returns string { return self.name }\n}\nfunc unrelated() returns void { return }\nfunc main() returns void {\n    auto user = User.new()\n    user.na\n}\n";
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
        assert!(!labels.contains("unrelated"), "{labels:?}");
        assert!(!labels.contains("while"), "{labels:?}");
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
