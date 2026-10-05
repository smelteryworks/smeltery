// Morphs one render of a Watchfire panel into the next with the Sparks runtime's `morph`, as the browser does when a
// pushed refresh arrives, and checks the result. Run by the `a_refresh_morphs_the_panel_in_place` test in
// src/web/live.rs: `node panel_morph.mjs <sparks js dir> <before.html> <after.html> <kept wire:key>`.
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [, , sparksDir, beforeFile, afterFile, keptKey] = process.argv;
const S = createRequire(join(sparksDir, 'sparks.js'))(join(sparksDir, 'sparks.js'));
const { h, Text } = await import(pathToFileURL(join(sparksDir, 'test', 'dom.mjs')).href);

const VOID = new Set(['input', 'meta', 'br', 'img', 'hr', 'link']);

// A parser for the well-formed HTML the panels render (entities stay as written, on both sides alike).
function parse(src) {
  const root = h('div');
  const stack = [root];
  const token = /<\/([A-Za-z0-9]+)\s*>|<([A-Za-z0-9]+)((?:\s+[^\s=>]+(?:="[^"]*")?)*)\s*>|([^<]+)/g;
  let m;
  while ((m = token.exec(src))) {
    const top = stack[stack.length - 1];
    if (m[1]) {
      const tag = m[1].toUpperCase();
      while (stack.length > 1 && stack.pop().tagName !== tag);
    } else if (m[2]) {
      const attrs = {};
      const attr = /([^\s=>]+)(?:="([^"]*)")?/g;
      let a;
      while ((a = attr.exec(m[3]))) attrs[a[1]] = a[2] === undefined ? '' : a[2];
      const el = h(m[2], attrs);
      top.appendChild(el);
      if (!VOID.has(m[2].toLowerCase())) stack.push(el);
    } else {
      top.appendChild(new Text(m[4]));
    }
  }
  return root;
}

// Serialize with sorted attributes (the order setAttribute leaves them in does not matter to a browser).
function out(n) {
  if (n.nodeType === 3) return n.nodeValue;
  const attrs = n.attributes
    .map((a) => ` ${a.name}="${a.value}"`)
    .sort()
    .join('');
  const tag = n.tagName.toLowerCase();
  return `<${tag}${attrs}>${n.childNodes.map(out).join('')}</${tag}>`;
}

function find(node, key) {
  for (const c of node.childNodes) {
    if (c.nodeType !== 1) continue;
    if (c.getAttribute('wire:key') === key) return c;
    const hit = find(c, key);
    if (hit) return hit;
  }
  return null;
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

const page = parse(readFileSync(beforeFile, 'utf8'));
const next = parse(readFileSync(afterFile, 'utf8'));
const root = page.firstChild;
const kept = find(root, keptKey);
if (!kept) fail(`no element with wire:key ${keptKey} before the morph`);
S.morph(root, next.firstChild, null);
const want = out(parse(readFileSync(afterFile, 'utf8')).firstChild);
const got = out(root);
if (got !== want) fail(`the morphed panel differs from the new render\n--- got\n${got}\n--- want\n${want}`);
if (find(root, keptKey) !== kept) fail(`the row ${keptKey} was replaced instead of updated in place`);

// Every action call in an agent's rows names that agent, as sparks.js parses it (quotes and backslashes included).
const decode = (v) =>
  v.replace(/&#x27;|&#39;|&quot;|&lt;|&gt;|&amp;/g, (e) => ({ '&#x27;': "'", '&#39;': "'", '&quot;': '"', '&lt;': '<', '&gt;': '>', '&amp;': '&' })[e]);
let calls = 0;
(function walk(node, agent) {
  for (const c of node.childNodes) {
    if (c.nodeType !== 1) continue;
    const key = c.getAttribute('wire:key');
    const mine = key && key.startsWith('agent-') ? decode(key.slice(6)) : agent;
    for (const name of ['wire:submit', 'wire:click.prevent']) {
      const value = c.getAttribute(name);
      if (!value || !mine) continue;
      const call = S.parseCall(decode(value));
      if (call.method === 'cancel') continue;
      calls++;
      if (call.params[0] !== mine) fail(`${name}="${value}" names ${JSON.stringify(call.params[0])}, not ${JSON.stringify(mine)}`);
    }
    walk(c, mine);
  }
})(root, null);
if (calls === 0) fail('no action calls found');
console.log('ok');
