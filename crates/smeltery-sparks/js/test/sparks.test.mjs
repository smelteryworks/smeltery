// Tests of the Sparks client runtime's logic: `node --test crates/smeltery-sparks/js/test`.
import { test, mock } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { h, html, Element } from './dom.mjs';
import { fakeAlpine } from './alpine.mjs';

const S = createRequire(import.meta.url)('../sparks.js');

test('attribute names and modifiers', () => {
  assert.deepEqual(S.parse('wire:model.live.debounce.300ms'), { name: 'model', mods: ['live', 'debounce', '300ms'] });
  assert.deepEqual(S.parse('wire:click'), { name: 'click', mods: [] });
  assert.equal(S.parse('class'), null);
  assert.equal(S.duration(['debounce', '300ms'], 150), 300);
  assert.equal(S.duration(['5s'], 2000), 5000);
  assert.equal(S.duration(['live'], 150), 150);
  const el = h('input', { 'wire:keydown.enter': 'save', 'wire:keydown.escape': 'cancel', 'wire:model': 'q' });
  assert.deepEqual(S.wires(el, 'keydown').map((w) => [w.value, w.mods]), [['save', ['enter']], ['cancel', ['escape']]]);
});

test('action calls and their parameters', () => {
  assert.deepEqual(S.parseCall('increment'), { method: 'increment', params: [] });
  assert.deepEqual(S.parseCall('remove(5, \'a, b\', true, null, "q\\"x", -1.5)'), {
    method: 'remove',
    params: [5, 'a, b', true, null, 'q"x', -1.5],
  });
  assert.deepEqual(S.parseCall('f( )'), { method: 'f', params: [] });
  assert.deepEqual(S.parseCall(''), { method: '$refresh', params: [] });
  assert.equal(S.parseCall('not a call!'), null);
});

// Mocked timers for a test, or an explicit skip. Node >= 20.4 takes `enable({ apis })`. The older MockTimers
// (Node 18.19, 20.0-20.3) takes only `enable([...])` and rejects the object form with ERR_INVALID_ARG_TYPE; its
// clearTimeout is broken (`#clearTimer(position)` passes the timer id to `executionQueue.removeAt` as if it were a heap
// position: lib/internal/test_runner/mock/mock_timers.js:117-118 in v18.19.1), so a cleared timeout still fires.
// Tests that clear timeouts are skipped there, with a message, rather than run against that bug.
const OLD_MOCK_TIMERS = 'skipped: this Node has the MockTimers API whose clearTimeout does not cancel (Node < 20.4)';

function enableTimeouts(t) {
  try {
    t.mock.timers.enable({ apis: ['setTimeout'] });
    return true;
  } catch (e) {
    if (e?.code !== 'ERR_INVALID_ARG_TYPE') throw e;
    t.skip(OLD_MOCK_TIMERS);
    return false;
  }
}

test('on the old MockTimers API the timer tests are skipped with a message, not run', () => {
  // A model of Node 18.19: the object form is refused (and its clearTimeout would not cancel).
  const calls = [];
  const skips = [];
  const node18 = {
    skip: (msg) => skips.push(msg),
    mock: {
      timers: {
        enable(arg) {
          calls.push(arg);
          if (!Array.isArray(arg)) {
            const e = new TypeError('The "timers" argument must be an instance of Array');
            e.code = 'ERR_INVALID_ARG_TYPE';
            throw e;
          }
        },
      },
    },
  };
  assert.equal(enableTimeouts(node18), false);
  assert.deepEqual(calls, [{ apis: ['setTimeout'] }], 'the broken array form is never enabled');
  assert.deepEqual(skips, [OLD_MOCK_TIMERS]);
});

test('debounce runs once after the quiet time', (t) => {
  if (!enableTimeouts(t)) return;
  let n = 0;
  const f = S.debounce(() => n++, 300);
  f(); t.mock.timers.tick(200); f(); t.mock.timers.tick(200);
  assert.equal(n, 0);
  t.mock.timers.tick(100);
  assert.equal(n, 1);
});

test('one request in flight; later updates and calls go out together', async () => {
  const sent = [];
  let release;
  const q = new S.Queue((p) => { sent.push(p); return new Promise((r) => { release = r; }); });
  q.set('title', 'a');
  const first = q.commit({ method: 'save', params: [] });
  await Promise.resolve();
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0], { updates: { title: 'a' }, calls: [{ method: 'save', params: [] }] });
  // While busy: queued, not sent.
  q.set('title', 'b');
  q.commit({ method: 'inc', params: [] });
  q.commit({ method: 'inc', params: [1] });
  await Promise.resolve();
  assert.equal(sent.length, 1);
  release();
  await new Promise((r) => setImmediate(r));
  assert.equal(sent.length, 2);
  assert.deepEqual(sent[1], { updates: { title: 'b' }, calls: [{ method: 'inc', params: [] }, { method: 'inc', params: [1] }] });
  release();
  await first; // the commit's promise settles once the queue is drained
  // A failing request does not block the queue.
  const failing = new S.Queue(() => Promise.reject(new Error('down')));
  await failing.commit({ method: 'a', params: [] });
  assert.equal(failing.busy, false);
  // Deferred updates wait for a commit.
  const lazy = []; const q2 = new S.Queue((p) => { lazy.push(p); });
  q2.set('x', 1);
  await Promise.resolve();
  assert.equal(lazy.length, 0);
  await q2.commit();
  assert.deepEqual(lazy, [{ updates: { x: 1 }, calls: [] }]);
});

