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
  assert.equal(document.getText(), '// unsaved 中文🙂\nfn main() { println(42) }\n');
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

  const untitled = await vscode.workspace.openTextDocument({ language: 'joky', content: original });
  await vscode.window.showTextDocument(untitled);
  const unsavedEdits = await vscode.commands.executeCommand('vscode.executeFormatDocumentProvider', untitled.uri, { tabSize: 4, insertSpaces: true });
  assert.ok(unsavedEdits.length > 0, 'untitled Joky documents can be formatted');
  console.log('Joky extension host: formatting, standalone tasks and project tasks passed.');
  await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor');
  await fs.writeFile(path.join(root, 'host-result.txt'), 'passed');
}

module.exports = { run };
