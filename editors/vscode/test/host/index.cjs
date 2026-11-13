const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const vscode = require('vscode');

async function runTask(command, uri, expectedTarget) {
  let subscription;
  let terminalOutput;
  let timer;
  const completed = new Promise((resolve, reject) => {
    timer = setTimeout(() => reject(new Error(`${command} did not finish`)), 20000);
    if (!vscode.workspace.workspaceFolders?.length) {
      subscription = vscode.window.onDidOpenTerminal(terminal => {
        if (!terminal.name.startsWith('Joky ')) return;
        let output = '';
        terminalOutput = terminal.creationOptions.pty.onDidWrite(data => {
          output += data;
          if (!output.includes('[Joky exited with')) return;
          try {
            assert.ok(output.includes('[Joky exited with code 0]'), output);
            if (command === 'joky.run') assert.ok(output.includes('42'), output);
            resolve();
          } catch (error) { reject(error); }
          terminal.dispose();
        });
      });
    } else subscription = vscode.tasks.onDidEndTaskProcess(event => {
      if (event.execution.task.definition.type !== 'joky') return;
      try {
        assert.equal(event.exitCode, 0);
        assert.equal(event.execution.task.definition.target, expectedTarget);
        assert.ok(event.execution.task.execution instanceof vscode.ProcessExecution);
        resolve();
      } catch (error) { reject(error); }
    });
  });
  try {
    await vscode.commands.executeCommand(command, uri);
    await completed;
  } finally {
    clearTimeout(timer);
    subscription.dispose();
    terminalOutput?.dispose();
  }
}

async function waitForDiagnostics(uri, count) {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    const diagnostics = vscode.languages.getDiagnostics(uri).filter(value => value.source === 'Joky');
    if (diagnostics.length === count) return diagnostics;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error(`Expected ${count} Joky diagnostics for ${uri.fsPath}`);
}