// Sweep W6-02: one refused `$listen` answers 403 for its whole request, so it never shares one with the visitor's
// typed values and clicks (they were lost with it).
test('$listen calls go out in requests of their own', async () => {
  const sent = [];
  let release;
  const q = new S.Queue((p) => { sent.push(p); return new Promise((r) => { release = r; }); });
  const listen = { method: '$listen', params: ['news', 'Posted', '{}', 1, 2, 's'] };
  q.set('title', 'a');
  const done = q.commit({ method: 'save', params: [] });
  await Promise.resolve();
  // While busy: a listen message, more input and another listen message arrive.
  q.commit(listen);
  q.set('title', 'b');
  q.commit({ method: 'inc', params: [] });
  q.commit({ ...listen, params: ['news', 'Posted', '{}', 1, 3, 's'] });
  release();
  await new Promise((r) => setImmediate(r));
  assert.equal(sent.length, 2);
  assert.deepEqual(sent[1], { updates: {}, calls: [listen, { ...listen, params: ['news', 'Posted', '{}', 1, 3, 's'] }] });
  release();
  await new Promise((r) => setImmediate(r));
  assert.equal(sent.length, 3);
  assert.deepEqual(sent[2], { updates: { title: 'b' }, calls: [{ method: 'inc', params: [] }] });
  release();
  await done;
  // A listen message alone sends no input; input alone sends no listen call.
  const alone = []; const q2 = new S.Queue((p) => { alone.push(p); });
  await q2.commit(listen);
  assert.deepEqual(alone, [{ updates: {}, calls: [listen] }]);
  // Every request carries at least one call or update.
  assert.ok(sent.concat(alone).every((p) => p.calls.length || Object.keys(p.updates).length));
});

test('morph updates text and attributes in place', () => {
  const p = h('p', { class: 'a', title: 'x' }, 'old');
  const from = h('div', {}, p);
  S.morph(from, h('div', {}, h('p', { class: 'b' }, 'new')));
  assert.equal(from.firstChild, p, 'the same element');
  assert.equal(html(from), '<div><p class="b">new</p></div>');
});

test('morph matches keyed children and keeps their identity', () => {
  const a = h('li', { 'wire:key': 'a' }, 'A');
  const b = h('li', { 'wire:key': 'b' }, 'B');
  const c = h('li', { 'wire:key': 'c' }, 'C');
  const from = h('ul', {}, a, b, c);
  S.morph(from, h('ul', {}, h('li', { 'wire:key': 'c' }, 'C2'), h('li', { 'wire:key': 'n' }, 'N'), h('li', { 'wire:key': 'a' }, 'A')));
  assert.equal(html(from), '<ul><li wire:key="c">C2</li><li wire:key="n">N</li><li wire:key="a">A</li></ul>');
  assert.equal(from.childNodes[0], c);
  assert.equal(from.childNodes[2], a);
  assert.equal(b.parentNode, null, 'removed');
  // ids work as keys too; unkeyed nodes match by tag and position.
  const x = h('span', { id: 'x' }, '1');
  const from2 = h('div', {}, h('b', {}, 'b'), x);
  S.morph(from2, h('div', {}, h('span', { id: 'x' }, '2'), h('i', {}, 'i')));
  assert.equal(html(from2), '<div><span id="x">2</span><i>i</i></div>');
  assert.equal(from2.firstChild, x);
});

test('morph keeps the value being typed and child instances', () => {
  const typing = h('input', { name: 'q', value: 'server' });
  typing.value = 'user typing';
  const other = h('input', { name: 'o', value: 'old' });
  const child = h('div', { 'wire:id': 'kid', 'wire:snapshot': '{}' }, h('span', {}, 'child state'));
  const from = h('div', { 'wire:id': 'root' }, typing, other, child);
  const to = h('div', { 'wire:id': 'root' }, h('input', { name: 'q', value: 'fresh' }), h('input', { name: 'o', value: 'new' }), h('div', { 'wire:id': 'kid' }));
  S.morph(from, to, typing);
  assert.equal(typing.value, 'user typing', 'focused input keeps its value');
  assert.equal(other.value, 'new');
  assert.equal(from.childNodes[2], child);
  assert.equal(html(child), '<div wire:id="kid" wire:snapshot="{}"><span>child state</span></div>', 'placeholder keeps the child');
  // A fresh render of a child (with a snapshot) is morphed.
  S.morph(from, h('div', { 'wire:id': 'root' }, h('input', { name: 'q' }), h('input', { name: 'o' }), h('div', { 'wire:id': 'kid', 'wire:snapshot': '{"v":1}' }, h('span', {}, 'new child'))), typing);
  assert.equal(html(child), '<div wire:id="kid" wire:snapshot="{"v":1}"><span>new child</span></div>');
});

