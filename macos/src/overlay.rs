//! Was die App selbst in die Oberfläche bringt: die Startseite (bis der Server antwortet) und den
//! Update-Hinweis mit Knöpfen. Der Hinweis wird als Skript in jede Seite eingefügt und spricht über
//! `window.ipc.postMessage` mit der App (`update:install`, `update:later`, `update:notes`).

/// Seite, solange der Server startet.
pub fn loading_page(error: Option<&str>) -> String {
    let (title, detail) = match error {
        None => ("luCode startet …".to_string(), String::new()),
        Some(e) => ("luCode konnte nicht starten".to_string(), html_escape(e)),
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
:root {{ color-scheme: light dark; }}
html,body {{ margin:0; height:100%; font: 14px -apple-system, BlinkMacSystemFont, "Inter", sans-serif; background:#f4f1ff; color:#141030; }}
@media (prefers-color-scheme: dark) {{ html,body {{ background:#141030; color:#f4f1ff; }} .d {{ color:#a9a1d6; }} }}
.w {{ height:100%; display:flex; flex-direction:column; align-items:center; justify-content:center; gap:14px; -webkit-user-select:none; cursor:default; }}
.s {{ width:22px; height:22px; border-radius:50%; border:2px solid currentColor; border-right-color:transparent; animation:r .8s linear infinite; opacity:.6; }}
.d {{ color:#6f6893; max-width:520px; text-align:center; white-space:pre-wrap; -webkit-user-select:text; }}
@keyframes r {{ to {{ transform:rotate(360deg); }} }}
</style></head><body><div class="w">{spinner}<div>{title}</div><div class="d">{detail}</div></div></body></html>"#,
        spinner = if error.is_none() { r#"<div class="s"></div>"# } else { "" },
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// JSON-String für eingebettetes JavaScript.
pub fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Läuft am Anfang jeder Seite (nur im Hauptrahmen) und stellt `window.__luUpdate` bereit.
pub const INIT_SCRIPT: &str = r#"
(function () {
  if (window.__luUpdate) return;
  var css = '#lu-update{position:fixed;right:16px;bottom:16px;z-index:2147483645;width:340px;box-sizing:border-box;'
    + 'padding:14px 14px 12px;border-radius:12px;border:1px solid rgba(0,0,0,.12);background:#fff;color:#141030;'
    + 'box-shadow:0 10px 30px rgba(0,0,0,.18);font:13px -apple-system,BlinkMacSystemFont,Inter,sans-serif;}'
    + '#lu-update .t{font-weight:600;margin-bottom:2px}#lu-update .m{color:#6f6893;margin-bottom:10px;line-height:1.4}'
    + '#lu-update .b{display:flex;gap:8px;justify-content:flex-end}'
    + '#lu-update button{font:inherit;border-radius:8px;padding:5px 11px;cursor:pointer;white-space:nowrap;border:1px solid rgba(0,0,0,.15);background:transparent;color:inherit}'
    + '#lu-update button.p{background:#5b4bc4;color:#fff;border-color:#5b4bc4}'
    + '#lu-update .bar{height:4px;border-radius:2px;background:rgba(0,0,0,.1);overflow:hidden;margin:4px 0 10px}'
    + '#lu-update .bar>div{height:100%;width:0;background:#5b4bc4;transition:width .2s}'
    + '#lu-update.err .m{color:#dc2626}'
    + '@media (prefers-color-scheme: dark){#lu-update{background:#1d1742;color:#f4f1ff;border-color:rgba(185,174,251,.2)}'
    + '#lu-update .m{color:#a9a1d6}#lu-update button{border-color:rgba(185,174,251,.3)}'
    + '#lu-update button.p{background:#d6cffd;color:#141030;border-color:#d6cffd}'
    + '#lu-update .bar{background:rgba(185,174,251,.2)}#lu-update .bar>div{background:#b9aefb}}';
  function send(m) { try { window.ipc.postMessage(m); } catch (e) {} }
  function box() {
    if (!document.body) return null;
    if (!document.getElementById('lu-update-css')) {
      var s = document.createElement('style'); s.id = 'lu-update-css'; s.textContent = css;
      (document.head || document.body).appendChild(s);
    }
    var b = document.getElementById('lu-update');
    if (!b) { b = document.createElement('div'); b.id = 'lu-update'; document.body.appendChild(b); }
    b.className = ''; b.textContent = '';
    return b;
  }
  function add(p, tag, cls, text) { var e = document.createElement(tag); if (cls) e.className = cls; if (text) e.textContent = text; p.appendChild(e); return e; }
  function button(p, text, primary, msg) { var e = add(p, 'button', primary ? 'p' : '', text); e.onclick = function () { send(msg); }; return e; }
  window.__luUpdate = {
    show: function (version, title) {
      var b = box(); if (!b) return;
      add(b, 'div', 't', 'Update verfügbar: luCode ' + version);
      add(b, 'div', 'm', (title && title !== 'luCode ' + version ? title + ' – ' : '') + 'Die neue Version wird geladen, installiert und die App neu gestartet.');
      var r = add(b, 'div', 'b');
      button(r, 'Neuerungen', false, 'update:notes');
      button(r, 'Später', false, 'update:later');
      button(r, 'Aktualisieren', true, 'update:install');
    },
    progress: function (p) {
      var b = document.getElementById('lu-update');
      if (!b || !b.querySelector('.bar')) {
        b = box(); if (!b) return;
        add(b, 'div', 't', 'Update wird geladen …');
        add(add(b, 'div', 'bar'), 'div');
        add(b, 'div', 'm');
      }
      var pct = p < 0 ? null : Math.round(p * 100);
      b.querySelector('.bar>div').style.width = (pct === null ? 50 : pct) + '%';
      b.querySelector('.m').textContent = pct === null ? 'Bitte warten …' : pct + ' %';
    },
    status: function (title, text, error) {
      var b = box(); if (!b) return;
      if (error) b.className = 'err';
      add(b, 'div', 't', title);
      add(b, 'div', 'm', text);
      var r = add(b, 'div', 'b');
      button(r, 'Schliessen', !error, 'update:later');
      if (error) button(r, 'Erneut versuchen', true, 'update:install');
    },
    hide: function () { var b = document.getElementById('lu-update'); if (b) b.remove(); }
  };
})();
"#;

/// Sperrbildschirm (Code-Eingabe), Einstellungen der App und Meldung von Eingaben an die App
/// (für die Sperre nach Inaktivität). Läuft wie `INIT_SCRIPT` am Anfang jeder Seite.
pub const LOCK_SCRIPT: &str = r#"
(function () {
  if (window.__luLock) return;
  function send(m) { try { window.ipc.postMessage(m); } catch (e) {} }
  // Eingaben melden – höchstens alle 20 Sekunden
  var last = 0;
  function act() { var t = Date.now(); if (t - last > 20000) { last = t; send('activity'); } }
  ['mousemove', 'mousedown', 'keydown', 'wheel', 'touchstart', 'scroll'].forEach(function (e) {
    window.addEventListener(e, act, { capture: true, passive: true });
  });
  // Liegt die Oberfläche vorn (Fokus in der Seite)? Ein vorgemerktes Update wartet dann länger –
  // vielleicht liest gerade jemand, ohne etwas einzugeben. (Das Fenster selbst weiss es nicht:
  // der Fokus liegt im WebView.)
  function front() { send(document.hasFocus() ? 'front:1' : 'front:0'); }
  window.addEventListener('focus', front);
  window.addEventListener('blur', front);
  front();
  var css = '#lu-lock,#lu-settings{position:fixed;inset:0;z-index:2147483647;display:flex;align-items:center;justify-content:center;'
    + 'font:14px -apple-system,BlinkMacSystemFont,Inter,sans-serif;color:#141030}'
    + '#lu-lock{background:#f4f1ff}#lu-settings{background:rgba(0,0,0,.35)}'
    + '.lu-card{width:340px;box-sizing:border-box;padding:24px;border-radius:14px;background:#fff;border:1px solid rgba(0,0,0,.1);box-shadow:0 20px 50px rgba(0,0,0,.18)}'
    + '#lu-lock .lu-card{text-align:center;box-shadow:none;background:transparent;border:0}'
    + '.lu-h{font-weight:600;font-size:16px;margin:0 0 4px}.lu-sub{color:#6f6893;font-size:13px;margin:0 0 18px;line-height:1.4}'
    + '.lu-pin{display:flex;gap:8px;justify-content:center}'
    + '.lu-pin input{width:42px;height:50px;text-align:center;font-size:22px;border-radius:10px;border:1px solid rgba(0,0,0,.18);background:#fff;color:inherit;outline:none;-webkit-text-security:disc}'
    + '.lu-pin input:focus{border-color:#5b4bc4;box-shadow:0 0 0 3px rgba(91,75,196,.22)}'
    + '.lu-err{color:#dc2626;font-size:13px;min-height:18px;margin-top:12px}'
    + '.lu-lbl{display:block;font-size:12px;font-weight:600;margin:14px 0 6px}'
    + '.lu-row{display:flex;align-items:center;justify-content:space-between;gap:12px}'
    + '.lu-card select{font:inherit;padding:6px 8px;border-radius:8px;border:1px solid rgba(0,0,0,.18);background:#fff;color:inherit}'
    + '.lu-btns{display:flex;gap:8px;justify-content:flex-end;margin-top:18px}'
    + '.lu-card button{font:inherit;border-radius:8px;padding:6px 14px;cursor:pointer;white-space:nowrap;border:1px solid rgba(0,0,0,.15);background:transparent;color:inherit}'
    + '.lu-card button.p{background:#5b4bc4;color:#fff;border-color:#5b4bc4}'
    + '.lu-ico{width:44px;height:44px;margin:0 auto 14px;border-radius:12px;display:flex;align-items:center;justify-content:center;background:#5b4bc4;color:#fff;font-size:20px}'
    + '#lu-settings .lu-card{width:420px;max-height:calc(100vh - 40px);overflow-y:auto}'
    + '.lu-sec{border-top:1px solid rgba(0,0,0,.1);margin-top:20px;padding-top:16px}'
    + '.lu-perm{display:flex;align-items:center;gap:10px;padding:8px 0;border-bottom:1px solid rgba(0,0,0,.06)}'
    + '.lu-perm:last-child{border-bottom:0}.lu-perm .i{flex:1;min-width:0}'
    + '.lu-perm .n{font-size:13px;font-weight:500;display:flex;align-items:center;gap:6px}'
    + '.lu-perm .d{font-size:12px;color:#6f6893;line-height:1.35;margin-top:1px}'
    + '.lu-dot{width:8px;height:8px;border-radius:50%;background:#a9a1d6;flex:none}'
    + '.lu-dot.granted{background:#16a34a}.lu-dot.denied{background:#dc2626}'
    + '.lu-card .lu-perm button{padding:4px 10px;font-size:12px}'
    + '.lu-ok{font-size:12px;color:#16a34a;white-space:nowrap}'
    + '@media (prefers-color-scheme: dark){#lu-lock,#lu-settings{color:#f4f1ff}#lu-lock{background:#141030}'
    + '.lu-card{background:#1d1742;border-color:rgba(185,174,251,.16)}.lu-sub{color:#a9a1d6}'
    + '.lu-pin input,.lu-card select{background:#141030;border-color:rgba(185,174,251,.3)}'
    + '.lu-pin input:focus{border-color:#b9aefb;box-shadow:0 0 0 3px rgba(185,174,251,.3)}'
    + '.lu-card button{border-color:rgba(185,174,251,.3)}.lu-card button.p{background:#d6cffd;color:#141030;border-color:#d6cffd}'
    + '.lu-ico{background:#d6cffd;color:#141030}.lu-sec{border-color:rgba(185,174,251,.16)}'
    + '.lu-perm{border-color:rgba(185,174,251,.1)}.lu-perm .d{color:#a9a1d6}.lu-ok{color:#4ade80}}';
  function style() {
    if (document.getElementById('lu-lock-css')) return;
    var s = document.createElement('style'); s.id = 'lu-lock-css'; s.textContent = css;
    (document.head || document.body).appendChild(s);
  }
  function el(p, tag, cls, text) { var e = document.createElement(tag); if (cls) e.className = cls; if (text) e.textContent = text; p.appendChild(e); return e; }
  // Sechs Eingabefelder für den Code; onDone(code) bei der sechsten Ziffer
  function pinBox(p, onDone) {
    var row = el(p, 'div', 'lu-pin'), inputs = [];
    for (var i = 0; i < 6; i++) {
      (function (i) {
        var inp = el(row, 'input');
        inp.inputMode = 'numeric'; inp.maxLength = 1; inp.autocomplete = 'off';
        inp.addEventListener('input', function () {
          var v = inp.value.replace(/\D/g, '');
          if (v.length > 1) { // eingefügt
            for (var k = 0; k < 6 - i && k < v.length; k++) inputs[i + k].value = v[k];
            var n = Math.min(5, i + v.length); inputs[n].focus();
          } else { inp.value = v; if (v && i < 5) inputs[i + 1].focus(); }
          var code = inputs.map(function (x) { return x.value; }).join('');
          if (code.length === 6 && onDone) onDone(code);
        });
        inp.addEventListener('keydown', function (e) {
          e.stopPropagation();
          if (e.key === 'Backspace' && !inp.value && i > 0) { inputs[i - 1].value = ''; inputs[i - 1].focus(); e.preventDefault(); }
          if (e.key === 'ArrowLeft' && i > 0) inputs[i - 1].focus();
          if (e.key === 'ArrowRight' && i < 5) inputs[i + 1].focus();
        });
        inputs.push(inp);
      })(i);
    }
    return {
      value: function () { return inputs.map(function (x) { return x.value; }).join(''); },
      clear: function () { inputs.forEach(function (x) { x.value = ''; }); inputs[0].focus(); },
      focus: function () { inputs[0].focus(); }
    };
  }
  var lockPin = null, permsAll = null;
  // Zurück aus den Systemeinstellungen: Zustand der Rechte neu holen
  window.addEventListener('focus', function () { if (document.getElementById('lu-perms')) send('perm:list'); });
  function stop(e) { e.stopPropagation(); }
  window.__luLock = {
    show: function () {
      if (!document.body) return;
      style();
      var o = document.getElementById('lu-lock');
      if (o) { if (lockPin) lockPin.focus(); return; }
      var s = document.getElementById('lu-settings'); if (s) s.remove();
      o = el(document.body, 'div'); o.id = 'lu-lock';
      ['keydown', 'keyup', 'keypress', 'paste', 'mousedown', 'click', 'wheel'].forEach(function (t) { o.addEventListener(t, stop); });
      var c = el(o, 'div', 'lu-card');
      el(c, 'div', 'lu-ico', '\u{1F512}');
      el(c, 'p', 'lu-h', 'luCode ist gesperrt');
      el(c, 'p', 'lu-sub', 'Code eingeben, um weiterzuarbeiten. Agenten laufen im Hintergrund weiter.');
      var err = el(c, 'div', 'lu-err');
      lockPin = pinBox(c, function (code) { err.textContent = ''; send('lock:unlock:' + code); });
      c.appendChild(err);
      setTimeout(function () { lockPin.focus(); }, 30);
    },
    error: function (msg) {
      var o = document.getElementById('lu-lock'); if (!o) return;
      o.querySelector('.lu-err').textContent = msg;
      if (lockPin) lockPin.clear();
    },
    hide: function () { var o = document.getElementById('lu-lock'); if (o) o.remove(); lockPin = null; }
  };
  window.__luSettings = {
    // enabled: Code gesetzt; minutes: Minuten bis zur Sperre; perms: Rechte von macOS zeigen
    show: function (enabled, minutes, perms) {
      if (!document.body || document.getElementById('lu-lock')) return;
      style();
      var old = document.getElementById('lu-settings'); if (old) old.remove();
      var o = el(document.body, 'div'); o.id = 'lu-settings';
      ['keydown', 'keyup', 'keypress', 'paste'].forEach(function (t) { o.addEventListener(t, stop); });
      o.addEventListener('mousedown', function (e) { if (e.target === o) o.remove(); });
      var c = el(o, 'div', 'lu-card');
      el(c, 'p', 'lu-h', 'Einstellungen der App');
      el(c, 'p', 'lu-sub', 'Code-Sperre: Nach einer Zeit ohne Eingabe und beim Start fragt luCode nach einem 6-stelligen Code. Sitzung und Agenten laufen weiter.');
      var cur = null;
      if (enabled) { el(c, 'label', 'lu-lbl', 'Aktueller Code'); cur = pinBox(c); }
      var row = el(c, 'div', 'lu-row'); row.style.marginTop = '14px';
      el(row, 'span', 'lu-lbl', 'Code-Sperre').style.margin = '0';
      var mode = el(row, 'select');
      [['keep', enabled ? 'Eingeschaltet' : 'Ausgeschaltet'], ['set', enabled ? 'Code ändern' : 'Einschalten'], ['off', 'Ausschalten']]
        .forEach(function (x) { if (!enabled && x[0] === 'off') return; var op = el(mode, 'option', '', x[1]); op.value = x[0]; });
      var newBox = el(c, 'div'); newBox.style.display = 'none';
      el(newBox, 'label', 'lu-lbl', 'Neuer Code (6 Ziffern)'); var n1 = pinBox(newBox);
      el(newBox, 'label', 'lu-lbl', 'Neuer Code wiederholen'); var n2 = pinBox(newBox);
      mode.onchange = function () { newBox.style.display = mode.value === 'set' ? '' : 'none'; if (mode.value === 'set') n1.focus(); };
      var row2 = el(c, 'div', 'lu-row'); row2.style.marginTop = '14px';
      el(row2, 'span', 'lu-lbl', 'Sperren nach').style.margin = '0';
      var sel = el(row2, 'select');
      [1, 2, 5, 10, 15, 30, 60, 120, 240].forEach(function (m) { var op = el(sel, 'option', '', m < 60 ? m + ' Minuten' : (m / 60) + (m === 60 ? ' Stunde' : ' Stunden')); op.value = m; });
      if (![1, 2, 5, 10, 15, 30, 60, 120, 240].includes(minutes)) { var op = el(sel, 'option', '', minutes + ' Minuten'); op.value = minutes; }
      sel.value = String(minutes);
      var err = el(c, 'div', 'lu-err');
      var b = el(c, 'div', 'lu-btns');
      el(b, 'button', '', 'Abbrechen').onclick = function () { o.remove(); };
      el(b, 'button', 'p', 'Sichern').onclick = function () {
        err.textContent = '';
        var pin = null;
        if (mode.value === 'set') {
          if (n1.value().length !== 6) { err.textContent = 'Der neue Code muss 6 Ziffern haben.'; return; }
          if (n1.value() !== n2.value()) { err.textContent = 'Die beiden Codes sind verschieden.'; n2.clear(); return; }
          pin = n1.value();
        } else if (mode.value === 'off') pin = '';
        if (cur && cur.value().length !== 6) { err.textContent = 'Bitte den aktuellen Code eingeben.'; cur.focus(); return; }
        send('settings:save:' + JSON.stringify({ current: cur ? cur.value() : '', pin: pin, minutes: Number(sel.value) }));
      };
      // Rechte von macOS: vorab erteilen statt mitten in einem Agenten-Lauf
      if (perms === false) { setTimeout(function () { if (cur) cur.focus(); }, 30); return; }
      var sec = el(c, 'div', 'lu-sec');
      var head = el(sec, 'div', 'lu-row');
      el(head, 'p', 'lu-h', 'Rechte von macOS').style.margin = '0';
      var all = el(head, 'button', '', 'Alle anfragen');
      all.onclick = function () { send('perm:all'); };
      el(sec, 'p', 'lu-sub', 'Agenten laufen unter luCode. Hier erteilte Rechte gelten auch für sie – so fragt macOS nicht erst mitten in einem Lauf nach.').style.margin = '6px 0 4px';
      var list = el(sec, 'div'); list.id = 'lu-perms';
      el(list, 'div', 'lu-perm').textContent = 'Wird geprüft …';
      permsAll = all;
      send('perm:list');
      setTimeout(function () { if (cur) cur.focus(); }, 30);
    },
    // Liste der Rechte von der App: [{id, name, detail, state, requestable}], busy = Abfrage läuft
    perms: function (items, busy) {
      var list = document.getElementById('lu-perms'); if (!list) return;
      list.textContent = '';
      if (permsAll) {
        permsAll.disabled = busy;
        permsAll.textContent = busy ? 'Wird abgefragt …' : 'Alle anfragen';
        permsAll.style.display = items.every(function (x) { return x.state === 'granted' || !x.requestable; }) ? 'none' : '';
      }
      items.forEach(function (x) {
        var r = el(list, 'div', 'lu-perm');
        el(r, 'span', 'lu-dot ' + x.state).title = x.state === 'granted' ? 'Erteilt' : x.state === 'denied' ? 'Nicht erteilt' : 'Noch nicht abgefragt';
        var i = el(r, 'div', 'i');
        el(i, 'div', 'n', x.name);
        el(i, 'div', 'd', x.detail);
        if (x.state === 'granted') { el(r, 'span', 'lu-ok', 'Erteilt'); return; }
        if (x.requestable) {
          var b = el(r, 'button', '', 'Anfragen'); b.disabled = busy;
          b.onclick = function () { send('perm:request:' + x.id); };
        }
        // Einmal abgelehnt fragt macOS nicht mehr nach – dann nur noch in den Systemeinstellungen
        if (x.state === 'denied') el(r, 'button', '', 'Einstellungen').onclick = function () { send('perm:open:' + x.id); };
      });
    },
    result: function (ok, msg) {
      var o = document.getElementById('lu-settings'); if (!o) return;
      if (ok) { o.remove(); return; }
      o.querySelector('.lu-err').textContent = msg;
    }
  };
})();
"#;

/// Nur im Hauptfenster: Titelleiste der App in der Oberfläche (Klasse `lu-mac` – die Kopfzeilen
/// lassen links Platz für die Ampel), Fenster an `[data-drag]` ziehen bzw. mit Doppelklick
/// zoomen, und native Mitteilungen für die Benachrichtigungen der Oberfläche.
pub const WINDOW_SCRIPT: &str = r#"
(function () {
  if (window.__luNative) return;
  function send(m) { try { window.ipc.postMessage(m); } catch (e) {} }
  function mark() { document.documentElement.classList.add('lu-mac'); }
  if (document.documentElement) mark(); else document.addEventListener('DOMContentLoaded', mark);
  // Klassen am <html> werden beim Themenwechsel neu gesetzt – Markierung erhalten
  new MutationObserver(function () { if (!document.documentElement.classList.contains('lu-mac')) mark(); })
    .observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
  var INTERACTIVE = 'button,a,input,select,textarea,label,[role=button],[contenteditable=true]';
  function dragZone(e) {
    if (e.button !== 0 || !e.target.closest || e.target.closest(INTERACTIVE)) return false;
    if (e.target.closest('[data-drag]')) return true;
    // Seiten ohne Kopfzeile (Start, Sperre): oberster Streifen
    return e.clientY < 32 && !document.querySelector('[data-drag]');
  }
  window.addEventListener('mousedown', function (e) {
    if (!dragZone(e)) return;
    if (e.detail === 2) { send('window:zoom'); return; }
    send('window:drag');
  }, true);
  window.__luNative = {
    notify: function (title, body) { send('notify:' + JSON.stringify({ title: String(title || ''), body: String(body || '') })); }
  };
})();
"#;

/// Nur im Hauptfenster, wenn Milchglas dahinter liegt (macOS Vibrancy, Windows Acryl bzw. Weichzeichnen):
/// `window.__luGlass`. Die Oberfläche tönt dann nur noch (`html.lu-glass`, abschaltbar unter
/// Einstellungen → Erscheinungsbild) – schon hier gesetzt, damit beim Laden nichts deckend aufblitzt.
pub const GLASS_SCRIPT: &str = r#"
(function () {
  window.__luGlass = true;
  try { if (localStorage.getItem('lu-glass') !== '1') return; } catch (e) { return; }
  function mark() { document.documentElement.classList.add('lu-glass'); }
  if (document.documentElement) mark(); else document.addEventListener('DOMContentLoaded', mark);
})();
"#;

/// Milchglas liess sich doch nicht anbringen: Oberfläche wieder deckend (Antwort auf `theme:`).
pub const NO_GLASS_SCRIPT: &str =
    "window.__luGlass = false; document.documentElement && document.documentElement.classList.remove('lu-glass');";

/// Hauptfenster unter Windows (ohne Titelleiste und Menü, wie am Mac): Klasse `lu-win` am
/// `<html>` (die Oberfläche lässt rechts oben Platz), rechts oben Minimieren/Maximieren/Schliessen
/// im Stil von Windows 11, Ziehen an Kopfzeilen (`[data-drag]`, sonst oberster Streifen),
/// Doppelklick dort maximiert, der obere Rand ändert die Grösse (links, rechts und unten macht
/// das der Fensterrahmen des Systems). Strg+, öffnet die Einstellungen.
/// Die App meldet maximiert/Vollbild über `window.__luWinState(max, full)`.
pub const WIN_SCRIPT: &str = r#"
(function () {
  if (window.__luWin) return;
  window.__luWin = true;
  function send(m) { try { window.ipc.postMessage(m); } catch (e) {} }
  var de = (navigator.language || '').toLowerCase().indexOf('de') === 0;
  var state = { max: false, full: false };
  function apply() {
    var c = document.documentElement.classList;
    if (!c.contains('lu-win')) c.add('lu-win');
    if (c.contains('lu-max') !== state.max) c.toggle('lu-max', state.max);
    if (c.contains('lu-full') !== state.full) c.toggle('lu-full', state.full);
    var b = document.getElementById('lu-cap-max');
    if (b) {
      b.textContent = state.max ? '' : '';
      b.title = state.max ? (de ? 'Verkleinern' : 'Restore') : (de ? 'Maximieren' : 'Maximize');
    }
  }
  window.__luWinState = function (max, full) { state.max = !!max; state.full = !!full; apply(); };
  if (document.documentElement) {
    apply();
    // Klassen am <html> werden beim Themenwechsel neu gesetzt – Markierung erhalten
    new MutationObserver(apply).observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
  }
  var css = '#lu-caption{position:fixed;top:0;right:0;z-index:2147483647;display:flex;height:32px;-webkit-user-select:none;user-select:none}'
    + '#lu-caption button{all:unset;box-sizing:border-box;width:46px;height:32px;display:grid;place-items:center;font:10px "Segoe Fluent Icons","Segoe MDL2 Assets";color:var(--foreground,#e5e5e5);cursor:default}'
    + '#lu-caption button:hover{background:rgba(128,128,128,.22)}'
    + '#lu-caption button:active{background:rgba(128,128,128,.34)}'
    + '#lu-caption button.lu-close:hover{background:#c42b1c;color:#fff}'
    + '#lu-caption button.lu-close:active{background:#b22a1b;color:#fff}'
    + '.lu-edge{position:fixed;z-index:2147483647}'
    + '#lu-edge-n{top:0;left:8px;right:8px;height:4px;cursor:n-resize}'
    + '#lu-edge-nw{top:0;left:0;width:8px;height:6px;cursor:nw-resize}'
    + '#lu-edge-ne{top:0;right:0;width:8px;height:6px;cursor:ne-resize}'
    + '.lu-max .lu-edge,.lu-full .lu-edge,.lu-full #lu-caption{display:none}';
  var EDGES = ['n', 'nw', 'ne'];
  function el(tag, id, cls) { var e = document.createElement(tag); e.id = id; if (cls) e.className = cls; return e; }
  function build() {
    if (!document.body || document.getElementById('lu-caption')) return;
    var st = el('style', 'lu-caption-css'); st.textContent = css;
    (document.head || document.documentElement).appendChild(st);
    var bar = el('div', 'lu-caption');
    [['lu-cap-min', '', '', de ? 'Minimieren' : 'Minimize', 'window:minimize'],
     ['lu-cap-max', '', '', de ? 'Maximieren' : 'Maximize', 'window:zoom'],
     ['lu-cap-close', 'lu-close', '', de ? 'Schliessen' : 'Close', 'window:close']].forEach(function (d) {
      var b = el('button', d[0], d[1]);
      b.type = 'button'; b.tabIndex = -1; b.textContent = d[2]; b.title = d[3];
      b.addEventListener('mousedown', function (e) { e.stopPropagation(); });
      b.addEventListener('dblclick', function (e) { e.stopPropagation(); });
      b.addEventListener('click', function (e) { e.preventDefault(); e.stopPropagation(); send(d[4]); });
      bar.appendChild(b);
    });
    document.body.appendChild(bar);
    EDGES.forEach(function (d) {
      var e = el('div', 'lu-edge-' + d, 'lu-edge');
      e.addEventListener('mousedown', function (ev) {
        if (ev.button !== 0) return;
        ev.preventDefault(); ev.stopPropagation();
        send('window:resize:' + d);
      });
      document.body.appendChild(e);
    });
    apply();
    // Immer zuoberst (auch über der Sperre): ans Ende von <body>, wenn dort etwas dazukommt
    new MutationObserver(function () {
      var last = document.body.lastElementChild;
      if (last && last.id !== 'lu-edge-ne') {
        document.body.appendChild(bar);
        EDGES.forEach(function (d) { var x = document.getElementById('lu-edge-' + d); if (x) document.body.appendChild(x); });
      }
    }).observe(document.body, { childList: true });
  }
  if (document.body) build(); else document.addEventListener('DOMContentLoaded', build);
  // Ziehen erst nach einer kleinen Bewegung (so bleibt der Doppelklick zum Maximieren erhalten)
  var INTERACTIVE = 'button,a,input,select,textarea,label,[role=button],[contenteditable=true],#lu-caption,.lu-edge';
  function dragZone(e) {
    if (e.button !== 0 || !e.target.closest || e.target.closest(INTERACTIVE)) return false;
    if (e.target.closest('[data-drag]')) return true;
    // Seiten ohne Kopfzeile (Start, Sperre): oberster Streifen
    return e.clientY < 32 && !document.querySelector('[data-drag]');
  }
  var press = null;
  window.addEventListener('mousedown', function (e) { press = dragZone(e) ? { x: e.screenX, y: e.screenY } : null; }, true);
  window.addEventListener('mousemove', function (e) {
    if (!press) return;
    if (!(e.buttons & 1)) { press = null; return; }
    if (Math.abs(e.screenX - press.x) + Math.abs(e.screenY - press.y) < 4) return;
    press = null;
    send('window:drag');
  }, true);
  window.addEventListener('mouseup', function () { press = null; }, true);
  window.addEventListener('dblclick', function (e) { if (dragZone(e)) send('window:zoom'); }, true);
  window.addEventListener('keydown', function (e) {
    if (e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey && e.key === ',') {
      e.preventDefault();
      if (window.__luOpenSettings) window.__luOpenSettings(); else send('settings:open');
    }
  }, true);
})();
"#;

/// Hauptfenster auf allen Systemen: soll sich wie eine native App anfühlen, nicht wie eine
/// Webseite – nichts ziehbar (Bilder, Symbole, Links), kein Browser-Kontextmenü (Neu laden,
/// Zurück; nur Eingabefelder behalten Kopieren/Einfügen), kein Neuladen oder Vor/Zurück per
/// Tastenkürzel oder Maustaste, keine Textauswahl auf Bedienelementen. Inhalte wie Chat und
/// Code bleiben auswählbar (Kopieren mit ⌘C).
pub const NATIVE_SCRIPT: &str = r#"
(function () {
  if (window.__luFeel) return;
  window.__luFeel = true;
  var css = 'img,svg,a,video,canvas{-webkit-user-drag:none;user-drag:none}'
    + 'button,[role=button],[role=menuitem],[role=tab],[role=option],label,nav,svg,img,[data-drag]{-webkit-user-select:none;user-select:none}'
    + 'a:not([href^="http"]){-webkit-touch-callout:none}';
  function style() {
    if (document.getElementById('lu-feel-css')) return;
    var s = document.createElement('style'); s.id = 'lu-feel-css'; s.textContent = css;
    (document.head || document.documentElement).appendChild(s);
  }
  if (document.documentElement) style(); else document.addEventListener('DOMContentLoaded', style);
  document.addEventListener('DOMContentLoaded', style);
  function editable(t) {
    return !!(t && t.closest && t.closest('input,textarea,select,[contenteditable=""],[contenteditable=true]'));
  }
  // Ziehen nur, wo die Oberfläche es ausdrücklich erlaubt (draggable="true", z. B. Tabs)
  window.addEventListener('dragstart', function (e) {
    var t = e.target;
    if (t && t.nodeType === 1 && t.closest && t.closest('[draggable="true"]')) return;
    if (editable(t)) return;
    e.preventDefault();
  }, true);
  // Browser-Kontextmenü nirgends – nur in Eingabefeldern (Kopieren/Einfügen). Eigene Menüs der
  // Oberfläche (z. B. Rechtsklick auf einen Thread) laufen vorher und bleiben. Ein Rechtsklick
  // markiert unter macOS das Wort darunter – das wird hier wieder aufgehoben.
  window.addEventListener('contextmenu', function (e) {
    if (editable(e.target)) return;
    e.preventDefault();
  }, true);
  window.addEventListener('mousedown', function (e) {
    if (e.button === 2 && !editable(e.target)) e.preventDefault();
  }, true);
  window.addEventListener('keydown', function (e) {
    var k = (e.key || '').toLowerCase(), mod = e.metaKey || e.ctrlKey;
    var block = k === 'f5' || k === 'browserrefresh' || k === 'browserback' || k === 'browserforward'
      || (mod && !e.altKey && (k === 'r' || k === 'p'))
      || (e.metaKey && (k === '[' || k === ']'))
      || (e.altKey && !mod && (k === 'arrowleft' || k === 'arrowright') && !editable(e.target))
      || (k === 'f3' || k === 'f7' || k === 'caretbrowsing')
      || (mod && e.shiftKey && (k === 'i' || k === 'j' || k === 'c') && !window.__luDevtools);
    if (block) { e.preventDefault(); }
  }, true);
  // Maustasten 4/5 (Zurück/Vor)
  ['mouseup', 'mousedown', 'auxclick'].forEach(function (t) {
    window.addEventListener(t, function (e) { if (e.button === 3 || e.button === 4) e.preventDefault(); }, true);
  });
  // Dateien, die neben eine Ablagefläche fallen, nicht als Seite öffnen
  window.addEventListener('dragover', function (e) { if (!e.defaultPrevented) e.preventDefault(); });
  window.addEventListener('drop', function (e) { if (!e.defaultPrevented) e.preventDefault(); });
})();
"#;
