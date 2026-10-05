// A minimal DOM for the runtime's pure parts: elements, text, attributes, child lists.
export class Node {
  constructor(nodeType) { this.nodeType = nodeType; this.parentNode = null; this.childNodes = []; }
  get firstChild() { return this.childNodes[0] || null; }
  get nextSibling() {
    if (!this.parentNode) return null;
    const s = this.parentNode.childNodes;
    return s[s.indexOf(this) + 1] || null;
  }
  get isConnected() { return true; }
  appendChild(n) { return this.insertBefore(n, null); }
  insertBefore(n, ref) {
    if (n.parentNode) n.parentNode.removeChild(n);
    const i = ref ? this.childNodes.indexOf(ref) : -1;
    if (ref && i < 0) throw new Error('ref is not a child');
    if (i < 0) this.childNodes.push(n); else this.childNodes.splice(i, 0, n);
    n.parentNode = this;
    return n;
  }
  removeChild(n) {
    const i = this.childNodes.indexOf(n);
    if (i < 0) throw new Error('not a child');
    this.childNodes.splice(i, 1);
    n.parentNode = null;
    return n;
  }
  get textContent() { return this.nodeType === 3 ? this.nodeValue : this.childNodes.map((c) => c.textContent).join(''); }
  set textContent(v) {
    if (this.nodeType === 3) { this.nodeValue = v; return; }
    for (const c of [...this.childNodes]) this.removeChild(c);
    this.appendChild(new Text(String(v)));
  }
}

export class Text extends Node {
  constructor(value) { super(3); this.nodeValue = value; }
}

export class Element extends Node {
  constructor(tag) {
    super(1);
    this.tagName = tag.toUpperCase();
    this.attributes = [];
    this.style = { display: '' };
    const el = this;
    this.classList = {
      add(c) { const l = el.classes(); if (!l.includes(c)) el.setAttribute('class', [...l, c].join(' ')); },
      remove(c) { el.setAttribute('class', el.classes().filter((x) => x !== c).join(' ')); },
      contains(c) { return el.classes().includes(c); },
    };
    // Live properties of form fields, set from attributes once (like a browser).
    this.value = '';
    this.checked = false;
  }
  classes() { return (this.getAttribute('class') || '').split(/\s+/).filter(Boolean); }
  get type() { return this.getAttribute('type') || ''; }
  get multiple() { return this.hasAttribute('multiple'); }
  get options() { return this.tagName === 'SELECT' ? this.childNodes.filter((c) => c.tagName === 'OPTION') : undefined; }
  get selected() { return !!this._selected; }
  set selected(v) { this._selected = v; }
  getAttribute(n) { const a = this.attributes.find((x) => x.name === n); return a ? a.value : null; }
  hasAttribute(n) { return this.attributes.some((x) => x.name === n); }
  // Like a browser, setAttribute refuses names that are not XML names (Alpine's "@click"); the HTML parser accepts
  // them, which `h` models with `put`.
  setAttribute(n, v) {
    if (!/^[A-Za-z_:][-\w:.]*$/.test(n)) throw new Error(`InvalidCharacterError: '${n}' is not a valid attribute name`);
    this.put(n, v);
  }
  put(n, v) {
    const a = this.attributes.find((x) => x.name === n);
    if (a) a.value = String(v); else this.attributes.push(new Attr(n, String(v)));
  }
  getAttributeNode(n) { return this.attributes.find((x) => x.name === n) || null; }
  setAttributeNode(a) { this.put(a.name, a.value); }
  removeAttribute(n) { this.attributes = this.attributes.filter((x) => x.name !== n); }
  // Counts focus() calls; Element.active models document.activeElement.
  focus() { this.focused = (this.focused || 0) + 1; Element.active = this; }
}

export class Attr {
  constructor(name, value) { this.name = name; this.value = value; }
  cloneNode() { return new Attr(this.name, this.value); }
}

/** h('div', {id: 'a'}, 'text', h('p')) */
export function h(tag, attrs = {}, ...kids) {
  const el = new Element(tag);
  for (const [k, v] of Object.entries(attrs)) el.put(k, v);
  if (el.hasAttribute('value')) el.value = el.getAttribute('value');
  if (el.hasAttribute('checked')) el.checked = true;
  for (const k of kids) el.appendChild(typeof k === 'string' ? new Text(k) : k);
  return el;
}

/** Serialize for assertions (attributes in their order). */
export function html(n) {
  if (n.nodeType === 3) return n.nodeValue;
  const attrs = n.attributes.map((a) => ` ${a.name}="${a.value}"`).join('');
  const tag = n.tagName.toLowerCase();
  return `<${tag}${attrs}>${n.childNodes.map(html).join('')}</${tag}>`;
}