test('fill puts the state into wire:model inputs, not into child instances', () => {
  const snap = JSON.stringify({ v: 1, data: { title: 'Hi', done: true, tags: ['a'], n: null } });
  const title = h('input', { 'wire:model': 'title' });
  const done = h('input', { type: 'checkbox', 'wire:model': 'done' });
  const tagA = h('input', { type: 'checkbox', value: 'a', 'wire:model': 'tags' });
  const tagB = h('input', { type: 'checkbox', value: 'b', 'wire:model': 'tags' });
  const n = h('input', { 'wire:model': 'n', value: 'x' });
  const childInput = h('input', { 'wire:model': 'title' });
  const root = h('div', { 'wire:id': 'r', 'wire:snapshot': snap }, title, done, tagA, tagB, n, h('div', { 'wire:id': 'c', 'wire:snapshot': '{}' }, childInput));
  S.fill(root, null);
  assert.equal(title.value, 'Hi');
  assert.equal(done.checked, true);
  assert.equal(tagA.checked, true);
  assert.equal(tagB.checked, false);
  assert.equal(n.value, '');
  assert.equal(childInput.value, '', 'the child instance fills itself');
  title.value = 'typing';
  S.fill(root, title);
  assert.equal(title.value, 'typing');
  // Reading values back.
  tagB.checked = true;
  assert.deepEqual(S.getVal(root, tagA, 'tags'), ['a', 'b']);
  assert.equal(S.getVal(root, done, 'done'), true);
  assert.equal(S.getVal(root, title, 'title'), 'typing');
  assert.equal(S.rootOf(tagA), root);
  assert.equal(S.rootOf(childInput).getAttribute('wire:id'), 'c');
});

test('loading states follow targets', () => {
  const spinner = h('span', { 'wire:loading': '' });
  const label = h('span', { 'wire:loading.remove': '' });
  const btn = h('button', { 'wire:loading.attr': 'disabled', 'wire:loading.class': 'opacity-50 busy', 'wire:target': 'save' });
  const other = h('span', { 'wire:loading': '', 'wire:target': 'delete' });
  const root = h('div', { 'wire:id': 'r' }, spinner, label, btn, other);
  S.loading(root, ['save'], true);
  assert.equal(spinner.style.display, 'inline-block');
  assert.equal(label.style.display, 'none');
  assert.equal(btn.hasAttribute('disabled'), true);
  assert.equal(btn.getAttribute('class'), 'opacity-50 busy');
  assert.equal(other.style.display, '', 'another target');
  S.loading(root, ['save'], false);
  assert.equal(spinner.style.display, '');
  assert.equal(label.style.display, '');
  assert.equal(btn.hasAttribute('disabled'), false);
  assert.equal(btn.getAttribute('class'), '');
});

test('the runtime speaks protocol 3', () => {
  assert.equal(S.PROTOCOL, 3);
});

test('morph copies attribute names that setAttribute refuses (Alpine @click)', () => {
  const btn = h('button', { '@click': 'open = 1' }, 'x');
  const from = h('div', {}, btn);
  S.morph(from, h('div', {}, h('button', { '@click': 'open = 2', '@keyup.enter': 'go' }, 'x')));
  assert.equal(from.firstChild, btn);
  assert.equal(html(from), '<div><button @click="open = 2" @keyup.enter="go">x</button></div>');
});

// ---- the Alpine.js bridge, against a minimal fake Alpine (test/alpine.mjs)

// A Spark root with the given state, its requests recorded instead of sent.
function spark(data, ...kids) {
  const root = h('div', { 'wire:id': 'r1', 'wire:name': 'counter', 'wire:snapshot': JSON.stringify({ v: 1, data }) }, ...kids);
  const sent = [];
  S.send = (el, p) => { sent.push(JSON.parse(JSON.stringify(p))); return Promise.resolve(); };
  return { root, sent };
}
// What `$spark` evaluates to inside el.
const $spark = (A, el) => A.magics.spark(el, { cleanup() {} });
const settle = () => new Promise((r) => setImmediate(r));

test('the bridge registers $spark with Alpine once, and only with an Alpine', () => {
  const A = fakeAlpine();
  assert.equal(S.alpine(undefined), false, 'no Alpine on the page: nothing to do');
  assert.equal(S.alpine({}), false, 'not Alpine');
  assert.equal(S.alpine(A), true);
  assert.equal(typeof A.magics.spark, 'function');
  assert.equal(S.alpine(A), false, 'alpine:init after an earlier registration adds nothing');
  // Outside a Spark there is no $spark.
  assert.equal($spark(A, h('p')), undefined);
});

