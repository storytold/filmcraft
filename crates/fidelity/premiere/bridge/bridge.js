/* The FilmCraft fidelity bridge panel.
 *
 * render.py writes `<id>.cmd` (JSON: {script, timeout_ms}) into ~/.filmcraft/fidelity-bridge/;
 * this panel runs the ExtendScript inside Premiere Pro and answers with `<id>.out`
 * (JSON: {ok, result} or {ok, error}). It writes heartbeat.json every tick so the driver can tell
 * "panel not open" from "Premiere busy". Commands run one at a time, in name order. The panel
 * needs no interaction: once it is open in a workspace, Premiere reopens it with that workspace.
 */
(function () {
  var fs = require('fs');
  var path = require('path');
  var os = require('os');

  var DIR = path.join(os.homedir(), '.filmcraft', 'fidelity-bridge');
  var POLL_MS = 200;
  var DEFAULT_TIMEOUT_MS = 10 * 60 * 1000;
  var MAX_TIMEOUT_MS = 60 * 60 * 1000;

  var busy = false;
  var lines = [];

  function show(id, text) {
    var el = document.getElementById(id);
    if (el) el.textContent = text;
  }

  function log(text) {
    lines.push(new Date().toISOString().slice(11, 19) + ' ' + text);
    if (lines.length > 40) lines.shift();
    show('log', lines.join('\n'));
  }

  function writeAtomic(name, obj) {
    var tmp = path.join(DIR, name + '.tmp');
    fs.writeFileSync(tmp, JSON.stringify(obj));
    fs.renameSync(tmp, path.join(DIR, name));
  }

  function respond(id, obj) {
    try {
      writeAtomic(id + '.out', obj);
    } catch (e) {
      log('could not answer ' + id + ': ' + e);
    }
  }

  function evalScript(source, callback) {
    // The host bridge every CEP panel has (CSInterface.evalScript is a wrapper around it).
    window.__adobe_cep__.evalScript(source, callback);
  }

  function runOne(name) {
    var id = name.slice(0, -'.cmd'.length);
    var file = path.join(DIR, name);
    var request;
    try {
      request = JSON.parse(fs.readFileSync(file, 'utf8'));
    } catch (e) {
      try { fs.unlinkSync(file); } catch (ignored) {}
      respond(id, { ok: false, error: 'unreadable command: ' + e });
      return;
    }
    try { fs.unlinkSync(file); } catch (ignored) {}
    var timeoutMs = Math.min(MAX_TIMEOUT_MS, Math.max(1000, Number(request.timeout_ms) || DEFAULT_TIMEOUT_MS));
    var done = false;
    busy = true;
    show('status', 'running ' + id);
    var timer = setTimeout(function () {
      if (done) return;
      done = true;
      busy = false;
      show('status', 'idle (last command timed out)');
      respond(id, { ok: false, error: 'no answer from Premiere in ' + timeoutMs + ' ms: a dialog may be blocking it' });
    }, timeoutMs);
    var wrapped = '(function () { try { ' + String(request.script || '') + '\n } catch (e) { return "__filmcraft_error__ " + e.toString(); } })()';
    evalScript(wrapped, function (result) {
      if (done) return;
      done = true;
      clearTimeout(timer);
      busy = false;
      show('status', 'idle');
      if (typeof result === 'string' && result.indexOf('__filmcraft_error__ ') === 0) {
        respond(id, { ok: false, error: result.slice('__filmcraft_error__ '.length) });
      } else if (result === 'EvalScript error.') {
        respond(id, { ok: false, error: 'EvalScript error: the script did not parse, or the host is not ready' });
      } else {
        respond(id, { ok: true, result: result });
      }
    });
  }

  function tick() {
    try {
      fs.mkdirSync(DIR, { recursive: true });
      writeAtomic('heartbeat.json', { ts: Date.now(), busy: busy });
      if (busy) return;
      var pending = fs.readdirSync(DIR).filter(function (n) { return n.slice(-4) === '.cmd'; }).sort();
      if (pending.length) runOne(pending[0]);
    } catch (e) {
      show('status', 'error: ' + e);
    }
  }

  show('dir', DIR);
  show('status', 'idle');
  log('watching ' + DIR);
  setInterval(tick, POLL_MS);
})();
