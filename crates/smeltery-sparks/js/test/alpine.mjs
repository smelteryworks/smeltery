// A tiny stand-in for Alpine.js 3 with the parts the Sparks bridge and morph use: magic, reactive / effect,
// interceptor + entangle (same logic as Alpine's src/interceptor.js and src/entangle.js), closestDataStack, and a
// cloneNode that renders x-text / x-show into the server's HTML. Directives: x-data, x-text, x-show. Effects run
// synchronously (Alpine batches them), which is enough for the bridge's contract.
export function fakeAlpine() {
  const magics = {};
  let running = null;
  let cloning = false;

  function effect(fn) {
    const e = () => {
      if (e.stopped || running === e) return;
      const prev = running;
      running = e;
      try { fn(); } finally { running = prev; }
    };
    e.deps = new Set();
    e();
    return e;
  }
  function release(e) { e.stopped = true; }
  function reactive(target) {
    const subs = {};
    return new Proxy(target, {
      get(t, k) {
        if (running && typeof k === 'string') (subs[k] ||= new Set()).add(running);
        return t[k];
      },
      set(t, k, v) {
        const changed = t[k] !== v;
        t[k] = v;
        if (changed) for (const e of [...(subs[k] || [])]) e();
        return true;
      },
    });
  }
  function interceptor(callback, mutateObj = () => {}) {
    const obj = {
      initialValue: undefined,
      _x_interceptor: true,
      initialize(data, path, key, cleanup) {
        return callback(this.initialValue, () => data[path], (v) => { data[path] = v; }, path, key, cleanup);
      },
    };
    mutateObj(obj);
    return (initialValue) => { obj.initialValue = initialValue; return obj; };
  }
  const clone = (v) => (typeof v === 'object' ? JSON.parse(JSON.stringify(v)) : v);
  function entangle({ get: outerGet, set: outerSet }, { get: innerGet, set: innerSet }) {
    let first = true;
    let outerHash;
    const ref = effect(() => {
      const outer = outerGet();
      const inner = innerGet();
      if (first) { innerSet(clone(outer)); first = false; } else {
        const o = JSON.stringify(outer);
        if (o !== outerHash) innerSet(clone(outer));
        else if (o !== JSON.stringify(inner)) outerSet(clone(inner));
      }
      outerHash = JSON.stringify(outerGet());
    });
    return () => release(ref);
  }
  function closestDataStack(n) {
    if (n._x_dataStack) return n._x_dataStack;
    return n.parentNode ? closestDataStack(n.parentNode) : [];
  }
  // The scope of an expression: magics, then the data stack (innermost first).
  function evaluate(el, expr, cleanups) {
    const scope = {};
    for (const [name, cb] of Object.entries(magics)) {
      Object.defineProperty(scope, `$${name}`, { get: () => cb(el, { cleanup: (f) => cleanups.push(f) }) });
    }
    const stack = closestDataStack(el);
    const proxy = new Proxy(scope, {
      has: () => true,
      get(t, k) {
        if (k === Symbol.unscopables) return undefined;
        for (const d of stack) if (k in d) return d[k];
        return k in t ? t[k] : globalThis[k];
      },
      set(t, k, v) {
        const d = stack.find((x) => k in x) || stack[0];
        d[k] = v;
        return true;
      },
    });
    // eslint-disable-next-line no-new-func
    const r = new Function('s', `with (s) { return (${expr}); }`)(proxy);
    // Like Alpine's runIfTypeOfFunction: an expression that yields a function is called.
    return typeof r === 'function' ? r() : r;
  }
  // x-data / x-text / x-show on one element; in clone mode effects run once and x-data keeps the live state.
  function init(el) {
    el._x_cleanups ||= [];
    const data = el.getAttribute('x-data');
    if (data !== null && !(cloning && el._x_dataStack)) {
      // Like Alpine's x-data: reactive first, then the interceptors run on the reactive object.
      const obj = reactive(evaluate(el, data || '{}', el._x_cleanups));
      for (const [k, v] of Object.entries(obj)) {
        if (v && v._x_interceptor) obj[k] = v.initialize(obj, k, k, (f) => el._x_cleanups.push(f));
      }
      el._x_dataStack = [obj, ...(el.parentNode ? closestDataStack(el.parentNode) : [])];
    }
    const run = (fn) => (cloning ? fn() : effect(fn));
    const text = el.getAttribute('x-text');
    if (text !== null) run(() => { el.textContent = evaluate(el, text, el._x_cleanups); });
    const show = el.getAttribute('x-show');
    if (show !== null) run(() => { el.style.display = evaluate(el, show, el._x_cleanups) ? '' : 'none'; if (el.style.display) el.put('style', 'display: none;'); else el.removeAttribute('style'); });
  }
  function initTree(el) {
    init(el);
    for (const c of el.childNodes) if (c.nodeType === 1) initTree(c);
  }
  const A = {
    version: '3.17.4',
    cloned: 0,
    magic(name, cb) { magics[name] = cb; },
    reactive,
    effect,
    release,
    interceptor,
    entangle,
    closestDataStack,
    evaluate: (el, expr) => evaluate(el, expr, []),
    initTree,
    // Alpine's cloneNode: carry the live data stack over, then initialise only `to` itself in clone mode.
    cloneNode(from, to) {
      A.cloned++;
      if (from._x_dataStack) { to._x_dataStack = from._x_dataStack; to.put('data-has-alpine-state', 'true'); }
      cloning = true;
      try { init(to); } finally { cloning = false; }
    },
    magics,
  };
  return A;
}