test('$spark reads the state, records deferred sets, and calls actions through the queue', async () => {
  const A = fakeAlpine();
  S.alpine(A);
  const btn = h('button');
  const { root, sent } = spark({ count: 5, step: 1, tags: ['a'] }, btn);
  const w = $spark(A, btn);
  assert.equal(w.count, 5);
  assert.deepEqual(w.tags, ['a']);
  assert.equal(w.$id, 'r1');
  assert.equal(w.$name, 'counter');
  assert.equal(w.$el, root);
  assert.equal(w.then, undefined, 'not a thenable');
  assert.equal(w.__v_isRef, undefined, 'internal probes see nothing');
  // A plain assignment is deferred, like wire:model: shown at once, sent with the next action.
  w.step = 3;
  assert.equal(w.step, 3);
  await settle();
  assert.equal(sent.length, 0);
  await w.add(10, 'x');
  assert.deepEqual(sent, [{ updates: { step: 3 }, calls: [{ method: 'add', params: [10, 'x'] }] }]);
  // $set sends now (unless told otherwise); $call, $refresh and $commit.
  await w.$set('step', 4);
  await w.$set('step', 5, false);
  await w.$call('increment');
  await w.$refresh();
  w.step = 6;
  await w.$commit();
  assert.deepEqual(sent.slice(1), [
    { updates: { step: 4 }, calls: [] },
    { updates: { step: 5 }, calls: [{ method: 'increment', params: [] }] },
    { updates: {}, calls: [{ method: '$refresh', params: [] }] },
    { updates: { step: 6 }, calls: [] },
  ]);
  // The same instance (queue and state) for every element inside the root.
  assert.equal(S.comp(root).store, S.comp(S.rootOf(btn)).store);
});

test('$spark is reactive: a response updates what Alpine shows, keeping updates not sent yet', () => {
  const A = fakeAlpine();
  S.alpine(A);
  const p = h('p', { 'x-text': '$spark.count' });
  const { root } = spark({ count: 1, step: 1 }, h('div', { 'x-data': '' }, p));
  A.initTree(root);
  assert.equal(p.textContent, '1');
  S.comp(root).q.set('step', 9); // recorded, not sent yet
  S.sync(root, JSON.stringify({ v: 1, data: { count: 2, step: 1 } }));
  assert.equal(p.textContent, '2');
  assert.equal($spark(A, p).step, 9, 'a pending update is not overwritten by the response');
  S.sync(root, 'not json'); // ignored
  assert.equal(p.textContent, '2');
});

test('$entangle: deferred by default, .live sends each change, the server state flows back', async () => {
  const A = fakeAlpine();
  S.alpine(A);
  const box = h('div', { 'x-data': "{ open: $spark.$entangle('open'), q: $spark.$entangle('q').live }" }, h('span', { 'x-text': 'open' }));
  const { root, sent } = spark({ open: false, q: '' }, box);
  A.initTree(root);
  const data = box._x_dataStack[0];
  assert.equal(data.open, false, 'starts from the Spark field');
  assert.equal(data.q, '');
  // Deferred: Alpine's change is recorded and shown, sent with the next action.
  data.open = true;
  await settle();
  assert.equal(sent.length, 0);
  assert.equal($spark(A, box).open, true);
  await $spark(A, box).save();
  assert.deepEqual(sent, [{ updates: { open: true }, calls: [{ method: 'save', params: [] }] }]);
  // Live: sent at once.
  data.q = 'rust';
  await settle();
  assert.deepEqual(sent[1], { updates: { q: 'rust' }, calls: [] });
  // Server to Alpine: a response that changes the field changes the Alpine property.
  S.sync(root, JSON.stringify({ v: 1, data: { open: false, q: 'rust' } }));
  assert.equal(data.open, false);
  assert.equal(box.firstChild.textContent, 'false');
  // `$entangle(field, true)` is the same as `.live`.
  const box2 = h('div', { 'x-data': "{ n: $spark.$entangle('n', true) }" });
  const s2 = spark({ n: 1 }, box2);
  A.initTree(s2.root);
  box2._x_dataStack[0].n = 2;
  await settle();
  assert.deepEqual(s2.sent, [{ updates: { n: 2 }, calls: [] }]);
});

