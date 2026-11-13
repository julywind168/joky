const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const { PassThrough } = require('node:stream');
const { test } = require('node:test');
const { frame, readMessages, registerDiagnostics } = require('../src/diagnostics.cjs');

test('LSP framing handles split headers, Unicode byte lengths and coalesced messages', () => {
  const stream = new PassThrough();
  const received = [];
  readMessages(stream, message => received.push(message), error => { throw error; });
  const first = frame({ method: 'first', params: { text: '中文🙂' } });
  stream.write(first.subarray(0, 9));
  assert.equal(received.length, 0);
  stream.write(first.subarray(9, first.length - 2));
  assert.equal(received.length, 0);
  stream.write(Buffer.concat([first.subarray(first.length - 2), frame({ method: 'second' })]));
  assert.deepEqual(received, [
    { jsonrpc: '2.0', method: 'first', params: { text: '中文🙂' } },
    { jsonrpc: '2.0', method: 'second' },
  ]);
});

function harness(t, trusted = true, diagnostics = true) {
  const events = new Map();
  const documents = [];
  const calls = [];
  const problems = new Map();
  const subscriptions = [];
  const child = new EventEmitter();
  child.stdin = new PassThrough();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  child.kill = () => {};
  readMessages(child.stdin, message => calls.push(message), error => { throw error; });
  const disposable = { dispose() {} };
  const on = name => callback => { events.set(name, callback); return disposable; };
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const vscode = {
    Uri: { parse: uri },
    Range: class { constructor(...positions) { this.positions = positions; } },
    Hover: class { constructor(contents, range) { Object.assign(this, { contents, range }); } },
    CompletionItem: class { constructor(label, kind) { Object.assign(this, { label, kind }); } },
    CompletionList: class { constructor(items, isIncomplete) { Object.assign(this, { items, isIncomplete }); } },
    MarkdownString: class { appendCodeblock(value, language) { Object.assign(this, { value, language }); } },
    Diagnostic: class { constructor(range, message) { Object.assign(this, { range, message }); } },
    Location: class { constructor(uri, range) { Object.assign(this, { uri, range }); } },
    DiagnosticRelatedInformation: class { constructor(location, message) { Object.assign(this, { location, message }); } },
    DiagnosticSeverity: { Error: 0 },
    languages: {
      registerHoverProvider(_selector, provider) { return on('hover')(provider.provideHover); },
      registerDefinitionProvider(_selector, provider) { return on('definition')(provider.provideDefinition); },
      registerCompletionItemProvider(_selector, provider, ...triggers) {
        assert.deepEqual(triggers, ['.']);
        return on('completion')(provider.provideCompletionItems);
      },
      createDiagnosticCollection: () => ({
        set(uri, diagnostics) { problems.set(uri.toString(), diagnostics); },
        clear() { problems.clear(); },
        dispose() {},
      }),
    },
    workspace: {
      isTrusted: trusted, textDocuments: documents,
      getConfiguration: () => ({ get: () => diagnostics }),
      onDidOpenTextDocument: on('open'), onDidChangeTextDocument: on('change'),
      onDidSaveTextDocument: on('save'), onDidCloseTextDocument: on('close'),
      onDidChangeConfiguration: on('configuration'), onDidChangeWorkspaceFolders: on('folders'),
      onDidGrantWorkspaceTrust: on('trust'),
    },
  };
  registerDiagnostics({ subscriptions }, vscode, async () => '/compiler/joky', {
    appendLine(message) { assert.fail(message); },
  }, (_executable, args) => { assert.deepEqual(args, ['lsp']); return child; });
  t.after(() => subscriptions.forEach(value => value.dispose()));
  return {
    calls, problems, documents,
    async open(name) {
      const document = { uri: uri('file:///' + name), languageId: 'joky', version: 1, getText: () => 'fn main() {}' };
      documents.push(document);
      events.get('open')(document);
      await Promise.resolve();
      return document;
    },
    edit(document, text) {
      document.version++;
      document.getText = () => text;
      events.get('change')({ document, contentChanges: [{}] });
    },
    save(document) { events.get('save')(document); },
    request(feature, document, token) {
      return events.get(feature)(document, { line: 0, character: 3 }, token);
    },
    receive(message) { child.stdout.write(frame(message)); },
  };
}

