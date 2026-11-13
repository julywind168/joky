const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { registerDiagnostics } = require('../src/diagnostics.cjs');
const toolchain = require('../src/toolchain.cjs');

async function harness(t) {
  const scratch = await fs.mkdtemp(path.join(os.tmpdir(), 'joky-diagnostics-'));
  const root = await fs.realpath(scratch);
  t.after(() => fs.rm(scratch, { recursive: true, force: true }));
  const subscriptions = [];
  const problems = new Map();
  const documents = [];
  const calls = [];
  const log = [];
  let saved;
  let changed;
  let configured;
  let launch;
  let resolveTarget = toolchain.resolveTarget;
  const uri = file => ({ scheme: 'file', fsPath: file, toString: () => file });
  const vscode = {
    Uri: { file: uri },
    Range: class { constructor(...positions) { this.positions = positions; } },
    Location: class { constructor(uri, range) { Object.assign(this, { uri, range }); } },
    Diagnostic: class { constructor(range, message, severity) { Object.assign(this, { range, message, severity }); } },
    DiagnosticRelatedInformation: class { constructor(location, message) { Object.assign(this, { location, message }); } },
    DiagnosticSeverity: { Error: 0 },
    languages: { createDiagnosticCollection: () => ({
      set(entries) { for (const [uri, values] of entries) values ? problems.set(uri.fsPath, values[0]) : problems.delete(uri.fsPath); },
      clear() { problems.clear(); },
      dispose() {},
    }) },
    workspace: {
      isTrusted: true, textDocuments: documents,
      getWorkspaceFolder: () => ({ uri: uri(root) }),
      getConfiguration: () => ({ get: () => true }),
      onDidSaveTextDocument(callback) { saved = callback; },
      onDidChangeTextDocument(callback) { changed = callback; },
      onDidChangeConfiguration(callback) { configured = callback; },
    },
  };
  registerDiagnostics({ subscriptions }, vscode, {
    ...toolchain,
    resolveTarget: (...args) => resolveTarget(...args),
    execute: (_executable, args, options) => new Promise(resolve => {
      calls.push({ args, options, resolve });
      launch?.();
    }),
  }, async () => '/compiler/joky', { appendLine: text => log.push(text) });
  t.after(() => subscriptions[0].dispose());

  async function document(name, text) {
    const file = path.join(root, name);
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, text);
    const result = {
      uri: uri(file), version: 1, isDirty: false, isClosed: false, languageId: 'joky',
      getText: () => text.replace(/^\uFEFF/, ''),
    };
    documents.push(result);
    return result;
  }
  async function start(doc) {
    const started = new Promise(resolve => { launch = resolve; });
    const finished = saved(doc);
    await started;
    return { ...calls.at(-1), finished };
  }
  function edit(doc) {
    doc.version++;
    doc.isDirty = true;
    changed({ document: doc, contentChanges: [{}] });
  }
  return {
    root, document, start, edit, problems, log, configured, save: doc => saved(doc), calls, workspace: vscode.workspace,
    setResolver: resolve => { resolveTarget = resolve; },
  };
}

function result(...diagnostics) {
  return JSON.stringify({ schemaVersion: 1, diagnostics });
}

function at(document, prefix, text) {
  const startByte = Buffer.byteLength(prefix);
  return { source: { kind: 'file', path: document.uri.fsPath }, startByte, endByte: startByte + Buffer.byteLength(text) };
}

function error(primary, labels = []) {
  return { code: 'syntax_error', severity: 'error', phase: 'parse', message: 'Invalid syntax', primary, labels, help: ['Fix the expression.'] };
}

test('save checks the project and maps UTF-8 ranges and related files into Problems', async t => {
  const h = await harness(t);
  await fs.writeFile(path.join(h.root, 'joky.toml'), '');
  const main = await h.document('main.jk', 'import dep');
  const prefix = '\uFEFF// heading\r\n🙂';
  const dep = await h.document('dep.jk', `${prefix}$`);
  const check = await h.start(main);
  assert.deepEqual(check.args, ['check', '--diagnostic-format', 'json', '--', path.join(h.root, 'joky.toml')]);
  assert.equal(check.options.acceptDiagnosticFailure, true);
  check.resolve(result(error(at(dep, prefix, '$'), [{ location: at(main, '', 'import'), message: 'Imported here' }])));
  await check.finished;
  const diagnostic = h.problems.get(dep.uri.fsPath);
  assert.deepEqual(diagnostic.range.positions, [1, 2, 1, 3]);
  assert.equal(diagnostic.code, 'syntax_error');
  assert.equal(diagnostic.message, 'Invalid syntax\nFix the expression.');
  assert.equal(diagnostic.relatedInformation[0].location.uri, main.uri);
  assert.deepEqual(diagnostic.relatedInformation[0].location.range.positions, [0, 0, 0, 6]);
  h.edit(dep);
  assert.equal(h.problems.size, 0);

  dep.isDirty = false;
  const clean = await h.start(dep);
  clean.resolve(result());
  await clean.finished;
  assert.equal(h.problems.size, 0);
});

test('old save results cannot replace diagnostics and edits to related dependencies invalidate pending checks', async t => {
  const h = await harness(t);
  const main = await h.document('main.jk', 'x');
  const dep = await h.document('other/dep.jk', 'y');
  const old = await h.start(main);
  h.edit(main);
  assert.equal(old.options.signal.aborted, true);
  main.isDirty = false;
  const latest = await h.start(main);
  latest.resolve(result(error(at(main, '', 'x'))));
  await latest.finished;
  old.resolve(result());
  await old.finished;
  assert.equal(h.problems.size, 1);

  const pending = await h.start(main);
  h.edit(dep);
  pending.resolve(result(error(at(main, '', 'x'), [{ location: at(dep, '', 'y'), message: 'Dependency' }])));
  await pending.finished;
  assert.equal(h.problems.size, 0);
});

test('save checks respect trust and configuration and log unlocated failures', async t => {
  const h = await harness(t);
  const doc = await h.document('main.jk', 'x');
  h.workspace.isTrusted = false;
  await h.save(doc);
  h.workspace.isTrusted = true;
  h.workspace.getConfiguration = () => ({ get: () => false });
  await h.save(doc);
  assert.equal(h.calls.length, 0);
  h.workspace.getConfiguration = () => ({ get: () => true });
  const check = await h.start(doc);
  check.resolve(result(error(null)));
  await check.finished;
  assert.equal(h.problems.size, 0);
  assert.match(h.log[0], /syntax_error.*Invalid syntax/);
  const pending = await h.start(doc);
  h.configured({ affectsConfiguration: key => key === 'joky.checkOnSave' });
  assert.equal(pending.options.signal.aborted, true);
  pending.resolve(result(error(at(doc, '', 'x'))));
  await pending.finished;
  assert.equal(h.problems.size, 0);
});

test('a save still resolving its target is superseded by a newer successful check', async t => {
  const h = await harness(t);
  await fs.writeFile(path.join(h.root, 'joky.toml'), '');
  const main = await h.document('main.jk', 'x');
  const dep = await h.document('dep.jk', 'y');
  let release;
  h.setResolver((file, root) => file === main.uri.fsPath ? new Promise(resolve => {
    release = async () => resolve(await toolchain.resolveTarget(file, root));
  }) : toolchain.resolveTarget(file, root));
  const old = h.save(main);
  const newest = await h.start(dep);
  newest.resolve(result());
  await newest.finished;
  await release();
  await old;
  assert.equal(h.calls.length, 1);
});