test('morph keeps Alpine state: x-data, what Alpine rendered, x-for rows and x-model values', () => {
  const A = fakeAlpine();
  globalThis.Alpine = A;
  try {
    const label = h('p', { 'x-text': "open ? 'open' : 'closed'" }, 'closed');
    const menu = h('ul', { 'x-show': 'open', style: 'display: none;' }, h('li', {}, 'menu'));
    const box = h('div', { 'x-data': '{ open: false }' }, label, menu);
    const tpl = h('template', { 'x-for': 'r in rows' });
    const row1 = h('li', {}, 'one');
    const row2 = h('li', {}, 'two');
    const typed = h('input', { 'x-model': 'name', value: '' });
    const { root } = spark({ count: 1 }, box, h('ul', {}, tpl, row1, row2), typed, h('b', {}, 'Count: 1'));
    A.initTree(root);
    box._x_dataStack[0].open = true; // the visitor opened the menu
    assert.equal(label.textContent, 'open');
    tpl._x_lastRenderedEl = row2; // what Alpine's x-for leaves on its template
    typed._x_model = {};
    typed.value = 'Ada';
    const state = box._x_dataStack;
    // The server's new render knows nothing of Alpine's state.
    const to = h('div', { 'wire:id': 'r1', 'wire:name': 'counter', 'wire:snapshot': JSON.stringify({ v: 1, data: { count: 2 } }) },
      h('div', { 'x-data': '{ open: false }' }, h('p', { 'x-text': "open ? 'open' : 'closed'" }, 'closed'), h('ul', { 'x-show': 'open', style: 'display: none;' }, h('li', {}, 'menu'))),
      h('ul', {}, h('template', { 'x-for': 'r in rows' })),
      h('input', { 'x-model': 'name', value: '' }),
      h('b', {}, 'Count: 2'));
    S.morph(root, to, null);
    assert.equal(root.childNodes[0], box, 'the x-data element is kept');
    assert.equal(box._x_dataStack, state, 'with its Alpine state');
    assert.equal(state[0].open, true);
    assert.equal(label.textContent, 'open', 'Alpine-rendered text survives the morph');
    assert.equal(menu.hasAttribute('style'), false, 'x-show stays shown');
    assert.deepEqual(root.childNodes[1].childNodes, [tpl, row1, row2], 'x-for rows are Alpine\'s, not removed');
    assert.equal(typed.value, 'Ada', 'an x-model input keeps its value');
    assert.equal(root.childNodes[3].textContent, 'Count: 2', 'server-rendered parts update');
    assert.ok(A.cloned > 0);
  } finally {
    delete globalThis.Alpine;
  }
});

test('morph without Alpine on the page is unchanged by the bridge', () => {
  assert.equal(globalThis.Alpine, undefined);
  const box = h('div', { 'x-data': '{ open: false }' }, h('p', {}, 'a'));
  const from = h('div', {}, box);
  S.morph(from, h('div', {}, h('div', { 'x-data': '{ open: false }' }, h('p', {}, 'b'))));
  assert.equal(from.firstChild, box);
  assert.equal(html(from), '<div><div x-data="{ open: false }"><p>b</p></div></div>');
});

// ---- review round: request storms, old Alpine, wire:model into $spark, and the documented edge cases

test('a binding that resolves to an action is not re-sent by each morph (no request storm)', async (t) => {
  const warn = t.mock.method(console, 'warn', () => {});
  const A = fakeAlpine();
  S.alpine(A);
  globalThis.Alpine = A;
  try {
    // `total` is not a field, so `$spark.total` is an action call; Alpine calls a function result.
    const view = () => h('div', { 'x-data': '' }, h('span', { 'x-text': '$spark.total' }));
    const { root, sent } = spark({ count: 1 }, view());
    A.initTree(root);
    await settle();
    assert.equal(sent.length, 1, 'the first evaluation sends once, as without Alpine');
    for (let i = 0; i < 2; i++) {
      S.morph(root, h('div', { 'wire:id': 'r1', 'wire:name': 'counter', 'wire:snapshot': JSON.stringify({ v: 1, data: { count: 1 } }) }, view()), null);
      await settle();
    }
    assert.equal(sent.length, 1, 'the morph\'s clone pass sends nothing');
    assert.equal(warn.mock.callCount(), 1, 'one warning');
    assert.match(warn.mock.calls[0].arguments[0], /\$spark\.total\(\) called while rendering/);
  } finally {
    delete globalThis.Alpine;
  }
});

test('an Alpine older than 3.13 gets no $spark and one warning', (t) => {
  const warn = t.mock.method(console, 'warn', () => {});
  const A = fakeAlpine();
  delete A.entangle; // Alpine 3.12 exports magic and interceptor, not entangle / cloneNode
  assert.equal(S.alpine(A), false);
  assert.equal(S.alpine(A), false);
  assert.equal(A.magics.spark, undefined);
  assert.equal(warn.mock.callCount(), 1);
  assert.match(warn.mock.calls[0].arguments[0], /needs Alpine\.js 3\.13 or newer/);
});

test('wire:model input reaches $spark at once, and a new $spark sees queued updates', () => {
  const A = fakeAlpine();
  S.alpine(A);
  const input = h('input', { 'wire:model': 'title' });
  const span = h('span', { 'x-text': '$spark.title' });
  const { root } = spark({ title: 'a', body: 'b' }, h('div', { 'x-data': '' }, input, span));
  A.initTree(root);
  input.value = 'typed';
  S.model({ target: input }, 'input');
  assert.equal(span.textContent, 'typed');
  assert.deepEqual(S.comp(root).q.updates, { title: 'typed' });
  // Created after an update was queued: the reactive copy starts from it.
  const p = h('p');
  const s2 = spark({ body: 'server' }, p);
  S.comp(s2.root).q.set('body', 'queued');
  assert.equal($spark(A, p).body, 'queued');
});

test('$spark ignores writes to helpers, symbols and __ names', () => {
  const A = fakeAlpine();
  S.alpine(A);
  const p = h('p');
  const { root } = spark({ n: 1 }, p);
  const w = $spark(A, p);
  w.$id = 'other';
  w.__proto__ = { polluted: true }; // eslint-disable-line no-proto
  w[Symbol('s')] = 1;
  const q = S.comp(root).q;
  assert.deepEqual(Object.keys(q.updates), []);
  assert.equal(Object.getPrototypeOf(q.updates), Object.prototype);
  assert.equal(w.$id, 'r1');
});

