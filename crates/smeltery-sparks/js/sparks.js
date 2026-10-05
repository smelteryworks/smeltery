/* Sparks client runtime (Smeltery), wire protocol 3. No dependencies, no build step. */
(function (G) {
  'use strict';
  var PROTOCOL = 3;
  var D = typeof document !== 'undefined' ? document : null;
  var hasOwn = Object.prototype.hasOwnProperty;
  var KEYS = { enter: 'Enter', escape: 'Escape', tab: 'Tab', space: ' ', up: 'ArrowUp', down: 'ArrowDown', left: 'ArrowLeft', right: 'ArrowRight' };

  // "wire:model.debounce.300ms" -> {name: "model", mods: ["debounce", "300ms"]}
  function parse(attr) {
    if (attr.slice(0, 5) !== 'wire:') return null;
    var p = attr.slice(5).split('.');
    return { name: p[0], mods: p.slice(1) };
  }
  // Every wire:<name>[.mods] attribute of el.
  function wires(el, name) {
    var out = [], a = el && el.attributes, i, p;
    for (i = 0; a && i < a.length; i++) {
      p = parse(a[i].name);
      if (p && p.name === name) out.push({ value: a[i].value, mods: p.mods });
    }
    return out;
  }
  function wire(el, name) { return wires(el, name)[0] || null; }
  function has(mods, m) { return mods.indexOf(m) >= 0; }
  function duration(mods, dflt) {
    for (var i = 0, m; i < mods.length; i++) {
      if ((m = /^(\d+)(ms|s)$/.exec(mods[i]))) return +m[1] * (m[2] === 's' ? 1000 : 1);
    }
    return dflt;
  }
  // "remove(5, 'a b')" -> {method: "remove", params: [5, "a b"]}
  function parseCall(s) {
    var m = /^\s*([\w$]+)\s*(?:\(([\s\S]*)\))?\s*$/.exec(s || '$refresh');
    return m ? { method: m[1], params: m[2] ? parseArgs(m[2]) : [] } : null;
  }
  function parseArgs(s) {
    var out = [], re = /\s*('(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"|[^,]+?)\s*(,|$)/g, m, t;
    s = s.trim();
    while (re.lastIndex < s.length && (m = re.exec(s)) && m[0]) {
      t = m[1].trim();
      if (t[0] === "'" || t[0] === '"') out.push(unescape(t.slice(1, -1)));
      else if (t === 'true' || t === 'false' || t === 'null') out.push(JSON.parse(t));
      else if (t !== '' && !isNaN(+t)) out.push(+t);
      else out.push(t);
      if (!m[2]) break;
    }
    return out;
  }
  // The escapes of a quoted argument, as JavaScript and JSON read them (what Mold's `json` filter writes).
  var ESC = { n: '\n', r: '\r', t: '\t', b: '\b', f: '\f', v: '\v', 0: '\0' };
  function unescape(s) {
    return s.replace(/\\(u[0-9a-fA-F]{4}|[\s\S])/g, function (m, c) {
      return c.length === 5 ? String.fromCharCode(parseInt(c.slice(1), 16)) : hasOwn.call(ESC, c) ? ESC[c] : c;
    });
  }
  // Where a redirect effect may send the browser: an http(s) URL (a path resolves against the page), else null.
  function redirectTarget(url, base) {
    if (typeof url !== 'string' || !url) return null;
    try { url = new URL(url, base); } catch (e) { return null; }
    return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null;
  }
  // Keys that would reach an object's prototype when assigned: never copied from server state.
  function unsafeKey(k) { return k === '__proto__' || k === 'constructor' || k === 'prototype'; }
  // The value at a wire:model name in the state; `form.title` is key `title` of field `form`. Own keys only.
  function lookup(data, path) {
    var parts = String(path).split('.'), v = data;
    for (var i = 0; i < parts.length; i++) {
      if (v === null || typeof v !== 'object' || unsafeKey(parts[i]) || !hasOwn.call(v, parts[i])) return { found: false };
      v = v[parts[i]];
    }
    return { found: true, value: v };
  }
  // GET /_sparks/stream with the stream tokens (`wire:stream`) of the subscribed roots; null without any.
  function streamUrl(roots) {
    var tokens = [];
    roots.forEach(function (el) { var t = el.getAttribute('wire:stream'); if (t && tokens.indexOf(t) < 0) tokens.push(t); });
    return tokens.length ? '/_sparks/stream?t=' + encodeURIComponent(tokens.join(',')) : null;
  }
  // The `$listen` call of a listen message: the message as the server signed it, sent back unchanged; null when it
  // does not have that shape.
  function listenCall(m) {
    var s = function (v) { return typeof v === 'string'; }, n = function (v) { return typeof v === 'number'; };
    if (!m || !s(m.target) || !s(m.channel) || !s(m.event) || !s(m.data) || !n(m.exp) || !n(m.seq) || !s(m.sig)) return null;
    return { method: '$listen', params: [m.channel, m.event, m.data, m.exp, m.seq, m.sig] };
  }
  // One stream message. `commit(root, call)` queues a call on an instance, `fire(name, detail)` a window event.
  // refresh: a `$refresh` of the instances named; event: a window event; listen: the instance's `$listen` call, then
  // the window event `anvil:<event>` with the event's data.
  function onMessage(m, roots, commit, fire) {
    var call, detail;
    if (!m || typeof m !== 'object') return;
    if (m.kind === 'refresh') {
      roots.forEach(function (el) {
        if (!m.target || el.getAttribute('wire:id') === m.target || el.getAttribute('wire:name') === m.target) commit(el, { method: '$refresh', params: [] });
      });
    } else if (m.kind === 'event') {
      fire(m.event, m.payload);
    } else if (m.kind === 'listen' && (call = listenCall(m))) {
      roots.forEach(function (el) { if (el.getAttribute('wire:id') === m.target) commit(el, call); });
      try { detail = JSON.parse(m.data); } catch (x) { return; }
      fire('anvil:' + m.event, detail);
    }
  }
  // When to reopen the page's stream: 5 s after the first failure, doubling up to 5 min while it keeps failing
  // (an idle page past its tokens' lifetime, a refused stream), back to 5 s once a stream opens; never after the
  // server said the session ended (`{"kind":"end"}`).
  function Reconnect() { this.failures = 0; this.stopped = false; }
  Reconnect.prototype.failed = function () {
    if (this.stopped) return null;
    this.failures += 1;
    return Math.min(5000 * Math.pow(2, this.failures - 1), 300000);
  };
  Reconnect.prototype.opened = function () { this.failures = 0; };
  Reconnect.prototype.end = function () { this.stopped = true; };
  function debounce(fn, ms) {
    var t;
    return function () { clearTimeout(t); t = setTimeout(fn, ms); };
  }

  // One request in flight per instance; updates and calls arriving meanwhile go out together next. `$listen` calls
  // go out in requests of their own: the server refuses a whole request for one refused listen message (expired,
  // out of order, access lost), and the visitor's typed values and clicks must never be lost with it.
  function Queue(send) { this.send = send; this.busy = false; this.dirty = false; this.updates = {}; this.calls = []; this.listens = []; }
  Queue.prototype.set = function (field, value) { this.updates[field] = value; };
  Queue.prototype.commit = function (call) {
    if (call) (call.method === '$listen' ? this.listens : this.calls).push(call);
    this.dirty = true;
    return this.flush();
  };
  Queue.prototype.flush = function () {
    if (this.busy || !this.dirty) return Promise.resolve();
    var q = this, p;
    if (q.listens.length) {
      p = { updates: {}, calls: q.listens };
      q.listens = [];
      q.dirty = q.calls.length > 0 || Object.keys(q.updates).length > 0;
    } else {
      p = { updates: q.updates, calls: q.calls };
      q.updates = {}; q.calls = []; q.dirty = false;
    }
    q.busy = true;
    function done() { q.busy = false; return q.flush(); }
    return Promise.resolve().then(function () { return q.send(p); }).then(done, done);
  };

  // ---- DOM morph: keyed by wire:key / wire:id / id, then tag at the same position.
  function isEl(n) { return n && n.nodeType === 1; }
  function key(n) { return isEl(n) ? n.getAttribute('wire:key') || n.getAttribute('wire:id') || n.getAttribute('id') || null : null; }
  function same(a, b) { return a.nodeType === b.nodeType && (!isEl(a) || a.tagName === b.tagName); }
  // The next sibling, past the elements Alpine's x-for / x-if rendered after a <template> (they are Alpine's).
  function next(n) { return (n._x_lastRenderedEl || n).nextSibling; }
  // Alpine 3.13+ on the page: its cloneNode renders the live Alpine state into the server's HTML before the diff.
  function alp() { var A = G.Alpine; return A && A.cloneNode && A.closestDataStack ? A : null; }
  // Set while cloneNode evaluates bindings in the new HTML: $spark sends nothing then (no request per morph).
  var morphing = false, autofocus = null;
  function morph(from, to, active) {
    var A = alp(), f;
    if (A && isEl(from) && !from._x_dataStack) to._x_dataStack = A.closestDataStack(from);
    morphing = true; autofocus = null;
    try { patch(from, to, active, A); } finally { morphing = false; f = autofocus; autofocus = null; }
    return f;
  }
  // A response's re-render: morph, boot (fills wire:model inputs, so before any focus), then autofocus. Browsers
  // ignore autofocus on inserted nodes, so the first new one gets focus, unless focus (before the morph) was on an
  // element other than <body> outside this Spark (decided before the morph, which may remove that element). With
  // Alpine, after its observer has initialised the new nodes.
  function render(el, to, active, boot) {
    // The focused element's nearest Spark, before the morph: a child Spark counts as another Spark.
    var out = isEl(active) && !/^(BODY|HTML)$/.test(active.tagName) && rootOf(active) !== el, f;
    f = morph(el, to, active);
    boot(el);
    if (!f || !f.focus || out) return;
    if (G.Alpine && typeof queueMicrotask === 'function') queueMicrotask(function () { f.focus(); }); else f.focus();
  }
  // The first element with autofocus in a newly inserted subtree (once per morph).
  function seek(t) {
    if (autofocus || !isEl(t)) return;
    if (t.hasAttribute('autofocus')) { autofocus = t; return; }
    walk(t, function (el) { if (autofocus) return false; if (el.hasAttribute('autofocus')) { autofocus = el; return false; } });
  }
  function patch(from, to, active, A) {
    if (!isEl(from)) { if (from.nodeValue !== to.nodeValue) from.nodeValue = to.nodeValue; return; }
    var i, a, tag = from.tagName;
    if (A) { if (from.__sparks) to.__sparks = from.__sparks; A.cloneNode(from, to); }
    if (!from._x_transitioning) {
      for (i = from.attributes.length - 1; i >= 0; i--) {
        a = from.attributes[i];
        if (!to.hasAttribute(a.name)) from.removeAttribute(a.name);
      }
      for (i = 0; i < to.attributes.length; i++) {
        a = to.attributes[i];
        if (from.getAttribute(a.name) === a.value) continue;
        // setAttribute refuses names like Alpine's "@click"; a copy of the parsed attribute node does not.
        try { from.setAttribute(a.name, a.value); } catch (e) { from.setAttributeNode(a.cloneNode(true)); }
      }
    }
    if (tag !== 'TEXTAREA') children(from, to, active, A);
    if ((tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') && from !== active && !wire(from, 'model') && !from._x_model) {
      if (from.type === 'checkbox' || from.type === 'radio') from.checked = to.hasAttribute('checked');
      else if (from.type !== 'file') {
        var v = tag === 'TEXTAREA' ? to.textContent : to.getAttribute('value') || '';
        if (from.value !== v) from.value = v;
      }
    }
  }
  function children(from, to, active, A) {
    var keyed = {}, cur, n, k, m, t, list = [];
    for (n = from.firstChild; n; n = next(n)) if ((k = key(n)) !== null) keyed[k] = n;
    for (n = to.firstChild; n; n = n.nextSibling) list.push(n);
    cur = from.firstChild;
    for (var i = 0; i < list.length; i++) {
      t = list[i]; k = key(t); m = null;
      if (k !== null) { m = keyed[k] || null; delete keyed[k]; } else if (cur && key(cur) === null && same(cur, t)) m = cur;
      if (m && !same(m, t)) m = null;
      if (m) {
        if (m === cur) cur = next(cur); else from.insertBefore(m, cur);
        // A child instance's placeholder keeps the existing child untouched.
        if (!(isEl(t) && t.getAttribute('wire:id') && !t.hasAttribute('wire:snapshot'))) patch(m, t, active, A);
      } else { from.insertBefore(t, cur); seek(t); }
    }
    while (cur) { n = next(cur); from.removeChild(cur); cur = n; }
  }

  // ---- tree helpers (no selectors, so they also run on a minimal DOM)
  function walk(node, fn) {
    for (var n = node.firstChild; n; n = n.nextSibling) if (isEl(n) && fn(n) !== false) walk(n, fn);
  }
  function rootOf(el) {
    while (el && !(isEl(el) && el.hasAttribute('wire:id'))) el = el.parentNode;
    return el || null;
  }
  // The elements of this instance (not of child instances), the root included.
  function own(root, fn) {
    fn(root);
    walk(root, function (el) { if (el.hasAttribute('wire:id')) return false; fn(el); });
  }

  // ---- values
  function getVal(root, el, field) {
    if (el.type === 'checkbox') {
      var data = state(root).data || {};
      if (!Array.isArray(lookup(data, field).value)) return el.checked;
      var vals = [];
      own(root, function (x) { var w = wire(x, 'model'); if (w && w.value === field && x.type === 'checkbox' && x.checked) vals.push(x.value); });
      return vals;
    }
    if (el.multiple && el.options) return [].filter.call(el.options, function (o) { return o.selected; }).map(function (o) { return o.value; });
    return el.value;
  }
  function setVal(el, v) {
    if (el.type === 'file') return;
    if (el.type === 'checkbox') el.checked = Array.isArray(v) ? v.map(String).indexOf(el.value) >= 0 : !!v;
    else if (el.type === 'radio') el.checked = String(v) === el.value;
    else if (el.multiple && el.options) [].forEach.call(el.options, function (o) { o.selected = (v || []).map(String).indexOf(o.value) >= 0; });
    else el.value = v === null || v === undefined ? '' : String(v);
  }
  function state(root) {
    try { return JSON.parse(root.getAttribute('wire:snapshot')); } catch (e) { return {}; }
  }
  // Inputs bound with wire:model show the instance's state (except the one being typed in).
  function fill(root, active) {
    var data = state(root).data;
    if (!data) return;
    own(root, function (el) {
      var w = wire(el, 'model'), found = w && el !== active ? lookup(data, w.value) : null;
      if (found && found.found) setVal(el, found.value);
    });
  }

  // ---- loading states
  function loading(root, targets, on) {
    own(root, function (el) {
      var ws = wires(el, 'loading');
      if (!ws.length) return;
      var t = el.getAttribute('wire:target');
      if (t && !t.split(',').some(function (x) { return targets.indexOf(x.trim()) >= 0; })) return;
      ws.forEach(function (w) {
        var m = w.mods;
        if (m[0] === 'class') {
          w.value.split(/\s+/).filter(Boolean).forEach(function (c) { el.classList[on !== (m[1] === 'remove') ? 'add' : 'remove'](c); });
        } else if (m[0] === 'attr') {
          if (on) el.setAttribute(w.value, ''); else el.removeAttribute(w.value);
        } else if (m[0] === 'remove') el.style.display = on ? 'none' : '';
        else el.style.display = on ? (has(m, 'flex') ? 'flex' : has(m, 'block') ? 'block' : 'inline-block') : '';
      });
    });
  }

  // ---- instances and the Alpine.js bridge ($spark). Requests go through the same queue as wire:* attributes,
  // so Alpine reaches exactly what the server allows (model fields, actions with their guards).
  var hooked = null, warned = {};
  function warn(m) { if (!warned[m]) { warned[m] = 1; console.warn('[sparks] ' + m); } }
  function copy(v) { return v !== null && typeof v === 'object' ? JSON.parse(JSON.stringify(v)) : v; }
  function comp(root) {
    if (!root) return null;
    if (!root.__sparks) root.__sparks = { el: root, q: new Queue(function (p) { return api.send(root, p); }) };
    return root.__sparks;
  }
  // Records a field update for the next request; $spark shows it at once.
  function record(c, field, v) {
    c.q.set(field, copy(v));
    if (c.store && !unsafeKey(field)) c.store[field] = copy(v);
  }
  // Queues an action call, except while the morph renders bindings (a binding would call it on every morph).
  function act(c, m, params) {
    if (morphing) { warn((m ? '$spark.' + m + '()' : 'a live $spark update') + ' called while rendering; actions run from events, not bindings'); return Promise.resolve(); }
    return c.q.commit(m === null ? undefined : { method: m, params: params });
  }
  // Sets a field: recorded for the next request (deferred) or sent now (live).
  function put(c, field, v, live) {
    record(c, field, v);
    return live ? act(c, null) : Promise.resolve();
  }
  // wire:model: the input's value into the next request (live, debounced, on blur or change), or an upload.
  function model(e, kind) {
    var el = e.target, w = wire(el, 'model'), root = rootOf(el), c = w && comp(root);
    if (!c) return;
    if (el.type === 'file') { if (kind === 'change') api.upload(root, el, w.value); return; }
    var m = w.mods, live = has(m, 'live') || has(m, 'debounce');
    record(c, w.value, getVal(root, el, w.value));
    if (live && kind === 'input') {
      if (!el.__deb) el.__deb = debounce(function () { c.q.commit(); }, duration(m, 150));
      el.__deb();
    } else if ((has(m, 'blur') && kind === 'blur') || (has(m, 'change') && kind === 'change')) c.q.commit();
  }
  // After a response: the new state into the reactive copy Alpine reads, updates not sent yet laid over it.
  function sync(root, snapshot) {
    var c = root && root.__sparks, data, k;
    if (!c || !c.store) return;
    try { data = JSON.parse(snapshot).data || {}; } catch (e) { return; }
    for (k in c.q.updates) if (hasOwn.call(c.q.updates, k) && !unsafeKey(k)) data[k] = c.q.updates[k];
    for (k in data) if (hasOwn.call(data, k) && !unsafeKey(k) && JSON.stringify(c.store[k]) !== JSON.stringify(data[k])) c.store[k] = data[k];
  }
  // x-data="{ open: $spark.$entangle('open') }": two-way between an Alpine property and a field.
  function entangle(A, c, f, live, u) {
    var s = c.store;
    return A.interceptor(function (init, get, set, path, key, cleanup) {
      var stop = A.entangle({ get: function () { return s[f]; }, set: function (v) { put(c, f, v, live); } }, { get: get, set: set });
      if ((cleanup = cleanup || (u && u.cleanup))) cleanup(stop);
      return copy(s[f]);
    }, function (o) { Object.defineProperty(o, 'live', { get: function () { live = true; return o; } }); })(s[f]);
  }
  // The first copy of the state for $spark: the state's own keys, then the queued updates; never a prototype key.
  function seed(data, updates) {
    var out = {};
    [data, updates].forEach(function (o) { for (var k in o) if (hasOwn.call(o, k) && !unsafeKey(k)) out[k] = o[k]; });
    return out;
  }
  // $spark: the instance around el. Fields read from a reactive copy of the state; other names call actions.
  function bridge(A, el, u) {
    var root = rootOf(el), c = comp(root), x;
    if (!c) return undefined;
    if (!c.store) c.store = A.reactive(seed(copy(state(root).data || {}), copy(c.q.updates)));
    function call(m, params) { return act(c, m, params); }
    x = {
      $id: root.getAttribute('wire:id'), $name: root.getAttribute('wire:name'), $el: root,
      $get: function (f) { return c.store[f]; },
      $set: function (f, v, live) { return put(c, f, v, live !== false); },
      $call: function (m) { return call(m, [].slice.call(arguments, 1)); },
      $refresh: function () { return call('$refresh', []); },
      $commit: function () { return act(c, null); },
      $entangle: function (f, live) { return entangle(A, c, f, !!live, u); }
    };
    // Names $spark keeps for itself: not actions, not fields to set.
    function kept(k) { return typeof k !== 'string' || k === 'then' || k === 'toJSON' || k.slice(0, 2) === '__' || unsafeKey(k) || hasOwn.call(x, k); }
    return new Proxy(x, {
      get: function (t, k) {
        if (kept(k)) return hasOwn.call(x, k) ? x[k] : undefined;
        if (k in c.store) return c.store[k];
        return function () { return call(k, [].slice.call(arguments)); };
      },
      set: function (t, k, v) { if (!kept(k)) put(c, k, v, false); return true; }
    });
  }
  // Registers $spark with Alpine 3.13+ (once per Alpine object).
  function alpine(A) {
    if (!A || !A.magic || A === hooked) return false;
    if (!(A.interceptor && A.entangle && A.cloneNode && A.closestDataStack)) { warn('$spark needs Alpine.js 3.13 or newer'); return false; }
    hooked = A;
    A.magic('spark', function (el, u) { return bridge(A, el, u); });
    return true;
  }

  var api = { PROTOCOL: PROTOCOL, redirectTarget: redirectTarget, streamUrl: streamUrl, listenCall: listenCall, onMessage: onMessage, Reconnect: Reconnect, parse: parse, wires: wires, duration: duration, parseCall: parseCall, debounce: debounce, Queue: Queue, morph: morph, fill: fill, getVal: getVal, setVal: setVal, loading: loading, rootOf: rootOf, comp: comp, sync: sync, alpine: alpine, model: model, render: render };
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (!D) return;

  // ---- browser wiring
  function csrf() { var m = D.querySelector('meta[name="csrf-token"]'); return m ? m.content : ''; }
  function find(id) { var r = null; walk(D.body, function (el) { if (el.getAttribute('wire:id') === id) { r = el; return false; } }); return r; }
  function failed(r) { console.error('[sparks] request failed: ' + r.status); }
  function send(root, p) {
    var targets = Object.keys(p.updates).concat(p.calls.map(function (c) { return c.method; }));
    loading(root, targets, true);
    return fetch('/_sparks/update', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json', 'X-CSRF-TOKEN': csrf(), 'X-Sparks': '1' },
      body: JSON.stringify({ v: PROTOCOL, components: [{ snapshot: root.getAttribute('wire:snapshot'), updates: p.updates, calls: p.calls }] })
    }).then(function (r) {
      if (r.status === 419) return location.reload();
      if (!r.ok) return failed(r);
      return r.json().then(function (j) { j.components.forEach(apply); });
    }).catch(function (e) { console.error('[sparks]', e); }).then(function () { loading(root, targets, false); });
  }
  function apply(res) {
    var fx = res.effects || {}, el = find(res.id), tpl, to;
    if (res.error) {
      // This component failed after other components of the same request ran (their results are applied).
      if (res.error.status === 419) return location.reload();
      return console.error('[sparks] request failed: ' + res.error.status + ' ' + res.error.message);
    }
    if (fx.redirect) {
      if ((to = redirectTarget(fx.redirect, location.href))) location.href = to;
      else console.error('[sparks] refused to follow a redirect that is not an http(s) URL');
      return;
    }
    if (el) {
      sync(el, res.snapshot);
      tpl = D.createElement('template');
      tpl.innerHTML = res.html;
      if (tpl.content.firstElementChild) render(el, tpl.content.firstElementChild, D.activeElement, boot);
      else boot(el);
    }
    (fx.dispatches || []).forEach(function (d) { G.dispatchEvent(new CustomEvent(d.event, { detail: d.payload })); });
  }
  function call(el, expr) {
    var c = comp(rootOf(el)), p = parseCall(expr);
    if (c && p) c.q.commit(p);
  }
  function upload(root, el, field) {
    var f = el.files && el.files[0], enc = encodeURIComponent;
    if (!f) return;
    loading(root, [field], true);
    fetch('/_sparks/upload?component=' + enc(root.getAttribute('wire:name')) + '&field=' + enc(field) + '&name=' + enc(f.name), {
      method: 'POST', headers: { 'Content-Type': f.type || 'application/octet-stream', Accept: 'application/json', 'X-CSRF-TOKEN': csrf() }, body: f
    }).then(function (r) {
      if (r.status === 419) return location.reload();
      if (!r.ok) return failed(r);
      return r.json().then(function (j) { var c = comp(root); c.q.set(field, j.token); c.q.commit(); });
    }).catch(function (e) { console.error('[sparks]', e); }).then(function () { loading(root, [field], false); });
  }
  function boot(node) {
    var active = D.activeElement;
    if (node.hasAttribute('wire:id')) fill(node, active);
    walk(node, function (el) {
      if (el.hasAttribute('wire:id')) fill(el, active);
      var w = wire(el, 'poll');
      if (w && !el.__poll) {
        el.__poll = setInterval(function () {
          var now = wire(el, 'poll');
          if (!el.isConnected || !now) { clearInterval(el.__poll); el.__poll = null; return; }
          if (!D.hidden) call(el, now.value);
        }, duration(w.mods, 2000));
      }
    });
  }
  function stream() {
    var roots = [];
    walk(D.body, function (el) { if (el.getAttribute('wire:stream')) roots.push(el); });
    if (!roots.length || !G.EventSource) return;
    var lost = false, retry = new Reconnect();
    function commit(el, c) { comp(el).q.commit(c); }
    function fire(name, detail) { G.dispatchEvent(new CustomEvent(name, { detail: detail })); }
    // The tokens come from the latest renders, so a stream the server ended (expired tokens) reopens with fresh ones:
    // the browser's own retry would send the old tokens again.
    function open() {
      var url = streamUrl(roots), es;
      if (!url) return;
      es = new EventSource(url);
      es.onerror = function () {
        var wait = retry.failed();
        lost = true;
        es.close();
        if (wait !== null) setTimeout(open, wait);
      };
      es.onopen = function () { retry.opened(); if (lost) { lost = false; onMessage({ kind: 'refresh' }, roots, commit, fire); } };
      es.onmessage = function (e) {
        var m;
        try { m = JSON.parse(e.data); } catch (x) { return; }
        if (m && m.kind === 'end') { retry.end(); es.close(); return; }
        onMessage(m, roots, commit, fire);
      };
    }
    open();
  }
  function start() {
    var css = D.createElement('style');
    css.textContent = '[wire\\:loading],[wire\\:loading\\.flex],[wire\\:loading\\.block]{display:none}';
    D.head.appendChild(css);
    D.addEventListener('click', function (e) {
      for (var el = e.target; isEl(el); el = el.parentNode) {
        var w = wire(el, 'click');
        if (w) { if (has(w.mods, 'prevent')) e.preventDefault(); call(el, w.value); return; }
      }
    });
    D.addEventListener('submit', function (e) {
      var w = wire(e.target, 'submit');
      if (w) { e.preventDefault(); call(e.target, w.value); }
    });
    D.addEventListener('keydown', function (e) {
      for (var el = e.target; isEl(el); el = el.parentNode) {
        var hit = wires(el, 'keydown').filter(function (w) { var k = w.mods[0]; return !k || KEYS[k] === e.key || k === String(e.key).toLowerCase(); })[0];
        if (hit) { if (has(hit.mods, 'prevent')) e.preventDefault(); call(el, hit.value); return; }
      }
    });
    D.addEventListener('input', function (e) { model(e, 'input'); });
    D.addEventListener('change', function (e) { model(e, 'change'); });
    D.addEventListener('focusout', function (e) { model(e, 'blur'); });
    boot(D.body);
    stream();
  }
  api.send = send;
  api.upload = upload;
  // Alpine loaded after this script (both deferred) fires alpine:init from Alpine.start(), before it walks the page.
  alpine(G.Alpine);
  D.addEventListener('alpine:init', function () { alpine(G.Alpine); });
  if (D.readyState === 'loading') D.addEventListener('DOMContentLoaded', start); else start();
})(typeof window !== 'undefined' ? window : globalThis);