test('the editor accepts only diagnostics for the latest synchronized snapshot', async t => {
  const h = harness(t);
  const main = await h.open('main.jk');
  assert.equal(h.calls[0].method, 'initialize');
  h.receive({ id: 1, result: { capabilities: { textDocumentSync: { change: 1 } } } });
  assert.equal(h.calls.at(-1).method, 'textDocument/didOpen');
  const dep = await h.open('dep.jk');
  const uri = main.uri.toString();
  const depUri = dep.uri.toString();
  const range = { start: { line: 0, character: 2 }, end: { line: 0, character: 3 } };
  const diagnostic = { code: 'syntax_error', message: 'Bad token', range, relatedInformation: [
    { location: { uri: depUri, range }, message: 'Imported here' },
  ] };
  const publish = (revision, version) => h.receive({ method: 'textDocument/publishDiagnostics', params: {
    uri, diagnostics: [diagnostic], jokySnapshotRevision: revision,
    jokySnapshotVersions: { [uri]: 1, [depUri]: version },
  } });
  publish(2, 1);
  assert.equal(h.problems.get(uri)[0].relatedInformation[0].location.uri.toString(), depUri);
  h.edit(dep, '🙂$');
  assert.deepEqual(h.calls.at(-1).params.contentChanges, [{ text: '🙂$' }]);
  assert.equal(h.problems.size, 0);
  publish(2, 1);
  publish(3, 1);
  assert.equal(h.problems.size, 0);
  publish(3, 2);
  assert.equal(h.problems.get(uri)[0].code, 'syntax_error');
  h.save(dep);
  publish(3, 2);
  assert.equal(h.problems.size, 0);
  publish(4, 2);
  assert.equal(h.problems.size, 1);
});

test('navigation uses the latest synchronized snapshot with diagnostics disabled', async t => {
  const h = harness(t, true, false);
  const main = await h.open('main.jk');
  const dep = await h.open('dep.jk');
  const token = { isCancellationRequested: false, onCancellationRequested: () => ({ dispose() {} }) };
  const pending = h.request('hover', main, token);
  h.receive({ id: 1, result: { capabilities: {
    textDocumentSync: { change: 1 }, hoverProvider: true, definitionProvider: true, completionProvider: {},
  } } });
  await Promise.resolve();
  const hover = h.calls.at(-1);
  assert.equal(hover.method, 'textDocument/hover');
  assert.deepEqual(hover.params, { textDocument: { uri: main.uri.toString() }, position: { line: 0, character: 3 } });
  const range = { start: { line: 0, character: 3 }, end: { line: 0, character: 7 } };
  h.edit(dep, '\nfn example() {}');
  assert.equal(h.calls.at(-1).method, '$/cancelRequest');
  h.receive({ id: hover.id, result: { contents: { value: 'old: Int32' }, range } });
  assert.equal(await pending, undefined);

  const definition = h.request('definition', main, token);
  await Promise.resolve();
  assert.equal(h.calls.at(-1).method, 'textDocument/definition');
  h.receive({ id: h.calls.at(-1).id, result: { uri: dep.uri.toString(), range } });
  assert.equal((await definition).uri.toString(), dep.uri.toString());
  const current = h.request('hover', main, token);
  await Promise.resolve();
  h.receive({ id: h.calls.at(-1).id, result: { contents: { value: 'name: Bool' }, range } });
  assert.equal((await current).contents.value, 'name: Bool');
  const completion = h.request('completion', main, token);
  await Promise.resolve();
  assert.equal(h.calls.at(-1).method, 'textDocument/completion');
  h.receive({ id: h.calls.at(-1).id, result: { isIncomplete: true, items: [{
    label: 'name', kind: 6, detail: 'Bool', textEdit: { range, newText: 'name' },
  }] } });
  const suggestions = await completion;
  assert.equal(suggestions.isIncomplete, true);
  assert.equal(suggestions.items[0].kind, 5);
  assert.equal(suggestions.items[0].detail, 'Bool');
  assert.equal(suggestions.items[0].insertText, 'name');
  assert.deepEqual(suggestions.items[0].range.positions, [0, 3, 0, 7]);
  h.receive({ method: 'textDocument/publishDiagnostics', params: {
    uri: dep.uri.toString(), diagnostics: [{ code: 'syntax_error', message: 'Bad token', range }],
    jokySnapshotRevision: 3, jokySnapshotVersions: { [main.uri.toString()]: 1, [dep.uri.toString()]: 2 },
  } });
  assert.equal(h.problems.size, 0);
});

test('an untrusted workspace does not start the language server', async t => {
  const h = harness(t, false);
  const doc = await h.open('main.jk');
  h.edit(doc, '$');
  h.save(doc);
  await Promise.resolve();
  assert.deepEqual(h.calls, []);
});