async function run() {
  const root = process.env.JOKY_TEST_WORKSPACE;
  const compiler = process.env.JOKY_TEST_COMPILER;
  assert.ok(root && compiler, 'Run through npm run test:host.');
  const extension = vscode.extensions.getExtension('joky-local.joky');
  assert.ok(extension);
  await extension.activate();
  await vscode.workspace.getConfiguration('joky').update('executablePath', compiler, vscode.ConfigurationTarget.Global);
  const file = path.join(root, 'hello ; $name.jk');
  const original = 'fn main(){println(42)}\n';
  await fs.writeFile(file, original);
  const document = await vscode.workspace.openTextDocument(file);
  const editor = await vscode.window.showTextDocument(document);
  assert.equal(document.languageId, 'joky');
  await editor.edit(edit => edit.insert(new vscode.Position(0, 0), '// unsaved 中文🙂\n'));
  const edits = await vscode.commands.executeCommand('vscode.executeFormatDocumentProvider', document.uri, { tabSize: 4, insertSpaces: true });
  assert.ok(edits.length > 0);
  const change = new vscode.WorkspaceEdit();
  change.set(document.uri, edits);
  assert.ok(await vscode.workspace.applyEdit(change));
  assert.equal(document.getText(), '// unsaved 中文🙂\nfn main() {\n    println(42)\n}\n');
  assert.equal(await fs.readFile(file, 'utf8'), original, 'formatting must not write the file');
  assert.equal(document.isDirty, true);
  await runTask('joky.check', document.uri, file);
  assert.equal(await fs.readFile(file, 'utf8'), document.getText(), 'checking saves the selected source');
  await runTask('joky.run', document.uri, file);

  const project = path.join(root, 'project');
  const entry = path.join(project, 'main.jk');
  await fs.mkdir(project);
  const manifest = path.join(project, 'joky.toml');
  await fs.writeFile(manifest, '[project]\nname = "demo"\nentry = "main.jk"\nsource-root = "."\n');
  await fs.writeFile(entry, original);
  await runTask('joky.check', vscode.Uri.file(entry), manifest);
  await runTask('joky.run', vscode.Uri.file(entry), manifest);

  const values = path.join(project, 'values.jk');
  const valuesUri = vscode.Uri.file(values);
  await fs.writeFile(values, 'pub fn answer() -> Int32 { 42 }\n');
  const projectDocument = await vscode.workspace.openTextDocument(entry);
  const projectEditor = await vscode.window.showTextDocument(projectDocument);
  const navigation = 'import values\nfn main() {\n    let result = values.answer()\n    println(result)\n}\n';
  await projectEditor.edit(edit => edit.replace(new vscode.Range(0, 0, projectDocument.lineCount, 0), navigation));
  const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider',
    projectDocument.uri, new vscode.Position(3, 13));
  assert.ok(hovers.some(hover => hover.contents.some(content => content.value.includes('result: Int32'))));
  const definitions = () => vscode.commands.executeCommand('vscode.executeDefinitionProvider',
    projectDocument.uri, new vscode.Position(2, 24));
  const [definition] = await definitions();
  assert.equal(definition.uri.fsPath, values);
  assert.equal(definition.range.start.line, 0);
  assert.equal(definition.range.start.character, 7);
  const valuesDocument = await vscode.workspace.openTextDocument(valuesUri);
  const valuesEditor = await vscode.window.showTextDocument(valuesDocument);
  await valuesEditor.edit(edit => edit.insert(new vscode.Position(0, 0), '// unsaved\n'));
  const [updated] = await definitions();
  assert.equal(updated.uri.fsPath, values);
  assert.equal(updated.range.start.line, 1);

  const completionEditor = await vscode.window.showTextDocument(projectDocument);
  const answer = new vscode.Range(2, 24, 2, 32);
  assert.ok(await completionEditor.edit(edit => edit.delete(answer)));
  const suggestions = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider',
    projectDocument.uri, answer.start, '.');
  const candidate = suggestions.items.find(item => item.label === 'answer' && item.detail);
  assert.equal(candidate?.detail, 'fn answer() -> Int32');
  assert.equal(candidate.kind, vscode.CompletionItemKind.Function);
  assert.ok(candidate.range.start.isEqual(answer.start) && candidate.range.end.isEqual(answer.start));
  assert.ok(await completionEditor.edit(edit => edit.insert(answer.start, 'answer()')));

  const record = 'pub struct Item { pub amount: Int32, hidden: Bool }\n' +
    'impl Item { pub fn read(&self) -> Int32 { self.amount } }\n' +
    'pub fn load() -> Item { Item { amount: 42, hidden: false } }\n';
  await fs.writeFile(path.join(project, 'record.jk'), record);
  const memberSource = 'import record\nfn main() {\n    record.load().\n}\n';
  assert.ok(await completionEditor.edit(edit => edit.replace(
    new vscode.Range(0, 0, projectDocument.lineCount, 0), memberSource)));
  const memberPosition = new vscode.Position(2, 18);
  const members = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider',
    projectDocument.uri, memberPosition, '.');
  const field = members.items.find(item => item.label === 'amount' && item.detail);
  const method = members.items.find(item => item.label === 'read' && item.detail);
  assert.equal(field?.detail, 'Int32');
  assert.equal(field.kind, vscode.CompletionItemKind.Field);
  assert.equal(method?.detail, 'fn read() -> Int32');
  assert.equal(method.kind, vscode.CompletionItemKind.Method);
  assert.ok(field.range.start.isEqual(memberPosition) && field.range.end.isEqual(memberPosition));
  assert.ok(!members.items.some(item => item.label === 'hidden' && item.detail));

  const interpolation = [
    'struct User { name: String, age: UInt64 }',
    'fn main() {',
    '    let user = User { name: "Alice", age: 30 }',
    '    println("User: {user.}, Age: {user.age}")',
    '}',
    '',
  ];
  assert.ok(await completionEditor.edit(edit => edit.replace(
    new vscode.Range(0, 0, projectDocument.lineCount, 0), interpolation.join('\n'))));
  const hole = new vscode.Position(3, interpolation[3].indexOf('user.') + 'user.'.length);
  const suggestionsInHole = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider',
    projectDocument.uri, hole, '.');
  const name = suggestionsInHole.items.find(item => item.label === 'name' && item.detail);
  assert.equal(name?.detail, 'String');
  assert.equal(name.kind, vscode.CompletionItemKind.Field);
  assert.ok(name.range.start.isEqual(hole) && name.range.end.isEqual(hole));
  assert.ok(await completionEditor.edit(edit => edit.replace(
    new vscode.Range(0, 0, projectDocument.lineCount, 0), navigation)));

  const dependency = path.join(project, 'bad.jk');
  const bad = '// heading\r\npub fn fail() { let text = "🙂" $ }\r\n';
  await fs.writeFile(dependency, `\uFEFF${bad}`);
  const diagnosticsEditor = await vscode.window.showTextDocument(projectDocument);
  await diagnosticsEditor.edit(edit => edit.insert(new vscode.Position(0, 0), 'import bad\n'));
  assert.ok(await projectDocument.save());
  const dependencyUri = vscode.Uri.file(dependency);
  const [problem] = await waitForDiagnostics(dependencyUri, 1);
  assert.equal(problem.code, 'syntax_error');
  assert.equal(problem.range.start.line, 1);
  assert.equal(problem.range.start.character, bad.split('\r\n')[1].indexOf('$'));
  assert.equal(problem.range.end.character, problem.range.start.character + 1);

  const dependencyDocument = await vscode.workspace.openTextDocument(dependencyUri);
  const dependencyEditor = await vscode.window.showTextDocument(dependencyDocument);
  await dependencyEditor.edit(edit => edit.replace(problem.range, ''));
  await waitForDiagnostics(dependencyUri, 0);
  assert.equal(dependencyDocument.isDirty, true);
  assert.ok((await fs.readFile(dependency, 'utf8')).includes('$'));
  await dependencyEditor.edit(edit => edit.insert(new vscode.Position(1, 0), '$'));
  const [unsaved] = await waitForDiagnostics(dependencyUri, 1);
  assert.equal(unsaved.range.start.line, 1);
  assert.equal(unsaved.range.start.character, 0);
  assert.ok((await fs.readFile(dependency, 'utf8')).includes('$'));
  await dependencyEditor.edit(edit => edit.replace(unsaved.range, ''));
  await waitForDiagnostics(dependencyUri, 0);
  assert.ok(await dependencyDocument.save());

  const untitled = await vscode.workspace.openTextDocument({ language: 'joky', content: original });
  await vscode.window.showTextDocument(untitled);
  const unsavedEdits = await vscode.commands.executeCommand('vscode.executeFormatDocumentProvider', untitled.uri, { tabSize: 4, insertSpaces: true });
  assert.ok(unsavedEdits.length > 0, 'untitled Joky documents can be formatted');
  console.log('Joky extension host: formatting, diagnostics, navigation, completion and tasks passed.');
  await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor');
  await fs.writeFile(path.join(root, 'host-result.txt'), 'passed');
}

module.exports = { run };
