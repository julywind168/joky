const vscode = require('vscode');
const { spawn } = require('node:child_process');

// Keep completed output visible even in single-file windows without task support.
function processTerminal(executable, args, cwd) {
  const write = new vscode.EventEmitter();
  let child;
  let input = '';
  let finished = false;
  return {
    onDidWrite: write.event,
    open() {
      child = spawn(executable, args, { cwd, windowsHide: true });
      const display = data => write.fire(data.replace(/\r?\n/g, '\r\n'));
      child.stdout.setEncoding('utf8').on('data', display);
      child.stderr.setEncoding('utf8').on('data', display);
      child.stdin.on('error', () => {});
      child.on('error', error => display(error.message));
      child.on('close', (code, signal) => {
        finished = true;
        write.fire(`\r\n[Joky exited with ${signal ? `signal ${signal}` : `code ${code}`}]\r\n`);
      });
    },
    handleInput(data) {
      if (!child || finished || data.startsWith('\x1b')) return;
      for (const character of data.replace(/\r\n/g, '\r')) {
        if (character === '\x03') {
          input = '';
          write.fire('^C\r\n');
          child.kill('SIGINT');
        } else if (character === '\x04') {
          child.stdin.end(input);
          input = '';
        } else if (character === '\r' || character === '\n') {
          child.stdin.write(`${input}\n`);
          input = '';
          write.fire('\r\n');
        } else if (character === '\x7f' || character === '\b') {
          if (input) { input = [...input].slice(0, -1).join(''); write.fire('\b \b'); }
        } else if (character >= ' ' || character === '\t') {
          input += character;
          write.fire(character);
        }
      }
    },
    close() {
      if (child && !finished) child.kill();
      write.dispose();
    },
  };
}

module.exports = { processTerminal };
