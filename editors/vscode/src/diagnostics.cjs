const path = require('node:path');
const { spawn } = require('node:child_process');

function frame(message) {
  const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...message }));
  return Buffer.concat([Buffer.from('Content-Length: ' + body.length + '\r\n\r\n'), body]);
}

function readMessages(stream, receive, fail) {
  let buffer = Buffer.alloc(0);
  let length;
  stream.on('data', data => {
    buffer = Buffer.concat([buffer, data]);
    try {
      while (buffer.length) {
        if (length === undefined) {
          const end = buffer.indexOf('\r\n\r\n');
          if (end === -1 && buffer.length <= 8192) return;
          const header = end < 0 || end > 8192 ? '' : buffer.toString('ascii', 0, end);
          const match = header.match(/^Content-Length: (\d+)\r?$/im);
          length = match && Number(match[1]);
          if (!length || length > 16 * 1024 * 1024) throw new Error('Invalid Joky LSP message header');
          buffer = buffer.subarray(end + 4);
        }
        if (buffer.length < length) return;
        const message = JSON.parse(buffer.toString('utf8', 0, length));
        buffer = buffer.subarray(length);
        length = undefined;
        receive(message);
      }
    } catch (error) {
      fail(error);
    }
  });
}

function registerDiagnostics(context, vscode, compiler, output, launch = spawn) {
  const collection = vscode.languages.createDiagnosticCollection('joky');
  let session;
  let disposed = false;
  let unavailable = false;

  function diagnosticsEnabled(uri) {
    return vscode.workspace.getConfiguration('joky', uri).get('checkOnSave', true);
  }

  function enabled(document) {
    return document.uri.scheme === 'file' && document.languageId === 'joky' &&
      path.extname(document.uri.fsPath) === '.jk';
  }

  function currentDocuments() {
    return vscode.workspace.textDocuments.filter(enabled);
  }

  function stop() {
    const previous = session;
    session = undefined;
    previous?.readySignal();
    if (previous) clearRequests(previous);
    if (previous?.child) {
      if (previous.ready && previous.child.stdin.writable) {
        previous.child.stdin.write(frame({ id: 2, method: 'shutdown', params: null }));
        const timeout = setTimeout(() => previous.child.kill(), 1000);
        timeout.unref();
        previous.child.once('exit', () => clearTimeout(timeout));
      } else {
        previous.child.stdin.destroy();
        previous.child.kill();
      }
    }
    collection.clear();
  }

  function send(state, message) {
    if (state === session && state.child?.stdin.writable) state.child.stdin.write(frame(message));
  }

  function notify(state, method, params) {
    send(state, { method, params });
  }

  function synchronize(state, method, params) {
    state.revision++;
    notify(state, method, { ...params, jokySnapshotRevision: state.revision });
    if (method === 'workspace/didChangeWatchedFiles') state.watchedRevision = state.revision;
    else clearRequests(state);
  }

  function clearDiagnosticsForChange(uri) {
    if (session?.capabilities?.experimental?.jokyScopedDiagnostics) {
      if (uri) collection.delete(uri);
    } else {
      collection.clear();
    }
  }

  function range({ start, end }) {
    return new vscode.Range(start.line, start.character, end.line, end.character);
  }

  function clearRequests(state) {
    for (const [id, complete] of state.requests) {
      notify(state, '$/cancelRequest', { id });
      complete();
    }
  }

  async function request(method, capability, document, position, token, retries = 1) {
    if (!enabled(document) || token.isCancellationRequested) return;
    const version = document.version;
    void start(document);
    const state = session;
    if (!state) return;
    await state.initialized;
    if (state !== session || !state.capabilities?.[capability] ||
        token.isCancellationRequested || document.version !== version ||
        !state.open.has(document.uri.toString())) return;
    const revision = state.revision;
    const result = await new Promise(resolve => {
      const id = state.nextId++;
      const complete = result => {
        if (!state.requests.delete(id)) return;
        cancellation.dispose();
        resolve(result);
      };
      const cancellation = token.onCancellationRequested(() => {
        notify(state, '$/cancelRequest', { id });
        complete();
      });
      state.requests.set(id, complete);
      send(state, { id, method, params: {
        textDocument: { uri: document.uri.toString() },
        position: { line: position.line, character: position.character },
      } });
    });
    if (state !== session || document.version !== version || token.isCancellationRequested) return;
    if (state.revision === revision) return result;
    if (retries && state.watchedRevision === state.revision) {
      return request(method, capability, document, position, token, retries - 1);
    }
  }

  function publish(state, params) {
    if (params.jokySnapshotRevision !== state.revision) return;
    const documents = currentDocuments();
    const versions = params.jokySnapshotVersions;
    if (!versions || Object.keys(versions).length !== documents.length ||
        documents.some(doc => versions[doc.uri.toString()] !== doc.version)) return;
    const uri = vscode.Uri.parse(params.uri);
    if (!diagnosticsEnabled(uri)) return;
    const values = params.diagnostics.map(item => {
      const diagnostic = new vscode.Diagnostic(range(item.range), item.message, vscode.DiagnosticSeverity.Error);
      diagnostic.code = item.code;
      diagnostic.source = 'Joky';
      diagnostic.relatedInformation = (item.relatedInformation || []).map(related =>
        new vscode.DiagnosticRelatedInformation(new vscode.Location(vscode.Uri.parse(related.location.uri),
          range(related.location.range)), related.message));
      return diagnostic;
    });
    if (state === session) collection.set(uri, values);
  }

  function open(state, document) {
    const uri = document.uri.toString();
    if (!enabled(document) || state.open.has(uri)) return;
    state.open.add(uri);
    synchronize(state, 'textDocument/didOpen', { textDocument: {
      uri, languageId: 'joky', version: document.version, text: document.getText(),
    } });
  }

  async function start(document) {
    if (disposed || unavailable || !vscode.workspace.isTrusted || session || !enabled(document)) return;
    const state = { open: new Set(), ready: false, revision: 0, nextId: 3, requests: new Map() };
    state.initialized = new Promise(resolve => { state.readySignal = resolve; });
    session = state;
    const fail = error => {
      if (state !== session) return;
      unavailable = true;
      output.appendLine('Joky language server: ' + error.message);
      stop();
    };
    try {
      const executable = await compiler(document.uri);
      if (state !== session) return;
      if (!executable) { stop(); return; }
      const child = launch(executable, ['lsp'], { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
      state.child = child;
      child.on('error', fail);
      child.stdin.on('error', fail);
      child.stderr.on('data', data => { if (state === session) output.appendLine(data.toString()); });
      child.on('exit', code => fail(new Error('Exited with code ' + code)));
      readMessages(child.stdout, message => {
        if (message.id === 2 && state !== session && child.stdin.writable) {
          child.stdin.end(frame({ method: 'exit' }));
          return;
        }
        if (state !== session) return;
        if (message.id === 1) {
          if (message.result?.capabilities?.textDocumentSync?.change !== 1) {
            throw new Error('The selected compiler does not support Joky LSP diagnostics');
          }
          state.capabilities = message.result.capabilities;
          state.ready = true;
          notify(state, 'initialized', {});
          currentDocuments().forEach(doc => open(state, doc));
          state.readySignal();
        } else if (state.requests.has(message.id)) {
          state.requests.get(message.id)(message.result);
        } else if (message.method === 'textDocument/publishDiagnostics') {
          publish(state, message.params);
        } else if (message.method === 'window/logMessage') {
          output.appendLine(message.params.message);
        }
      }, fail);
      send(state, { id: 1, method: 'initialize', params: {
        processId: process.pid,
        capabilities: { textDocument: { publishDiagnostics: { relatedInformation: true, versionSupport: true } } },
        initializationOptions: { jokySnapshotVersions: true },
        workspaceFolders: (vscode.workspace.workspaceFolders || []).filter(folder => folder.uri.scheme === 'file')
          .map(folder => ({ uri: folder.uri.toString(), name: folder.name })),
      } });
    } catch (error) { fail(error); }
  }

  function restart() {
    stop();
    unavailable = false;
    const [document] = currentDocuments();
    if (document) void start(document);
  }

  context.subscriptions.push(
    { dispose() { disposed = true; stop(); collection.dispose(); } },
    vscode.languages.registerHoverProvider({ scheme: 'file', language: 'joky' }, {
      async provideHover(document, position, token) {
        const result = await request('textDocument/hover', 'hoverProvider', document, position, token);
        if (!result || token.isCancellationRequested) return;
        const contents = new vscode.MarkdownString();
        contents.appendCodeblock(result.contents.value, 'joky');
        return new vscode.Hover(contents, range(result.range));
      },
    }),
    vscode.languages.registerDefinitionProvider({ scheme: 'file', language: 'joky' }, {
      async provideDefinition(document, position, token) {
        const result = await request('textDocument/definition', 'definitionProvider', document, position, token);
        if (result && !token.isCancellationRequested) {
          return new vscode.Location(vscode.Uri.parse(result.uri), range(result.range));
        }
      },
    }),
    vscode.languages.registerCompletionItemProvider({ scheme: 'file', language: 'joky' }, {
      async provideCompletionItems(document, position, token) {
        const result = await request('textDocument/completion', 'completionProvider', document, position, token);
        if (!result || token.isCancellationRequested) return;
        const items = result.items.map(item => {
          const value = new vscode.CompletionItem(item.label, item.kind - 1);
          value.detail = item.detail;
          value.range = range(item.textEdit.range);
          value.insertText = item.textEdit.newText;
          return value;
        });
        return new vscode.CompletionList(items, result.isIncomplete);
      },
    }, '.'),
    vscode.workspace.onDidOpenTextDocument(document => {
      if (!enabled(document)) return;
      clearDiagnosticsForChange(document.uri);
      if (session?.ready) open(session, document);
      else void start(document);
    }),
    vscode.workspace.onDidChangeTextDocument(({ document, contentChanges }) => {
      if (!contentChanges.length || !enabled(document)) return;
      clearDiagnosticsForChange(document.uri);
      if (!session?.ready) { void start(document); return; }
      const uri = document.uri.toString();
      if (!session.open.has(uri)) { open(session, document); return; }
      synchronize(session, 'textDocument/didChange', {
        textDocument: { uri, version: document.version }, contentChanges: [{ text: document.getText() }],
      });
    }),
    vscode.workspace.onDidSaveTextDocument(document => {
      if (document.uri.scheme !== 'file') return;
      if (enabled(document)) {
        unavailable = false;
        if (session?.ready) open(session, document);
        else void start(document);
      }
      if (session?.ready && (path.extname(document.uri.fsPath) === '.jk' || path.basename(document.uri.fsPath) === 'joky.toml')) {
        clearDiagnosticsForChange(document.uri);
        synchronize(session, 'textDocument/didSave', { textDocument: { uri: document.uri.toString() } });
      }
    }),
    vscode.workspace.onDidCloseTextDocument(document => {
      const uri = document.uri.toString();
      if (!session?.open.delete(uri)) return;
      clearDiagnosticsForChange(document.uri);
      synchronize(session, 'textDocument/didClose', { textDocument: { uri } });
    }),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('joky.checkOnSave') || event.affectsConfiguration('joky.executablePath')) restart();
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(restart),
    vscode.workspace.onDidGrantWorkspaceTrust(restart),
  );
  const watcher = vscode.workspace.createFileSystemWatcher('**/{*.jk,joky.toml}');
  context.subscriptions.push(watcher);
  for (const [event, type] of [['onDidCreate', 1], ['onDidChange', 2], ['onDidDelete', 3]]) {
    context.subscriptions.push(watcher[event](uri => {
      if (!session?.ready || uri.scheme !== 'file') return;
      // Open buffers supply their own text. Creation and deletion can change resolution.
      if (type === 2 && session.open.has(uri.toString())) return;
      clearDiagnosticsForChange(uri);
      synchronize(session, 'workspace/didChangeWatchedFiles', { changes: [{ uri: uri.toString(), type }] });
    }));
  }
  const [document] = currentDocuments();
  if (document) void start(document);
}

module.exports = { registerDiagnostics, frame, readMessages };
