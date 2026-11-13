const fs = require('node:fs/promises');
const path = require('node:path');

function diskPath(file) {
  if (process.platform !== 'win32') return file;
  if (file.startsWith('\\\\?\\UNC\\')) return `\\\\${file.slice(8)}`;
  return file.startsWith('\\\\?\\') ? file.slice(4) : file;
}

// The compiler counts raw UTF-8 bytes. VS Code counts UTF-16 units and hides a leading BOM.
function position(bytes, offset) {
  if (!Number.isSafeInteger(offset) || offset < 0 || offset > bytes.length || (bytes[offset] & 0xc0) === 0x80) return;
  const lines = bytes.toString('utf8', 0, offset).replace(/^\uFEFF/, '').split(/\r\n|\n|\r/);
  return [lines.length - 1, lines.at(-1).length];
}

function isSource(document) {
  return document.uri.scheme === 'file' && (document.languageId === 'joky' || path.basename(document.uri.fsPath) === 'joky.toml');
}

function registerDiagnostics(context, vscode, toolchain, compiler, output) {
  const collection = vscode.languages.createDiagnosticCollection('joky');
  const states = new Map();
  const latest = new Map();
  const pending = new Set();
  let lastFailure;
  let visible = new Map();
  let sequence = 0;
  let disposed = false;

  function publish() {
    const next = new Map();
    for (const state of states.values()) {
      for (const [uri, diagnostic] of state.entries) {
        const key = uri.toString();
        const previous = next.get(key);
        if (!previous || previous.sequence < state.sequence) next.set(key, { uri, diagnostic, sequence: state.sequence });
      }
    }
    const changes = [];
    for (const [key, { uri }] of visible) if (!next.has(key)) changes.push([uri, undefined]);
    for (const [key, entry] of next) {
      if (visible.get(key)?.diagnostic !== entry.diagnostic) changes.push([entry.uri, [entry.diagnostic]]);
    }
    if (changes.length) collection.set(changes);
    visible = next;
  }

  function invalidate(file, before = Infinity) {
    for (const [key, state] of states) {
      if (state.sequence <= before && (toolchain.contains(state.cwd, file) ||
          toolchain.contains(state.canonicalRoot, file) || state.files.has(file))) {
        state.controller.abort();
        states.delete(key);
      }
    }
    publish();
  }

  function reset() {
    for (const request of pending) request.controller.abort();
    for (const state of states.values()) state.controller.abort();
    states.clear();
    latest.clear();
    lastFailure = undefined;
    visible.clear();
    collection.clear();
  }

  async function check(document) {
    if (disposed || !isSource(document) || !vscode.workspace.isTrusted ||
        !vscode.workspace.getConfiguration('joky', document.uri).get('checkOnSave', true)) return;
    const file = document.uri.fsPath;
    const version = document.version;
    const id = ++sequence;
    const controller = new AbortController();
    const request = { id, controller, changed: new Set() };
    const current = () => !controller.signal.aborted && !document.isClosed && !document.isDirty &&
      document.version === version;
    let key;
    let state;
    pending.add(request);
    try {
      const folder = vscode.workspace.getWorkspaceFolder(document.uri);
      const target = await toolchain.resolveTarget(file, folder?.uri.fsPath);
      key = diskPath(await fs.realpath(target.target));
      const canonicalRoot = diskPath(await fs.realpath(target.cwd));
      if (!current() || latest.get(key) > id ||
          [...request.changed].some(file => toolchain.contains(target.cwd, file) || toolchain.contains(canonicalRoot, file))) return;
      latest.set(key, id);
      states.get(key)?.controller.abort();
      state = { cwd: target.cwd, canonicalRoot, sequence: id, controller, entries: [], files: new Set([file]) };
      states.set(key, state);
      publish();
      const executable = await compiler(document.uri);
      if (!executable || !current()) return;
      const stdout = await toolchain.execute(executable, ['check', '--diagnostic-format', 'json', '--', target.target], {
        cwd: target.cwd, signal: controller.signal, acceptDiagnosticFailure: true,
      });
      if (!current()) return;
      const result = JSON.parse(stdout);
      if (result?.schemaVersion !== 1 || !Array.isArray(result.diagnostics) || result.diagnostics.length > 1) {
        throw new Error('Unsupported Joky JSON diagnostic format.');
      }
      const item = result.diagnostics[0];
      if (!result.diagnostics.length) return;
      if (typeof item?.code !== 'string' || item.severity !== 'error' || typeof item.message !== 'string' ||
          !Array.isArray(item.labels) || !Array.isArray(item.help)) {
        throw new Error('Invalid Joky JSON diagnostic.');
      }

      const open = new Map(await Promise.all(vscode.workspace.textDocuments.filter(doc => doc.uri.scheme === 'file').map(async doc => {
        const canonical = await fs.realpath(doc.uri.fsPath).catch(() => doc.uri.fsPath);
        return [diskPath(canonical), doc];
      })));
      const buffers = new Map();
      const displayPath = file => {
        // Prefer the editor's spelling of a workspace path, including symlink aliases.
        if (toolchain.contains(canonicalRoot, file)) return path.join(target.cwd, path.relative(canonicalRoot, file));
        return file;
      };
      async function location(value) {
        if (value?.source?.kind !== 'file' || typeof value.source.path !== 'string' || !path.isAbsolute(value.source.path)) return;
        const file = diskPath(value.source.path);
        const doc = open.get(file);
        state.files.add(file);
        if (doc) state.files.add(doc.uri.fsPath);
        if (!buffers.has(file)) buffers.set(file, fs.readFile(file).catch(() => undefined));
        const bytes = await buffers.get(file);
        if (!bytes || (doc && (doc.isDirty || doc.getText() !== bytes.toString('utf8').replace(/^\uFEFF/, ''))) ||
            request.changed.has(file) || request.changed.has(doc?.uri.fsPath)) return;
        const start = position(bytes, value.startByte);
        const end = position(bytes, value.endByte);
        if (!start || !end || value.endByte < value.startByte) return;
        const display = doc?.uri.fsPath || displayPath(file);
        state.files.add(display);
        return new vscode.Location(doc?.uri || vscode.Uri.file(display), new vscode.Range(...start, ...end));
      }
      const primary = await location(item.primary);
      if (!primary) {
        if (current()) output.appendLine(`${target.target}: [${item.code}] ${item.message}`);
        return;
      }
      const message = [item.message, ...item.help].join('\n');
      const diagnostic = new vscode.Diagnostic(primary.range, message, vscode.DiagnosticSeverity.Error);
      diagnostic.code = item.code;
      diagnostic.source = 'Joky';
      const related = await Promise.all(item.labels.map(async label => {
        const place = await location(label.location);
        if (place) return new vscode.DiagnosticRelatedInformation(place, label.message);
      }));
      diagnostic.relatedInformation = related.filter(Boolean);
      if (!current() || [...state.files].some(file => request.changed.has(file))) return;
      state.entries = [[primary.uri, diagnostic]];
      publish();
    } catch (error) {
      if (current() && lastFailure !== error.message) {
        lastFailure = error.message;
        output.appendLine(`Check on save: ${error.message}`);
      }
    } finally {
      pending.delete(request);
      if (state && !state.entries.length && states.get(key) === state) states.delete(key);
      // A completed success still supersedes older saves resolving the same target.
      const oldest = pending.values().next().value?.id ?? Infinity;
      for (const [target, newest] of latest) if (newest < oldest) latest.delete(target);
    }
  }

  context.subscriptions.push(
    { dispose() { disposed = true; reset(); collection.dispose(); } },
    vscode.workspace.onDidSaveTextDocument(check),
    vscode.workspace.onDidChangeTextDocument(({ document, contentChanges }) => {
      if (disposed || document.uri.scheme !== 'file' || !contentChanges.length) return;
      const file = document.uri.fsPath;
      if (!isSource(document) && ![...states.values()].some(state => state.files.has(file))) return;
      const before = sequence;
      const requests = [...pending];
      for (const request of requests) request.changed.add(file);
      invalidate(file);
      fs.realpath(file).then(canonical => {
        if (disposed) return;
        canonical = diskPath(canonical);
        for (const request of requests) request.changed.add(canonical);
        if (canonical !== file) invalidate(canonical, before);
      }, () => {});
    }),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('joky.checkOnSave') || event.affectsConfiguration('joky.executablePath')) reset();
    }),
  );
}

module.exports = { registerDiagnostics };