test('an entangle is released with its element', async () => {
  const A = fakeAlpine();
  S.alpine(A);
  const box = h('div', { 'x-data': "{ open: $spark.$entangle('open') }" });
  const { root } = spark({ open: false }, box);
  A.initTree(root);
  const data = box._x_dataStack[0];
  box._x_cleanups.forEach((f) => f()); // what Alpine runs when the element is removed
  S.sync(root, JSON.stringify({ v: 1, data: { open: true } }));
  assert.equal(data.open, false, 'nothing written back into the released property');
  data.open = 'x';
  assert.deepEqual(S.comp(root).q.updates, {}, 'nothing recorded from it');
});

test('$spark is the nearest Spark: a child Spark inside a parent', async () => {
  const A = fakeAlpine();
  S.alpine(A);
  const btn = h('button');
  const kid = h('div', { 'wire:id': 'k1', 'wire:name': 'kid', 'wire:snapshot': JSON.stringify({ v: 1, data: { n: 7 } }) }, btn);
  h('div', { 'wire:id': 'p1', 'wire:name': 'parent', 'wire:snapshot': JSON.stringify({ v: 1, data: { n: 1 } }) }, kid);
  const sent = [];
  S.send = (el, p) => { sent.push([el.getAttribute('wire:id'), p.calls[0].method]); return Promise.resolve(); };
  const w = $spark(A, btn);
  assert.equal(w.$id, 'k1');
  assert.equal(w.$name, 'kid');
  assert.equal(w.n, 7);
  await w.ping();
  assert.deepEqual(sent, [['k1', 'ping']]);
});

test('morph leaves attributes alone during an Alpine transition and keeps x-if output', () => {
  const fading = h('div', { class: 'opacity-50' });
  fading._x_transitioning = {};
  const tpl = h('template', { 'x-if': 'open' });
  const shown = h('p', {}, 'shown by x-if');
  tpl._x_lastRenderedEl = shown;
  const from = h('div', {}, fading, tpl, shown, h('i', {}, 'old'));
  S.morph(from, h('div', {}, h('div', { class: 'opacity-0' }), h('template', { 'x-if': 'open' }), h('i', {}, 'new')));
  assert.equal(fading.getAttribute('class'), 'opacity-50');
  assert.deepEqual(from.childNodes.slice(0, 3), [fading, tpl, shown]);
  assert.equal(html(from.childNodes[3]), '<i>new</i>');
});

test('live entangle while a request is in flight: coalesced, and the response keeps the newer value', async () => {
  const A = fakeAlpine();
  S.alpine(A);
  const box = h('div', { 'x-data': "{ q: $spark.$entangle('q').live }" });
  const { root } = spark({ q: '' }, box);
  const sent = [];
  let release;
  S.send = (el, p) => { sent.push(p.updates); return new Promise((r) => { release = r; }); };
  A.initTree(root);
  const data = box._x_dataStack[0];
  data.q = 'r';
  await settle();
  data.q = 'ru';
  data.q = 'rus';
  await settle();
  assert.deepEqual(sent, [{ q: 'r' }], 'one request in flight');
  // The first response arrives with the older value; the queued one wins.
  S.sync(root, JSON.stringify({ v: 1, data: { q: 'r' } }));
  assert.equal(data.q, 'rus');
  release();
  await settle();
  assert.deepEqual(sent, [{ q: 'r' }, { q: 'rus' }]);
  release();
});

test('render focuses a newly inserted autofocus element once, not away from another Spark', () => {
  const snap = JSON.stringify({ v: 1, data: {} });
  const fresh = () => h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('p', {}, 'x'), h('form', {}, h('input', { name: 'n', autofocus: '' })));
  const render = (root, to, active) => S.render(root, to, active, () => {});
  // Inserted by the morph, nothing focused: focused once.
  const a = h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('p', {}, 'x'));
  render(a, fresh(), null);
  const input = a.childNodes[1].firstChild;
  assert.equal(input.focused, 1);
  // Already there: a later render does not focus it again.
  render(a, fresh(), null);
  assert.equal(input.focused, 1);
  // Focus on <body> (nothing in particular) does not block it.
  const body = h('body', {});
  const a2 = h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('p', {}, 'x'));
  body.appendChild(a2);
  render(a2, fresh(), body);
  assert.equal(a2.childNodes[1].firstChild.focused, 1);
  // The visitor is in another Spark, on a field or on a button: focus stays there.
  for (const tag of ['input', 'button']) {
    const other = h(tag, {});
    h('div', { 'wire:id': 'b' }, other);
    const b = h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('p', {}, 'x'));
    render(b, fresh(), other);
    assert.equal(b.childNodes[1].firstChild.focused, undefined, tag);
  }
  // On a link outside every Spark: focus stays there too.
  const link = h('a', { href: '/' });
  h('nav', {}, link);
  const n = h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('p', {}, 'x'));
  render(n, fresh(), link);
  assert.equal(n.childNodes[1].firstChild.focused, undefined);
  // Typing inside the same Spark: the new field takes focus (the Spark asked for it).
  const mine = h('input', { name: 'q' });
  const c = h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, mine);
  render(c, h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('input', { name: 'q' }), h('input', { name: 'n', autofocus: '' })), mine);
  assert.equal(c.childNodes[1].focused, 1);
});

