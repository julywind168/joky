const vscode = require('vscode');
const path = require('node:path');
const toolchain = require('./toolchain.cjs');
const { processTerminal } = require('./terminal.cjs');

const installation = 'https://github.com/julywind168/joky/blob/main/README.md#安装与产物';

function activate(context) {
  const output = vscode.window.createOutputChannel('Joky');
  const missing = new Set();
  const processes = new Set();
  context.subscriptions.push(output, { dispose: () => { for (const process of processes) process.abort(); } });

  function report(error) {
    output.appendLine(error.message);
    vscode.window.showErrorMessage(`Joky: ${error.message.split('\n')[0]}`, 'Show Output')
      .then(choice => { if (choice === 'Show Output') output.show(true); });
  }

  async function selectCompiler(uri = vscode.window.activeTextEditor?.document.uri) {
    const selected = await vscode.window.showOpenDialog({
      title: 'Select the Joky executable', canSelectFiles: true, canSelectFolders: false, canSelectMany: false,
      openLabel: 'Use Joky',
    });
    if (!selected?.length) return false;
    const configuration = vscode.workspace.getConfiguration('joky', uri);
    const current = configuration.inspect('executablePath');
    const scope = current?.workspaceFolderValue !== undefined ? vscode.ConfigurationTarget.WorkspaceFolder
      : current?.workspaceValue !== undefined ? vscode.ConfigurationTarget.Workspace : vscode.ConfigurationTarget.Global;
    await configuration.update('executablePath', selected[0].fsPath, scope);
    missing.clear();
    return true;
  }

  async function compiler(uri) {
    const setting = vscode.workspace.getConfiguration('joky', uri).get('executablePath', 'joky');
    try {
      const executable = await toolchain.resolveExecutable(setting);
      const version = await toolchain.execute(executable, ['--version'], { timeout: 5000 });
      if (!/^joky\s+\S+/i.test(version.trim())) throw new Error('The selected executable does not report a Joky version.');
      return executable;
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      output.appendLine(error.message);
      if (missing.has(setting)) return undefined;
      missing.add(setting);
      vscode.window.showErrorMessage(
        'Joky compiler not found. Install Joky or select its executable.', 'Install Joky', 'Select Compiler',
      ).then(async choice => {
        if (choice === 'Install Joky') await vscode.env.openExternal(vscode.Uri.parse(installation));
        if (choice === 'Select Compiler') await selectCompiler(uri);
      }).catch(report);
      return undefined;
    }
  }

  function trusted() {
    if (vscode.workspace.isTrusted) return true;
    vscode.window.showInformationMessage('Trust this workspace to run Joky commands. Syntax highlighting remains available.');
    return false;
  }

  async function targetUri(uri) {
    if (uri) return uri;
    let document = vscode.window.activeTextEditor?.document;
    if (document?.languageId === 'joky' || path.basename(document?.uri.fsPath || '') === 'joky.toml') {
      if (document.isUntitled) {
        if (!await document.save()) return undefined;
        document = vscode.window.activeTextEditor?.document;
      }
      return document?.uri;
    }
    const folders = vscode.workspace.workspaceFolders || [];
    if (folders.length === 1) return folders[0].uri;
    if (folders.length > 1) {
      return (await vscode.window.showWorkspaceFolderPick({ placeHolder: 'Choose a Joky project' }))?.uri;
    }
    throw new Error('Open a .jk file or a Joky project first.');
  }

  async function command(action, uri) {
    if (!trusted()) return;
    uri = await targetUri(uri);
    if (!uri) return;
    if (uri.scheme !== 'file') throw new Error('Save the file locally before running Joky.');
    const folder = vscode.workspace.getWorkspaceFolder(uri);
    const target = await toolchain.resolveTarget(uri.fsPath, folder?.uri.fsPath);
    const executable = await compiler(uri);
    if (!executable) return;
    for (const document of vscode.workspace.textDocuments) {
      if (document.isDirty && document.uri.scheme === 'file' && toolchain.contains(target.cwd, document.uri.fsPath) &&
        (document.languageId === 'joky' || path.basename(document.uri.fsPath) === 'joky.toml')) {
        if (!await document.save()) return;
      }
    }
    // VS Code's task service needs an open workspace. Single-file windows can
    // still run a process directly in a terminal without passing through a shell.
    if (!vscode.workspace.workspaceFolders?.length) {
      const terminal = vscode.window.createTerminal({
        name: `Joky ${action}`,
        pty: processTerminal(executable, [action, '--', target.target], target.cwd),
        isTransient: true,
      });
      context.subscriptions.push(terminal);
      terminal.show(action !== 'run');
      return terminal;
    }
    const task = new vscode.Task(
      { type: 'joky', command: action, target: target.target }, folder || vscode.TaskScope.Workspace,
      `${action === 'run' ? 'Run' : 'Check'} ${path.basename(target.project ? target.cwd : target.target)}`,
      'Joky', new vscode.ProcessExecution(executable, [action, '--', target.target], { cwd: target.cwd }), [],
    );
    task.presentationOptions = {
      reveal: vscode.TaskRevealKind.Always, panel: vscode.TaskPanelKind.Dedicated,
      clear: true, focus: action === 'run', showReuseMessage: false,
    };
    return vscode.tasks.executeTask(task);
  }

  const provider = {
    async provideDocumentFormattingEdits(document, _options, token) {
      if (!trusted() || token.isCancellationRequested) return [];
      const version = document.version;
      const original = document.getText();
      const controller = new AbortController();
      processes.add(controller);
      const cancellation = token.onCancellationRequested(() => controller.abort());
      try {
        const executable = await compiler(document.uri);
        if (!executable || token.isCancellationRequested) return [];
        const formatted = await toolchain.execute(executable, ['fmt', '--stdin'], {
          input: original, signal: controller.signal,
        });
        if (controller.signal.aborted || token.isCancellationRequested || document.isClosed || document.version !== version) return [];
        const edit = toolchain.replacement(original, formatted);
        return edit ? [vscode.TextEdit.replace(new vscode.Range(
          document.positionAt(edit.start), document.positionAt(edit.end),
        ), edit.text)] : [];
      } catch (error) {
        if (!token.isCancellationRequested && !controller.signal.aborted) await report(error);
        return [];
      } finally {
        cancellation.dispose();
        processes.delete(controller);
      }
    },
  };
  const register = (name, callback) => context.subscriptions.push(vscode.commands.registerCommand(name, async (...args) => {
    try { return await callback(...args); } catch (error) { await report(error); }
  }));
  register('joky.run', uri => command('run', uri));
  register('joky.check', uri => command('check', uri));
  register('joky.selectCompiler', selectCompiler);
  register('joky.format', () => {
    if (vscode.window.activeTextEditor?.document.languageId === 'joky' && trusted()) {
      return vscode.commands.executeCommand('editor.action.formatDocument');
    }
  });
  context.subscriptions.push(
    vscode.languages.registerDocumentFormattingEditProvider([
      { language: 'joky', scheme: 'file' }, { language: 'joky', scheme: 'untitled' },
    ], provider),
    vscode.workspace.onDidChangeConfiguration(event => { if (event.affectsConfiguration('joky.executablePath')) missing.clear(); }),
  );
}

module.exports = { activate };