test('an inserted autofocus wire:model input gets its value before it is focused', () => {
  const snap = JSON.stringify({ v: 1, data: { title: 'Hello' } });
  const root = h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('button', {}, 'Edit'));
  Element.active = null;
  // The browser's boot: fill the wire:model inputs, skipping the focused one (document.activeElement).
  const boot = (el) => S.fill(el, Element.active);
  S.render(root, h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('input', { 'wire:model': 'title', autofocus: '' })), null, boot);
  const input = root.firstChild;
  assert.equal(input.value, 'Hello');
  assert.equal(input.focused, 1);
});

test('with Alpine on the page, the autofocus waits for Alpine to initialise the new nodes (a microtask)', async () => {
  globalThis.Alpine = fakeAlpine();
  try {
    const snap = JSON.stringify({ v: 1, data: {} });
    const root = h('div', { 'wire:id': 'a', 'wire:snapshot': snap });
    S.render(root, h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('input', { autofocus: '' })), null, () => {});
    const input = root.firstChild;
    assert.equal(input.focused, undefined, 'not yet: Alpine\'s mutation observer runs first');
    await Promise.resolve();
    assert.equal(input.focused, 1);
  } finally {
    delete globalThis.Alpine;
  }
});

test('focus on an element of the same Spark that the morph removes does not block the autofocus', () => {
  const snap = JSON.stringify({ v: 1, data: { title: 'Hello' } });
  const edit = h('button', {}, 'Edit'); // focused: the visitor clicked it
  const root = h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, edit);
  S.render(root, h('div', { 'wire:id': 'a', 'wire:snapshot': snap }, h('input', { 'wire:model': 'title', autofocus: '' })), edit, () => {});
  assert.equal(edit.parentNode, null, 'the button was replaced by the form');
  assert.equal(root.firstChild.focused, 1);
});

test('a parent re-render does not take focus from a field inside a child Spark', () => {
  const snap = JSON.stringify({ v: 1, data: {} });
  const typing = h('input', { name: 'q' });
  const kid = h('div', { 'wire:id': 'kid', 'wire:snapshot': snap }, typing);
  const root = h('div', { 'wire:id': 'p', 'wire:snapshot': snap }, kid);
  S.render(root, h('div', { 'wire:id': 'p', 'wire:snapshot': snap }, h('div', { 'wire:id': 'kid' }), h('input', { autofocus: '' })), typing, () => {});
  assert.equal(root.firstChild, kid, 'the child stays');
  assert.equal(root.childNodes[1].focused, undefined);
});

test('call arguments written with the json filter decode to the values the template held', () => {
  // `wire:click="remove({{ id | json }}, {{ name | json }})"` once the browser decoded the attribute's entities.
  assert.deepEqual(S.parseCall('remove(7, "\\u0027+alert(1)+\\u0027\\\\\\u003c/script\\u003e\\u0026\\"")'), {
    method: 'remove',
    params: [7, "'+alert(1)+'\\</script>&\""],
  });
  assert.deepEqual(S.parseCall("f('a\\nb', \"t\\tx\", 'it\\'s', \"\\u2028\")"), { method: 'f', params: ['a\nb', 't\tx', "it's", '\u2028'] });
});

test('a redirect effect is followed only to http(s) URLs', () => {
  const base = 'https://app.test/posts/1';
  assert.equal(S.redirectTarget('/done', base), 'https://app.test/done');
  assert.equal(S.redirectTarget('https://other.test/x', base), 'https://other.test/x');
  for (const bad of ['javascript:alert(1)', ' JavaScript:alert(1)', 'java\nscript:alert(1)', 'data:text/html,x', 'vbscript:x', '', null, 5]) {
    assert.equal(S.redirectTarget(bad, base), null, String(bad));
  }
});

test('$spark ignores state keys that would reach an object prototype', () => {
  const A = fakeAlpine();
  S.alpine(A);
  const btn = h('button');
  const data = JSON.parse('{"count":1,"__proto__":{"polluted":true},"constructor":{"x":1},"prototype":2}');
  const { root } = spark(data, btn);
  const w = $spark(A, btn);
  const store = S.comp(root).store;
  assert.equal(w.count, 1);
  assert.equal(Object.getPrototypeOf(store), Object.prototype);
  assert.equal(store.polluted, undefined);
  assert.equal(w.constructor, undefined, 'kept by $spark, neither a field nor an action');
  assert.equal(w.prototype, undefined);
  S.sync(root, '{"data":{"count":2,"__proto__":{"later":true}}}');
  assert.equal(w.count, 2);
  assert.equal(Object.getPrototypeOf(store), Object.prototype);
  assert.equal(store.later, undefined);
  assert.equal(({}).polluted, undefined);
});

test('wire:model with a dotted name shows one key of an object field', () => {
  const input = h('input', { 'wire:model': 'form.title' });
  const root = h('div', { 'wire:id': 'f1', 'wire:name': 'post_form', 'wire:snapshot': JSON.stringify({ v: 3, data: { form: { title: 'Hi', body: 'x' } } }) }, input);
  S.fill(root, null);
  assert.equal(input.value, 'Hi');
});
test('the stream URL carries the stream tokens of the subscribed Sparks', () => {
  const roots = [h('div', { 'wire:stream': 'a.b' }), h('div', { 'wire:stream': 'c.d' }), h('div', { 'wire:stream': 'a.b' })];
  assert.equal(S.streamUrl(roots), '/_sparks/stream?t=a.b%2Cc.d');
  assert.equal(S.streamUrl([h('div', { 'wire:stream': '' })]), null, 'no token, no stream');
});

test('a listen message becomes the instance\'s $listen call, sent back unchanged, and a window event', () => {
  const a = h('div', { 'wire:id': 'A1', 'wire:name': 'order-status', 'wire:stream': 't1' });
  const b = h('div', { 'wire:id': 'B2', 'wire:name': 'order-status', 'wire:stream': 't2' });
  const committed = [];
  const fired = [];
  const commit = (el, call) => committed.push([el.getAttribute('wire:id'), call]);
  const fire = (name, detail) => fired.push([name, detail]);
  const m = {
    target: 'A1', kind: 'listen', channel: 'private-orders.7', event: 'OrderShipped',
    data: '{"order_id":7,"big":12345678901234567890}', exp: 1791160270, seq: 1791160210000001, sig: 'abc',
  };
  S.onMessage(m, [a, b], commit, fire);
  assert.deepEqual(committed, [['A1', {
    method: '$listen',
    params: ['private-orders.7', 'OrderShipped', '{"order_id":7,"big":12345678901234567890}', 1791160270, 1791160210000001, 'abc'],
  }]], 'only the target instance; the data text is sent back byte for byte');
  assert.equal(fired.length, 1);
  assert.equal(fired[0][0], 'anvil:OrderShipped');
  assert.equal(fired[0][1].order_id, 7);
});

test('a malformed listen message is dropped', () => {
  const a = h('div', { 'wire:id': 'A1', 'wire:stream': 't1' });
  const committed = [];
  const fired = [];
  const base = { target: 'A1', kind: 'listen', channel: 'c', event: 'e', data: '{}', exp: 1, seq: 2, sig: 's' };
  for (const bad of [
    { ...base, sig: undefined }, { ...base, data: { a: 1 } }, { ...base, seq: '2' }, { ...base, channel: 7 },
    { ...base, target: undefined }, null, 'listen',
  ]) {
    S.onMessage(bad, [a], (el, c) => committed.push(c), (n) => fired.push(n));
  }
  assert.deepEqual(committed, []);
  assert.deepEqual(fired, []);
  assert.equal(S.listenCall({ ...base, exp: '1' }), null);
  // Data that is not JSON still goes back to the server (which refuses it), but fires no window event.
  S.onMessage({ ...base, data: 'not json' }, [a], (el, c) => committed.push(c), (n) => fired.push(n));
  assert.equal(committed.length, 1);
  assert.deepEqual(fired, []);
});

test('refresh and event messages keep their behaviour', () => {
  const a = h('div', { 'wire:id': 'A1', 'wire:name': 'counter', 'wire:stream': 't1' });
  const b = h('div', { 'wire:id': 'B2', 'wire:name': 'other', 'wire:stream': 't2' });
  const committed = [];
  const fired = [];
  const commit = (el, call) => committed.push([el.getAttribute('wire:id'), call.method]);
  const fire = (name, detail) => fired.push([name, detail]);
  S.onMessage({ target: 'counter', kind: 'refresh' }, [a, b], commit, fire);
  S.onMessage({ target: 'B2', kind: 'refresh' }, [a, b], commit, fire);
  S.onMessage({ kind: 'refresh' }, [a, b], commit, fire);
  S.onMessage({ target: 'counter', kind: 'event', event: 'tick', payload: { n: 3 } }, [a, b], commit, fire);
  S.onMessage({ target: 'counter', kind: 'unknown' }, [a, b], commit, fire);
  assert.deepEqual(committed, [['A1', '$refresh'], ['B2', '$refresh'], ['A1', '$refresh'], ['B2', '$refresh']]);
  assert.deepEqual(fired, [['tick', { n: 3 }]]);
});

test('a failing stream is reopened with a growing wait, and never after the session ended', () => {
  const r = new S.Reconnect();
  assert.deepEqual([r.failed(), r.failed(), r.failed(), r.failed()], [5000, 10000, 20000, 40000]);
  for (let i = 0; i < 20; i++) r.failed();
  assert.equal(r.failed(), 300000, 'capped at 5 minutes');
  r.opened();
  assert.equal(r.failed(), 5000, 'back to 5 s after a stream opened');
  r.end();
  assert.equal(r.failed(), null, 'no reopen after {"kind":"end"}');
});
