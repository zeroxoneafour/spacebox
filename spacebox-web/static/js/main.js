// Spacebox admin client: panels, inspector, actions and the play loop.

import { store, subscribe, loadAll, tick, api } from './api.js';
import { Scene, buildingColor, empireColor, fleetDestination } from './scene.js';

const $ = (s) => document.querySelector(s);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const num = (x, d = 0) => (x == null ? '–' : Number(x).toLocaleString(undefined, { maximumFractionDigits: d, minimumFractionDigits: d }));
const signed = (x, d = 1) => `<span class="${x > 0.005 ? 'pos' : x < -0.005 ? 'neg' : 'muted'}">${x > 0 ? '+' : ''}${num(x, d)}</span>`;

const ui = {
  view: { mode: 'galaxy', id: null },
  sel: { kind: 'galaxy' },
  playing: false,
  busy: false,
};

// ---------------------------------------------------------------------------------------------
// Helpers over the store
// ---------------------------------------------------------------------------------------------

const S = () => store.state;
const D = () => store.defs;
const resName = (id) => D().resources[id]?.display_name ?? `#${id}`;
const costStr = (list) => (list.length ? list.map(([r, c]) => `${num(c)} ${resName(r)}`).join(', ') : 'free');
const planetOwner = (pid) => S().planets[pid].owner;
const fleetById = (id) => S().fleets.find((f) => f.id === id);
const shipName = (id) => D().ships[id]?.display_name ?? `#${id}`;
const pct = (x) => `${Math.round(Math.max(0, Math.min(1, x)) * 100)}%`;
const bar = (frac, color) => `<div class="bar"><div style="width:${pct(frac)};${color ? `background:${color}` : ''}"></div></div>`;

/// Hyperlane jumps from an empire's owned systems to every system (client-side BFS).
function jumpsFromBorders(empire) {
  const s = S();
  const dist = new Array(s.systems.length).fill(Infinity);
  const queue = [];
  for (const sys of s.systems) {
    if (sys.owner === empire) {
      dist[sys.id] = 0;
      queue.push(sys.id);
    }
  }
  for (let i = 0; i < queue.length; i++) {
    for (const n of s.systems[queue[i]].links) {
      if (dist[n] === Infinity) {
        dist[n] = dist[queue[i]] + 1;
        queue.push(n);
      }
    }
  }
  return { dist, owns: queue.length > 0 };
}

/// Influence cost and extra days of building in system `sys` for `empire` (null if not
/// an expansion).
function expansionCost(empire, sys) {
  const r = D().rules;
  if (S().systems[sys].owner != null || r.expansion_resource == null) return null;
  const { dist, owns } = jumpsFromBorders(empire);
  const jumps = owns ? dist[sys] : 1;
  if (jumps === Infinity) return { unreachable: true };
  const n = Math.max(0, jumps - 1);
  return { jumps, influence: r.expansion_base_cost * 2 ** n, days: Math.round(r.expansion_base_days * 2 ** (n / 2)) };
}
const knows = (empire, tech) => empire != null && S().empires[empire].techs.includes(tech);
const techsMet = (empire, list) => list.every((t) => knows(empire, t));

function toast(msg, err = false) {
  const el = document.createElement('div');
  el.className = err ? 'toast err' : 'toast';
  el.textContent = msg;
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), err ? 6000 : 2500);
}

// Re-render panels without clobbering a control the user is interacting with.
function setHTML(el, html) {
  const a = document.activeElement;
  if (a && el.contains(a) && ['INPUT', 'SELECT', 'TEXTAREA'].includes(a.tagName)) {
    el.dataset.stale = '1';
    return;
  }
  el.dataset.stale = '';
  el.innerHTML = html;
}
for (const id of ['#left', '#right']) {
  $(id).addEventListener('focusout', () => setTimeout(() => {
    if ($(id).querySelector('[data-stale="1"]') || $(id).dataset.stale === '1') render();
  }, 0));
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

let scene;
let queued = false;
function render() {
  if (queued) return;
  queued = true;
  requestAnimationFrame(() => {
    queued = false;
    if (!S()) return;
    validateSelection();
    $('#day').textContent = `day ${num(S().day)}`;
    $('#play').textContent = ui.playing ? '⏸' : '▶';
    $('#net').textContent = store.lastOps == null
      ? `full state ${kb(store.lastBytes)}`
      : `diff ${kb(store.lastBytes)} · ${store.lastOps} ops`;
    renderCrumbs();
    setHTML($('#empires'), empiresPanel());
    setHTML($('#inspector'), inspector());
    renderLog();
    scene.show(ui.view, store, ui.sel);
    $('#hint').textContent = {
      galaxy: 'click a system or fleet to inspect · double-click to enter · right-click a system to send the selected fleet',
      system: 'click a planet or fleet · double-click to land · right-click to move the selected fleet (or bombard a planet)',
      planet: 'click a tile to build · drag to rotate · right-click with a fleet selected to bombard',
    }[ui.view.mode];
  });
}
const kb = (b) => (b > 2048 ? `${(b / 1024).toFixed(1)} KB` : `${b} B`);

function validateSelection() {
  const s = S();
  const ok = (arr, id) => id != null && id < arr.length;
  if (ui.view.mode === 'system' && !ok(s.systems, ui.view.id)) ui.view = { mode: 'galaxy', id: null };
  if (ui.view.mode === 'planet' && !ok(s.planets, ui.view.id)) ui.view = { mode: 'galaxy', id: null };
  const k = ui.sel.kind;
  if ((k === 'system' && !ok(s.systems, ui.sel.id)) || ((k === 'planet') && !ok(s.planets, ui.sel.id)) || (k === 'empire' && !ok(s.empires, ui.sel.id)) || (k === 'fleet' && !fleetById(ui.sel.id))) {
    ui.sel = { kind: 'galaxy' };
  }
}

function renderCrumbs() {
  const s = S();
  const parts = ['<a data-act="goto-galaxy">Galaxy</a>'];
  if (ui.view.mode !== 'galaxy') {
    const sys = ui.view.mode === 'system' ? ui.view.id : s.planets[ui.view.id].system;
    parts.push(`<a data-act="goto-system" data-id="${sys}">${esc(s.systems[sys].display_name)}</a>`);
  }
  if (ui.view.mode === 'planet') parts.push(`<a data-act="goto-planet" data-id="${ui.view.id}">${esc(s.planets[ui.view.id].display_name)}</a>`);
  $('#crumbs').innerHTML = parts.join('<span class="sep">›</span>');
}

let lastLogSeq = -1;
function renderLog() {
  const evs = store.events;
  const top = evs.length ? evs[evs.length - 1].seq : 0;
  if (top === lastLogSeq) return;
  lastLogSeq = top;
  const log = $('#log');
  log.innerHTML = evs.slice(-200).reverse().map((e) => `<div class="k-${e.kind}${e.planet != null ? ' clickable' : ''}" ${e.planet != null ? `data-act="goto-planet" data-id="${e.planet}"` : ''}>` +
    `<span class="d">${num(e.day)}</span>${esc(e.message)}</div>`).join('');
}

function empiresPanel() {
  const s = S();
  if (!s.empires.length) return '<p class="muted">No empires. Create one to start.</p>';
  return s.empires.map((e) => `
    <div class="empire ${ui.sel.kind === 'empire' && ui.sel.id === e.id ? 'sel' : ''}" style="border-left-color:${empireColor(s, e.id)}" data-act="select-empire" data-id="${e.id}">
      <div class="name">${esc(e.display_name)}${e.ai ? '<span class="badge">AI</span>' : ''}${e.at_war.length ? '<span class="badge crisis">at war</span>' : ''}${e.crises.length ? `<span class="badge crisis">${e.crises.length} crisis</span>` : ''}</div>
      <div class="meta">${num(e.stats.pop)} pops · ${e.stats.systems} systems · ${e.stats.planets} planets</div>
      <div class="meta">${s.fleets.filter((f) => f.owner === e.id).reduce((a, f) => a + f.ships.length, 0)} ships · ${num(e.storage[D().rules.expansion_resource] ?? 0)} influence</div>
      <div class="meta">research ${num(e.stats.research_income, 1)}/day${e.researching != null ? ` → ${esc(D().technologies[e.researching].display_name)}` : ''}</div>
    </div>`).join('');
}

function inspector() {
  switch (ui.sel.kind) {
    case 'empire': return empireInspector(ui.sel.id);
    case 'system': return systemInspector(ui.sel.id);
    case 'planet': return planetInspector(ui.sel.id, ui.sel.tile);
    case 'fleet': return fleetInspector(ui.sel.id);
    default: return galaxyInspector();
  }
}

function storageTable(e, editable) {
  const rows = D().resources.map((r) => {
    if (!r.stockpile) return '';
    return `<tr><td>${esc(r.display_name)}</td><td class="num">${num(e.storage[r.id])}</td><td class="num">${signed(e.income[r.id])}</td>
      ${editable ? `<td><input type="number" step="any" data-res="${r.id}" placeholder="set" style="width:70px"></td><td><button class="small" data-act="empire-set-storage" data-id="${e.id}" data-res="${r.id}">set</button></td>` : ''}</tr>`;
  }).join('');
  return `<table><tr><th>Resource</th><th class="num">Stock</th><th class="num">/day</th>${editable ? '<th colspan="2">Admin</th>' : ''}</tr>${rows}</table>`;
}

function galaxyInspector() {
  const s = S();
  const pops = s.planets.reduce((a, p) => a + p.pop, 0);
  const market = s.market.map((m, r) => (m.volume > 0 ? `<tr><td>${esc(resName(r))}</td><td class="num">${num(m.volume, 1)}</td><td class="num">${num(m.price, 2)}</td></tr>` : '')).join('');
  const empireOpts = s.empires.map((e) => `<option value="${e.id}">${esc(e.display_name)}</option>`).join('');
  return `
    <h2>Galaxy</h2>
    <div class="kv"><span>Day</span><span>${num(s.day)}</span><span>Seed</span><span>${s.seed}</span>
      <span>Systems</span><span>${s.systems.length}</span><span>Planets</span><span>${s.planets.length}</span>
      <span>Empires</span><span>${s.empires.length}</span><span>Population</span><span>${num(pops)}</span>
      <span>Fleets</span><span>${s.fleets.length} (${s.fleets.reduce((a, f) => a + f.ships.length, 0)} ships)</span></div>
    ${warsList()}
    <h3>Galactic market (last day)</h3>
    ${market ? `<table><tr><th>Resource</th><th class="num">Volume</th><th class="num">Price</th></tr>${market}</table>` : '<p class="muted">No trades.</p>'}
    <h3>Add system</h3>
    <div class="box">
      <div class="row"><input type="text" id="ns-name" placeholder="Name (random if blank)"></div>
      <div class="row"><label>Coordinates</label><input type="number" id="ns-x" placeholder="x" step="any"><input type="number" id="ns-y" placeholder="y" step="any"></div>
      <div class="row"><label>Owner</label><select id="ns-owner"><option value="">none</option>${empireOpts}</select></div>
      <div class="row"><label><input type="checkbox" id="ns-random" checked>random planets</label><label><input type="checkbox" id="ns-link" checked>link to nearest</label></div>
      <div class="row"><button class="primary" data-act="add-system">Create system</button></div>
    </div>`;
}

function empireInspector(id) {
  const s = S();
  const e = s.empires[id];
  const d = D();
  const available = d.technologies.filter((t) => !e.techs.includes(t.id) && t.prerequisites.every((p) => e.techs.includes(p)));
  const cur = e.researching != null ? d.technologies[e.researching] : null;
  const systems = s.systems.filter((x) => x.owner === id);
  return `
    <h2><span class="swatch" style="background:${empireColor(s, id)}"></span>${esc(e.display_name)}</h2>
    <div class="row"><input type="text" id="emp-name" value="${esc(e.display_name)}"><button class="small" data-act="empire-rename" data-id="${id}">rename</button></div>
    <div class="row">
      <label><input type="checkbox" data-change="empire-ai" data-id="${id}" ${e.ai ? 'checked' : ''}>AI governor</label>
      <label><input type="checkbox" data-change="empire-flag" data-field="accepts_migrants" data-id="${id}" ${e.accepts_migrants ? 'checked' : ''}>migrants</label>
      <label><input type="checkbox" data-change="empire-flag" data-field="market_access" data-id="${id}" ${e.market_access ? 'checked' : ''}>market</label>
      <input type="color" data-change="empire-color" data-id="${id}" value="${e.color || '#9aa4b8'}">
    </div>
    ${e.ai ? `<div class="box">${TRAITS.map((t) => `<div class="row"><label style="min-width:90px">${t}</label>
      <input type="range" min="0" max="1" step="0.05" value="${e.ai[t]}" data-change="empire-trait" data-trait="${t}" data-id="${id}" style="flex:1">
      <span class="muted">${num(e.ai[t], 2)}</span></div>`).join('')}</div>` : ''}
    <div class="kv"><span>Population</span><span>${num(e.stats.pop)}</span><span>Planets</span><span>${e.stats.planets}</span>
      <span>Capital</span><span>${e.capital != null ? `<a class="list-item" data-act="goto-system" data-id="${e.capital}">${esc(s.systems[e.capital].display_name)}</a>` : '–'}</span></div>
    ${e.crises.length ? `<h3>Crises</h3>${e.crises.map((c) => `<div class="neg">${esc(resName(c.resource))}: ${num(c.severity * 100)}% unmet since day ${c.since_day}</div>`).join('')}` : ''}
    <h3>Storage</h3>${storageTable(e, true)}
    <h3>Research (${num(e.stats.research_income, 1)}/day)</h3>
    ${cur ? `<div>${esc(cur.display_name)} — ${num(e.research_progress)}/${num(cur.cost)}</div><div class="bar"><div style="width:${Math.min(100, (e.research_progress / cur.cost) * 100)}%"></div></div>` : '<div class="muted">Nothing (auto-picks cheapest)</div>'}
    <div class="row"><select data-change="empire-research" data-id="${id}"><option value="">— choose research —</option>${available.map((t) => `<option value="${t.id}" ${t.id === e.researching ? 'selected' : ''}>${esc(t.display_name)} (${num(t.cost)})</option>`).join('')}</select></div>
    <h3>Technologies (admin grant/revoke)</h3>
    <div class="techs">${d.technologies.map((t) => `<div><label><input type="checkbox" data-change="empire-tech" data-id="${id}" data-tech="${t.id}" ${e.techs.includes(t.id) ? 'checked' : ''}>${esc(t.display_name)}</label> <span class="muted">${esc(t.category ?? '')}</span></div>`).join('')}</div>
    <h3>Diplomacy</h3>
    ${s.empires.filter((o) => o.id !== id).map((o) => {
      const war = e.wars.find((w) => w.enemy === o.id);
      const truce = e.truces.find((t) => t.with === o.id);
      const status = war ? `at war since day ${war.since_day}` : truce ? `truce until day ${truce.until_day}` : 'at peace';
      return `<div class="row"><span class="swatch" style="background:${empireColor(s, o.id)}"></span>${esc(o.display_name)}
        <span class="${war ? 'neg' : 'muted'}">${status}</span>
        <button class="small ${war ? '' : 'danger'}" data-act="${war ? 'make-peace' : 'declare-war'}" data-id="${id}" data-other="${o.id}">${war ? 'make peace' : 'declare war'}</button></div>`;
    }).join('') || '<p class="muted">No other empires.</p>'}
    <h3>Fleets</h3>
    ${s.fleets.filter((f) => f.owner === id).map((f) => `<div class="list-item" data-act="select-fleet" data-id="${f.id}">${esc(f.display_name)} <span class="muted">${f.ships.length} ships @ ${esc(s.systems[f.system].display_name)}${f.in_combat ? ' · <span class="neg">in combat</span>' : ''}</span></div>`).join('') || '<p class="muted">None</p>'}
    ${e.ship_queue.length ? `<h3>Ship queue</h3>${e.ship_queue.map((o, i) => {
      const total = D().ships[o.ship].build_cost.filter(([r]) => D().resources[r].on_demand).reduce((a, [, c]) => a + c, 0);
      const left = o.remaining.reduce((a, [, c]) => a + c, 0);
      return `<div class="row">${esc(shipName(o.ship))} at ${esc(s.planets[o.planet].display_name)}<button class="small" data-act="cancel-ship" data-id="${id}" data-index="${i}">cancel</button></div>${bar(total ? 1 - left / total : 1)}`;
    }).join('')}` : ''}
    <h3>Spawn fleet (admin)</h3>
    <div class="box">
      <div class="row"><label>System</label><select id="sf-system">${s.systems.map((x) => `<option value="${x.id}" ${x.id === e.capital ? 'selected' : ''}>${esc(x.display_name)}</option>`).join('')}</select></div>
      <div class="row"><select id="sf-ship">${D().ships.map((sh) => `<option value="${sh.id}">${esc(sh.display_name)}</option>`).join('')}</select><input type="number" id="sf-count" value="3" min="1" style="width:60px"></div>
      <div class="row"><button class="small primary" data-act="spawn-fleet" data-id="${id}">Spawn instantly</button></div>
    </div>
    <h3>Systems</h3>
    ${systems.map((x) => `<div class="list-item" data-act="goto-system" data-id="${x.id}">${esc(x.display_name)}${x.id === e.capital ? ' ★' : ''}</div>`).join('') || '<p class="muted">None</p>'}`;
}

function warsList() {
  const s = S();
  const wars = [];
  for (const e of s.empires) for (const o of e.at_war) if (o > e.id) wars.push(`${esc(e.display_name)} ⚔ ${esc(s.empires[o].display_name)}`);
  return wars.length ? `<h3>Wars</h3>${wars.map((w) => `<div class="neg">${w}</div>`).join('')}` : '';
}

function fleetInspector(id) {
  const s = S();
  const d = D();
  const f = fleetById(id);
  const owner = s.empires[f.owner];
  const sys = s.systems[f.system];
  const dest = fleetDestination(s, f);
  let order = 'Idle';
  if (f.order.type === 'move') order = `Moving to ${esc(s.systems[f.order.system].display_name)}${f.order.coordinates ? ` (${f.order.coordinates.map((v) => num(v)).join(', ')})` : ''}`;
  if (f.order.type === 'bombard') order = `Bombarding ${esc(s.planets[f.order.planet].display_name)}${f.order.tile != null ? ` (tile ${f.order.tile})` : ''}`;
  const enemies = (p) => p.owner == null || owner.at_war.includes(p.owner);
  const targets = s.planets.filter((p) => p.owner !== f.owner && enemies(p));
  const mates = s.fleets.filter((o) => o.id !== id && o.owner === f.owner && o.system === f.system);
  const power = f.ships.reduce((a, [def]) => a + d.ships[def].dps, 0);
  return `
    <h2><span class="swatch" style="background:${empireColor(s, f.owner)}"></span>${esc(f.display_name)}</h2>
    <div class="row"><input type="text" id="fl-name" value="${esc(f.display_name)}"><button class="small" data-act="fleet-rename" data-id="${id}">rename</button></div>
    <div class="kv">
      <span>Owner</span><span><a class="list-item" data-act="select-empire" data-id="${f.owner}">${esc(owner.display_name)}</a></span>
      <span>Location</span><span><a class="list-item" data-act="goto-system" data-id="${f.system}">${esc(sys.display_name)}</a> (${f.coordinates.map((v) => num(v)).join(', ')})</span>
      <span>Order</span><span>${order}</span>
      <span>Status</span><span>${f.in_combat ? '<span class="neg">in combat</span>' : 'quiet'}</span>
      <span>Firepower</span><span>${num(power)} dmg/day</span>
    </div>
    ${dest != null && dest !== f.system ? `<div>Hyperdrive charge ${num(f.jump_charge, 1)} / ${num(f.jump_time, 1)} days</div>${bar(f.jump_time ? f.jump_charge / f.jump_time : 1)}` : ''}
    <h3>Orders</h3>
    <div class="row"><select id="fl-dest">${s.systems.map((x) => `<option value="${x.id}" ${x.id === f.system ? 'selected' : ''}>${esc(x.display_name)}</option>`).join('')}</select>
      <button class="small primary" data-act="fleet-move" data-id="${id}">move</button></div>
    <div class="row"><select id="fl-target">${targets.map((p) => `<option value="${p.id}">${esc(p.display_name)} (${p.owner != null ? esc(s.empires[p.owner].display_name) : 'unowned'})</option>`).join('')}</select>
      <button class="small danger" data-act="fleet-bombard" data-id="${id}" ${targets.length ? '' : 'disabled title="no enemy planets (declare war first)"'}>bombard</button></div>
    <div class="row"><button class="small" data-act="fleet-stop" data-id="${id}">stop</button>
      ${mates.length ? `<select id="fl-merge">${mates.map((m) => `<option value="${m.id}">${esc(m.display_name)}</option>`).join('')}</select><button class="small" data-act="fleet-merge" data-id="${id}">absorb</button>` : ''}
      ${f.ships.length > 1 ? `<button class="small" data-act="fleet-split" data-id="${id}">split in half</button>` : ''}</div>
    <h3>Ships (${f.ships.length})</h3>
    <table><tr><th>Design</th><th>Hull</th><th>Shield</th></tr>
    ${f.ships.map(([def, hull, shield]) => {
      const sd = d.ships[def];
      return `<tr><td>${esc(sd.display_name)}</td><td>${bar(hull / sd.hull, '#6bd68a')}</td><td>${sd.shield ? bar(shield / sd.shield, '#6fb3ff') : ''}</td></tr>`;
    }).join('')}</table>
    <h3>Admin</h3>
    <div class="row"><button class="small" data-act="fleet-repair" data-id="${id}">repair</button>
      <select id="fl-owner">${s.empires.map((e) => `<option value="${e.id}" ${e.id === f.owner ? 'selected' : ''}>${esc(e.display_name)}</option>`).join('')}</select>
      <button class="small" data-act="fleet-transfer" data-id="${id}">transfer</button></div>
    <div class="row"><button class="small" data-act="fleet-teleport" data-id="${id}">teleport to selected system</button>
      <button class="small danger" data-act="fleet-disband" data-id="${id}">disband</button></div>`;
}

function systemInspector(id) {
  const s = S();
  const sys = s.systems[id];
  const empireOpts = s.empires.map((e) => `<option value="${e.id}" ${sys.owner === e.id ? 'selected' : ''}>${esc(e.display_name)}</option>`).join('');
  const linkable = s.systems.filter((x) => x.id !== id && !sys.links.includes(x.id));
  return `
    <h2>${esc(sys.display_name)}</h2>
    <div class="row"><button data-act="goto-system" data-id="${id}" class="primary">Enter system</button><button class="danger" data-act="system-delete" data-id="${id}">Delete system</button></div>
    <div class="row"><input type="text" id="sys-name" value="${esc(sys.display_name)}"><button class="small" data-act="system-rename" data-id="${id}">rename</button></div>
    <div class="kv"><span>Star</span><span>${esc(sys.star_type)}${sys.star_count > 1 ? ` ×${sys.star_count}` : ''}</span>
      <span>Coordinates</span><span>${sys.coordinates.map((v) => num(v, 1)).join(', ')}</span></div>
    <h3>Ownership</h3>
    <div class="row"><select data-change="system-owner" data-id="${id}"><option value="">unowned</option>${empireOpts}</select>
      ${sys.owner != null ? `<button class="small" data-act="system-capital" data-id="${id}">make capital</button>` : ''}</div>
    <h3>Hyperlanes</h3>
    ${sys.links.map((l) => `<div class="row"><a class="list-item" data-act="select-system" data-id="${l}">${esc(s.systems[l].display_name)}</a><button class="small danger" data-act="system-unlink" data-id="${id}" data-other="${l}">cut</button></div>`).join('') || '<p class="muted">Not linked</p>'}
    <div class="row"><select id="sys-link">${linkable.map((x) => `<option value="${x.id}">${esc(x.display_name)}</option>`).join('')}</select><button class="small" data-act="system-link" data-id="${id}">link</button></div>
    <h3>Planets</h3>
    ${sys.planets.map((pid) => {
      const p = s.planets[pid];
      const own = p.owner != null ? `<span class="swatch" style="background:${empireColor(s, p.owner)}"></span>` : '';
      return `<div class="list-item" data-act="select-planet" data-id="${pid}">${own}${esc(p.display_name)} <span class="muted">${p.size} ${p.habitable ? 'habitable' : 'barren'}${p.pop ? ` · ${num(p.pop)} pops` : ''}${p.moon_of ? ` · moon of ${esc(p.moon_of)}` : ''}${p.siege > 0 ? ' · <span class="neg">under siege</span>' : ''}</span></div>`;
    }).join('') || '<p class="muted">No planets</p>'}
    <h3>Fleets here</h3>
    ${s.fleets.filter((f) => f.system === id).map((f) => `<div class="list-item" data-act="select-fleet" data-id="${f.id}"><span class="swatch" style="background:${empireColor(s, f.owner)}"></span>${esc(f.display_name)} <span class="muted">${f.ships.length} ships${f.in_combat ? ' · <span class="neg">fighting</span>' : ''}</span></div>`).join('') || '<p class="muted">None</p>'}
    <h3>Add planet</h3>
    <div class="box">
      <div class="row"><input type="text" id="np-name" placeholder="Name"></div>
      <div class="row"><select id="np-size"><option value="large">large (32 tiles)</option><option value="small">small (12 tiles)</option></select>
        <label><input type="checkbox" id="np-hab">habitable</label></div>
      <div class="row"><label>Pops</label><input type="number" id="np-pop" value="0" min="0"><input type="text" id="np-moon" placeholder="moon of (optional)" style="width:120px"></div>
      <div class="row"><button class="primary" data-act="system-add-planet" data-id="${id}">Add planet</button></div>
    </div>`;
}

function planetInspector(id, tile) {
  const s = S();
  const p = s.planets[id];
  const st = p.stats;
  const owner = planetOwner(id);
  const d = D();
  const tf = d.terraforming[p.size];
  const flows = p.net.map((v, r) => (Math.abs(v) > 0.005 ? `<tr><td>${esc(resName(r))}</td><td class="num">${signed(v)}</td></tr>` : '')).join('');
  const others = s.planets.filter((x) => x.id !== id);
  return `
    <h2>${esc(p.display_name)}</h2>
    <div class="row">
      ${ui.view.mode !== 'planet' || ui.view.id !== id ? `<button class="primary" data-act="goto-planet" data-id="${id}">View surface</button>` : ''}
      <button class="danger" data-act="planet-delete" data-id="${id}">Delete planet</button>
    </div>
    <div class="row"><input type="text" id="pl-name" value="${esc(p.display_name)}"><button class="small" data-act="planet-rename" data-id="${id}">rename</button></div>
    <div class="kv">
      <span>System</span><span><a class="list-item" data-act="select-system" data-id="${p.system}">${esc(s.systems[p.system].display_name)}</a></span>
      <span>Owner</span><span><select data-change="planet-owner" data-id="${id}"><option value="">unowned</option>${s.empires.map((e) => `<option value="${e.id}" ${e.id === owner ? 'selected' : ''}>${esc(e.display_name)}</option>`).join('')}</select></span>
      <span>Coordinates</span><span>${p.coordinates.map((v) => num(v, 1)).join(', ')}</span>
      <span>Type</span><span>${p.size}, ${p.habitable ? 'habitable' : 'barren'}${p.terraform_days_left ? ` (terraforming, ${p.terraform_days_left} days)` : ''}</span>
      <span>Population</span><span>${num(p.pop)} / ${num(st.housing)} housing</span>
      <span>Jobs</span><span>${num(st.employed)} filled of ${num(st.jobs)} (${num(st.free_jobs)} free)</span>
      <span>Unemployed</span><span>${num(st.unemployed)}${st.homeless ? `, ${num(st.homeless)} homeless` : ''}</span>
      <span>Amenities</span><span>${num(st.amenities)} / ${num(st.amenities_needed)}</span>
      <span>Stability</span><span class="${st.stability < 25 ? 'neg' : st.stability > 60 ? 'pos' : ''}">${num(st.stability, 1)}</span>
    </div>
    ${p.siege > 0 ? `<div class="box"><span class="neg">Under siege</span>: ${num(p.siege)} / ${num(p.siege_threshold)} until surrender${bar(p.siege / p.siege_threshold, '#ff6b6b')}
      <div class="row"><button class="small" data-act="planet-repair" data-id="${id}">lift siege &amp; repair (admin)</button></div></div>` : ''}
    ${shipyardPanel(id)}
    <h3>Population (admin)</h3>
    <div class="row">
      <button class="small" data-act="pop-add" data-id="${id}" data-n="-100">−100</button>
      <button class="small" data-act="pop-add" data-id="${id}" data-n="-10">−10</button>
      <button class="small" data-act="pop-add" data-id="${id}" data-n="10">+10</button>
      <button class="small" data-act="pop-add" data-id="${id}" data-n="100">+100</button>
      <input type="number" id="pl-pop" min="0" placeholder="set"><button class="small" data-act="pop-set" data-id="${id}">set</button>
    </div>
    <div class="row"><label>Move</label><input type="number" id="mv-count" min="1" value="50">
      <select id="mv-to">${others.map((x) => `<option value="${x.id}">${esc(x.display_name)}</option>`).join('')}</select>
      <button class="small" data-act="move-pops" data-id="${id}">move</button></div>
    <h3>World</h3>
    <div class="row">
      <label><input type="checkbox" data-change="planet-habitable" data-id="${id}" ${p.habitable ? 'checked' : ''}>habitable (admin)</label>
      ${!p.habitable && tf ? `<button class="small" data-act="terraform" data-id="${id}" title="${esc(costStr(tf.cost))}, ${tf.days} days">Terraform</button>` : ''}
      ${p.terraform_days_left ? `<button class="small" data-act="terraform-instant" data-id="${id}">finish now</button>` : ''}
    </div>
    ${flows ? `<h3>Net production / day</h3><table>${flows}</table>` : ''}
    ${tile != null ? tileInspector(id, tile) : `<p class="muted">${ui.view.mode === 'planet' ? 'Click a tile to manage it.' : 'Open the surface to manage tiles.'}</p>`}`;
}

function shipyardPanel(pid) {
  const s = S();
  const d = D();
  const p = s.planets[pid];
  const yard = p.tiles.some((t) => t.b != null && d.buildings[t.b].shipyard && !t.off && (t.dmg ?? 0) < d.rules.building_disable_damage);
  if (!yard || p.owner == null) return '';
  const owner = p.owner;
  const designs = d.ships.map((sh) => {
    const ok = techsMet(owner, sh.requires_tech);
    return `<option value="${sh.id}" ${ok ? '' : 'disabled'}>${ok ? '' : '✕ '}${esc(sh.display_name)} — ${esc(costStr(sh.build_cost))}</option>`;
  }).join('');
  const queued = s.empires[owner].ship_queue.filter((o) => o.planet === pid);
  return `<h3>Shipyard</h3>
    <div class="row"><select id="ship-design">${designs}</select></div>
    <div class="row"><button class="small primary" data-act="build-ship" data-id="${pid}">order ship</button>
      <span class="muted">${queued.length ? `${queued.length} queued here` : 'idle (no workers until ships are ordered)'}</span></div>`;
}

function tileInspector(pid, i) {
  const s = S();
  const d = D();
  const p = s.planets[pid];
  const t = p.tiles[i] || {};
  const owner = planetOwner(pid);
  const neighbors = d.geometry[p.size].neighbors[i];
  const mults = (t.m || []).map(([r, m]) => `${esc(resName(r))} ×${m}`).join(', ');
  let html = `<h3>Tile ${i} <span class="muted">(${t.wa ? 'water' : 'land'}, ${d.geometry[p.size].sides[i]}-sided, next to ${neighbors.join(', ')})</span></h3>`;
  if (mults) html += `<div>Resources: ${mults}</div>`;

  if (t.c) {
    const what = t.c.building != null ? d.buildings[t.c.building].display_name : d.upgrades[t.c.upgrade].display_name;
    const who = t.c.empire != null && t.c.empire !== p.owner ? ` (claim by ${esc(s.empires[t.c.empire].display_name)})` : '';
    html += `<div class="box">Constructing <b>${esc(what)}</b>${who}: ${t.c.days} days left
      <div class="row"><button class="small" data-act="tile-cancel" data-id="${pid}" data-tile="${i}">cancel (admin)</button></div></div>`;
  }
  if (t.b != null && d.buildings[t.b].blocker) {
    const b = d.buildings[t.b];
    const payer = p.owner ?? s.systems[p.system].owner;
    html += `<div class="box">
      <div><b>${esc(b.display_name)}</b> <span class="muted">natural blocker</span></div>
      ${b.description ? `<div class="muted">${esc(b.description)}</div>` : ''}
      ${b.amenities ? `<div>Amenities: ${num(b.amenities)}</div>` : ''}
      <div>Clearing costs ${esc(costStr(b.remove_cost))}${payer == null ? ' <span class="muted">(claim the system first)</span>' : ''}</div>
      <div class="row"><button class="small" data-act="tile-demolish" data-id="${pid}" data-tile="${i}" ${payer == null ? 'disabled' : ''}>clear</button>
        <button class="small" data-act="tile-clear" data-id="${pid}" data-tile="${i}">remove now (admin)</button></div></div>`;
  } else if (t.b != null) {
    const b = d.buildings[t.b];
    const installed = t.u || [];
    const upOpts = b.upgrades.filter((u) => !installed.includes(u)).map((u) => {
      const up = d.upgrades[u];
      const ready = up.needs.every((n) => installed.includes(n)) && techsMet(owner, up.requires_tech);
      return `<option value="${u}">${esc(up.display_name)}${ready ? '' : ' (locked)'} — ${esc(costStr(up.cost))}</option>`;
    }).join('');
    html += `<div class="box">
      <div><span class="swatch" style="background:${buildingColor(d, t.b)}"></span><b>${esc(b.display_name)}</b>${t.off ? ' <span class="badge crisis">disabled</span>' : ''}</div>
      ${b.description ? `<div class="muted">${esc(b.description)}</div>` : ''}
      ${t.dmg ? `<div class="neg">Damage ${pct(t.dmg)}${t.dmg >= d.rules.building_disable_damage ? ' (out of action)' : ''}</div>${bar(1 - t.dmg, '#ff6b6b')}` : ''}
      ${b.max_workers ? `<div>Workers: ${t.w || 0}/${b.max_workers} ${b.job != null ? `(${esc(d.jobs[b.job].display_name)}, appeal ${d.jobs[b.job].appeal})` : ''}${t.cap != null ? ` · cap ${t.cap}${t.pin ? ' pinned' : ''}` : ''}</div>` : '<div class="muted">Automated (no workers)</div>'}
      <div>Out: ${esc(costStr(b.outputs) === 'free' ? '–' : costStr(b.outputs))}</div>
      ${b.inputs.length ? `<div>In: ${esc(costStr(b.inputs))}</div>` : ''}
      ${b.upkeep.length ? `<div>Upkeep: ${esc(costStr(b.upkeep))}</div>` : ''}
      ${installed.length ? `<div>Upgrades: ${installed.map((u) => esc(d.upgrades[u].display_name)).join(', ')}</div>` : ''}
      ${b.max_workers ? `<div class="row"><label>Jobs</label><input type="number" id="tile-cap" min="0" max="${b.max_workers}" value="${t.cap ?? ''}" placeholder="all">
        <button class="small" data-act="tile-cap" data-id="${pid}" data-tile="${i}">limit</button>
        <button class="small" data-act="tile-pin" data-id="${pid}" data-tile="${i}" title="Assign exactly this many workers first">pin</button>
        <button class="small" data-act="tile-unpin" data-id="${pid}" data-tile="${i}">clear</button></div>` : ''}
      <div class="row"><button class="small" data-act="tile-toggle" data-id="${pid}" data-tile="${i}" data-on="${t.off ? 1 : 0}">${t.off ? 'enable' : 'disable'}</button>
        <button class="small danger" data-act="tile-demolish" data-id="${pid}" data-tile="${i}" title="${b.remove_cost.length ? esc(costStr(b.remove_cost)) : 'free'}">demolish</button></div>
      ${upOpts ? `<div class="row"><select id="tile-upgrade">${upOpts}</select></div>
        <div class="row"><button class="small" data-act="tile-upgrade" data-id="${pid}" data-tile="${i}">order upgrade</button>
        <button class="small" data-act="tile-upgrade-instant" data-id="${pid}" data-tile="${i}">install now (admin)</button></div>` : ''}
    </div>`;
  } else if (!t.c) {
    // Who would build here: the planet's owner, the system's owner, or (in unowned systems)
    // any empire staking a claim.
    const sysOwner = s.systems[p.system].owner;
    const builders = p.owner != null ? [p.owner] : sysOwner != null ? [sysOwner] : s.empires.map((e) => e.id);
    const builder = builders.includes(ui.builder) ? ui.builder : builders[0];
    const exp = builder != null ? expansionCost(builder, p.system) : null;
    const counts = {};
    p.tiles.forEach((x) => x.b != null && (counts[x.b] = (counts[x.b] || 0) + 1));
    const opts = d.buildings.map((b) => {
      const terrain = !b.space && !(b.land && t.wa) && !(b.water && !t.wa);
      const legal = b.buildable && terrain && (p.habitable || b.barren) && techsMet(builder, b.requires_tech) && (b.max_per_planet == null || (counts[b.id] || 0) < b.max_per_planet);
      return { b, legal };
    }).sort((x, y) => y.legal - x.legal || x.b.display_name.localeCompare(y.b.display_name))
      .map(({ b, legal }) => `<option value="${b.id}">${legal ? '' : '✕ '}${esc(b.display_name)} — ${esc(costStr(b.build_cost))}, ${b.build_days}d</option>`).join('');
    const expText = !exp ? '' : exp.unreachable
      ? '<div class="neg">No hyperlane route from that empire\'s borders.</div>'
      : `<div class="pos">Expansion claim: +${num(exp.influence)} influence, +${exp.days} days (${exp.jumps} jump${exp.jumps === 1 ? '' : 's'} from the border). Finishing claims the system.</div>`;
    html += `<div class="box"><div class="muted">Empty tile. ✕ = not allowed by the rules here (admin placement ignores rules).</div>
      ${builders.length > 1 || p.owner == null ? `<div class="row"><label>Builder</label><select data-change="builder">${builders.map((e) => `<option value="${e}" ${e === builder ? 'selected' : ''}>${esc(s.empires[e].display_name)}</option>`).join('')}</select></div>` : ''}
      ${expText}
      <div class="row"><select id="tile-building">${opts}</select></div>
      <div class="row"><button class="small primary" data-act="tile-build" data-id="${pid}" data-tile="${i}" data-empire="${builder ?? ''}" ${builder == null ? 'disabled title="no empire can build here"' : ''}>order construction</button>
        <button class="small" data-act="tile-place" data-id="${pid}" data-tile="${i}">place now (admin)</button></div></div>`;
  }
  html += `<div class="row"><button class="small" data-act="tile-water" data-id="${pid}" data-tile="${i}" data-wet="${t.wa ? 0 : 1}">${t.wa ? 'drain' : 'flood'} tile (admin)</button></div>`;
  return html;
}

// ---------------------------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------------------------

const intOr = (v, d = null) => (v === '' || v == null ? d : Number(v));

const actions = {
  'goto-galaxy': () => { ui.view = { mode: 'galaxy', id: null }; },
  'goto-system': (el) => { const id = +el.dataset.id; ui.view = { mode: 'system', id }; ui.sel = { kind: 'system', id }; },
  'goto-planet': (el) => { const id = +el.dataset.id; ui.view = { mode: 'planet', id }; ui.sel = { kind: 'planet', id, tile: null }; },
  'select-empire': (el) => { ui.sel = { kind: 'empire', id: +el.dataset.id }; },
  'select-system': (el) => { ui.sel = { kind: 'system', id: +el.dataset.id }; },
  'select-planet': (el) => { ui.sel = { kind: 'planet', id: +el.dataset.id, tile: null }; },

  'add-system': async () => {
    const x = $('#ns-x').value;
    const y = $('#ns-y').value;
    const r = await api.createSystem({
      display_name: $('#ns-name').value || null,
      coordinates: x !== '' && y !== '' ? [+x, +y] : null,
      owner: intOr($('#ns-owner').value),
      random_planets: $('#ns-random').checked,
      link_nearest: $('#ns-link').checked,
    });
    ui.sel = { kind: 'system', id: r.id };
    toast(`Created ${S().systems[r.id].display_name}`);
  },

  'empire-rename': (el) => api.patchEmpire(+el.dataset.id, { display_name: $('#emp-name').value }),
  'empire-set-storage': (el) => {
    const input = $(`#inspector input[data-res="${el.dataset.res}"]`);
    if (input.value === '') return;
    return api.setStorage(+el.dataset.id, +el.dataset.res, +input.value);
  },

  'system-rename': (el) => api.patchSystem(+el.dataset.id, { display_name: $('#sys-name').value }),
  'system-capital': (el) => api.command({ type: 'set_capital', empire: S().systems[+el.dataset.id].owner, system: +el.dataset.id }),
  'system-link': (el) => { const o = $('#sys-link').value; if (o !== '') return api.command({ type: 'link', a: +el.dataset.id, b: +o }); },
  'system-unlink': (el) => api.command({ type: 'unlink', a: +el.dataset.id, b: +el.dataset.other }),
  'system-delete': async (el) => {
    const id = +el.dataset.id;
    if (!confirm(`Delete ${S().systems[id].display_name} and all its planets?`)) return;
    ui.sel = { kind: 'galaxy' };
    ui.view = { mode: 'galaxy', id: null };
    await api.deleteSystem(id);
  },
  'system-add-planet': async (el) => {
    const name = $('#np-name').value.trim();
    if (!name) throw new Error('the planet needs a name');
    const r = await api.createPlanet(+el.dataset.id, {
      display_name: name,
      size: $('#np-size').value,
      habitable: $('#np-hab').checked,
      pop: +$('#np-pop').value || 0,
      moon_of: $('#np-moon').value || null,
    });
    ui.sel = { kind: 'planet', id: r.id, tile: null };
  },

  'planet-rename': (el) => api.patchPlanet(+el.dataset.id, { display_name: $('#pl-name').value }),
  'planet-delete': async (el) => {
    const id = +el.dataset.id;
    if (!confirm(`Delete ${S().planets[id].display_name}?`)) return;
    const sys = S().planets[id].system;
    ui.sel = { kind: 'system', id: sys };
    if (ui.view.mode === 'planet') ui.view = { mode: 'system', id: sys };
    await api.deletePlanet(id);
  },
  'pop-add': (el) => api.patchPlanet(+el.dataset.id, { add_pops: +el.dataset.n }),
  'pop-set': (el) => { const v = $('#pl-pop').value; if (v !== '') return api.patchPlanet(+el.dataset.id, { pop: +v }); },
  'move-pops': (el) => api.command({ type: 'move_pops', from: +el.dataset.id, to: +$('#mv-to').value, count: +$('#mv-count').value }),
  terraform: (el) => api.command({ type: 'terraform', planet: +el.dataset.id }),
  'terraform-instant': (el) => api.patchPlanet(+el.dataset.id, { terraform_days_left: 0 }),

  'tile-build': (el) => api.command({ type: 'build', planet: +el.dataset.id, tile: +el.dataset.tile, building: +$('#tile-building').value, empire: intOr(el.dataset.empire) }),
  'select-fleet': (el) => { ui.sel = { kind: 'fleet', id: +el.dataset.id }; },
  'declare-war': (el) => api.command({ type: 'declare_war', empire: +el.dataset.id, target: +el.dataset.other }),
  'make-peace': (el) => api.command({ type: 'make_peace', empire: +el.dataset.id, target: +el.dataset.other }),
  'cancel-ship': (el) => api.command({ type: 'cancel_ship_order', empire: +el.dataset.id, index: +el.dataset.index }),
  'spawn-fleet': async (el) => {
    const r = await api.createFleet({
      empire: +el.dataset.id,
      system: +$('#sf-system').value,
      ships: [{ ship: +$('#sf-ship').value, count: +$('#sf-count').value || 1 }],
    });
    ui.sel = { kind: 'fleet', id: r.id };
  },
  'build-ship': (el) => api.command({ type: 'build_ship', planet: +el.dataset.id, ship: +$('#ship-design').value }),
  'planet-repair': (el) => api.patchPlanet(+el.dataset.id, { repair: true }),
  'fleet-rename': (el) => api.command({ type: 'rename_fleet', fleet: +el.dataset.id, display_name: $('#fl-name').value }),
  'fleet-move': (el) => api.command({ type: 'move_fleet', fleet: +el.dataset.id, system: +$('#fl-dest').value }),
  'fleet-bombard': (el) => api.command({ type: 'bombard', fleet: +el.dataset.id, planet: +$('#fl-target').value }),
  'fleet-stop': (el) => api.command({ type: 'stop_fleet', fleet: +el.dataset.id }),
  'fleet-merge': (el) => api.command({ type: 'merge_fleets', into: +el.dataset.id, from: +$('#fl-merge').value }),
  'fleet-split': (el) => {
    const n = fleetById(+el.dataset.id).ships.length;
    return api.command({ type: 'split_fleet', fleet: +el.dataset.id, ships: [...Array(Math.floor(n / 2)).keys()] });
  },
  'fleet-repair': (el) => api.patchFleet(+el.dataset.id, { repair: true }),
  'fleet-transfer': (el) => api.patchFleet(+el.dataset.id, { owner: +$('#fl-owner').value }),
  'fleet-teleport': (el) => {
    const target = ui.view.mode === 'system' ? ui.view.id : +$('#fl-dest').value;
    return api.patchFleet(+el.dataset.id, { system: target });
  },
  'fleet-disband': (el) => {
    if (!confirm('Disband this fleet?')) return;
    return api.deleteFleet(+el.dataset.id);
  },
  'tile-place': async (el) => {
    const r = await api.editTile(+el.dataset.id, +el.dataset.tile, { building: +$('#tile-building').value });
    if (r.warning) toast(r.warning);
  },
  'tile-demolish': (el) => api.command({ type: 'demolish', planet: +el.dataset.id, tile: +el.dataset.tile }),
  'tile-clear': (el) => api.editTile(+el.dataset.id, +el.dataset.tile, { building: null }),
  'tile-water': async (el) => {
    const r = await api.editTile(+el.dataset.id, +el.dataset.tile, { water: el.dataset.wet === '1' });
    if (r.warning) toast(r.warning);
  },
  'tile-cancel': (el) => api.editTile(+el.dataset.id, +el.dataset.tile, { cancel_construction: true }),
  'tile-upgrade': (el) => api.command({ type: 'upgrade', planet: +el.dataset.id, tile: +el.dataset.tile, upgrade: +$('#tile-upgrade').value }),
  'tile-upgrade-instant': (el) => {
    const t = S().planets[+el.dataset.id].tiles[+el.dataset.tile];
    const u = +$('#tile-upgrade').value;
    // Admin install also pulls in any upgrades it depends on.
    const want = new Set(t.u || []);
    const add = (id) => { D().upgrades[id].needs.forEach(add); want.add(id); };
    add(u);
    return api.editTile(+el.dataset.id, +el.dataset.tile, { upgrades: [...want] });
  },
  'tile-cap': (el) => api.command({ type: 'set_job_cap', planet: +el.dataset.id, tile: +el.dataset.tile, cap: intOr($('#tile-cap').value) }),
  'tile-pin': (el) => api.command({ type: 'assign_workers', planet: +el.dataset.id, tile: +el.dataset.tile, workers: intOr($('#tile-cap').value, 0) }),
  'tile-unpin': (el) => api.command({ type: 'set_job_cap', planet: +el.dataset.id, tile: +el.dataset.tile, cap: null }),
  'tile-toggle': (el) => api.command({ type: 'set_enabled', planet: +el.dataset.id, tile: +el.dataset.tile, enabled: el.dataset.on === '1' }),

  'world-regenerate': async () => {
    const r = await api.regenerate({
      systems: +$('#gen-systems').value,
      empires: +$('#gen-empires').value,
      seed: intOr($('#gen-seed').value),
      ai: $('#gen-ai').checked,
    });
    resetView();
    closeModal();
    toast(`Generated a new galaxy (seed ${r.seed})`);
  },
  'world-load': async () => {
    const r = await api.load($('#load-file').value);
    resetView();
    closeModal();
    toast(r.warnings?.length ? `Loaded with warnings: ${r.warnings.join('; ')}` : 'Loaded');
  },
  'world-save': async () => {
    const r = await api.save($('#save-name').value || `day-${S().day}`);
    toast(`Saved to data/${r.path}`);
    closeModal();
  },
  'empire-create': async () => {
    const r = await api.createEmpire({
      display_name: $('#ne-name').value || 'New Empire',
      color: $('#ne-color').value,
      ai: $('#ne-ai').checked ? Object.fromEntries(TRAITS.map((t) => [t, +$(`#ne-${t}`).value])) : null,
      capital: intOr($('#ne-capital').value),
      starting_resources: $('#ne-stock').checked,
    });
    closeModal();
    ui.sel = { kind: 'empire', id: r.id };
  },
  'modal-close': () => closeModal(),
};

const TRAITS = ['militant', 'expansionist', 'scientific'];

const changes = {
  'empire-ai': (el) => api.patchEmpire(+el.dataset.id, { ai: el.checked ? { militant: 0.5, expansionist: 0.5, scientific: 0.5 } : null }),
  'empire-trait': (el) => {
    const ai = { ...S().empires[+el.dataset.id].ai, [el.dataset.trait]: +el.value };
    return api.patchEmpire(+el.dataset.id, { ai });
  },
  'planet-owner': (el) => api.command({ type: 'set_planet_owner', planet: +el.dataset.id, owner: intOr(el.value) }),
  builder: (el) => { ui.builder = intOr(el.value); },
  'empire-flag': (el) => api.patchEmpire(+el.dataset.id, { [el.dataset.field]: el.checked }),
  'empire-color': (el) => api.patchEmpire(+el.dataset.id, { color: el.value }),
  'empire-research': (el) => api.command({ type: 'set_research', empire: +el.dataset.id, tech: intOr(el.value) }),
  'empire-tech': (el) => api.setTech(+el.dataset.id, +el.dataset.tech, el.checked),
  'system-owner': (el) => api.command({ type: 'set_owner', system: +el.dataset.id, owner: intOr(el.value) }),
  'planet-habitable': (el) => api.patchPlanet(+el.dataset.id, { habitable: el.checked }),
};

async function run(fn, el) {
  try {
    await fn(el);
  } catch (e) {
    toast(e.message, true);
  }
  el?.blur?.();
  render();
}

document.addEventListener('click', (ev) => {
  const el = ev.target.closest('[data-act]');
  if (el && actions[el.dataset.act]) run(actions[el.dataset.act], el);
});
document.addEventListener('change', (ev) => {
  const el = ev.target.closest('[data-change]');
  if (el && changes[el.dataset.change]) run(changes[el.dataset.change], el);
});

function resetView() {
  ui.view = { mode: 'galaxy', id: null };
  ui.sel = { kind: 'galaxy' };
}

// ---------------------------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------------------------

function openModal(html) {
  $('#modal-body').innerHTML = html;
  $('#modal').hidden = false;
}
function closeModal() {
  $('#modal').hidden = true;
}
$('#modal').addEventListener('click', (ev) => { if (ev.target.id === 'modal') closeModal(); });

$('#world-btn').addEventListener('click', async () => {
  let files = [];
  try { files = await api.files(); } catch (e) { toast(e.message, true); }
  openModal(`
    <h2>World</h2>
    <h3>Generate a new galaxy</h3>
    <div class="row"><label>Systems</label><input type="number" id="gen-systems" value="80" min="1" max="20000"></div>
    <div class="row"><label>Empires</label><input type="number" id="gen-empires" value="5" min="0"></div>
    <div class="row"><label>Seed</label><input type="number" id="gen-seed" placeholder="random"></div>
    <div class="row"><label><input type="checkbox" id="gen-ai" checked>empires run by AI</label></div>
    <div class="row"><button class="primary" data-act="world-regenerate">Regenerate</button></div>
    <h3>Load</h3>
    <div class="row"><select id="load-file">${files.map((f) => `<option>${esc(f)}</option>`).join('')}</select><button data-act="world-load">Load</button></div>
    <h3>Save</h3>
    <div class="row"><input type="text" id="save-name" placeholder="day-${S().day}" style="width:200px"><button data-act="world-save">Save to data/saves</button></div>
    <div class="row" style="justify-content:flex-end"><button data-act="modal-close">Close</button></div>`);
});

$('#new-empire').addEventListener('click', () => {
  const s = S();
  const free = s.systems.filter((x) => x.owner == null);
  const hue = Math.floor(Math.random() * 360);
  openModal(`
    <h2>New empire</h2>
    <div class="row"><label>Name</label><input type="text" id="ne-name" placeholder="New Empire" style="width:220px"></div>
    <div class="row"><label>Color</label><input type="color" id="ne-color" value="${hslHex(hue)}"></div>
    <div class="row"><label>Capital</label><select id="ne-capital"><option value="">none (no territory)</option>
      ${free.map((x) => `<option value="${x.id}" ${ui.sel.kind === 'system' && ui.sel.id === x.id ? 'selected' : ''}>${esc(x.display_name)} (${x.planets.length} planets)</option>`).join('')}</select></div>
    <div class="row"><label><input type="checkbox" id="ne-ai" checked>AI governor</label><label><input type="checkbox" id="ne-stock" checked>starting stockpile</label></div>
    ${TRAITS.map((t) => `<div class="row"><label>${t}</label><input type="range" id="ne-${t}" min="0" max="1" step="0.05" value="0.5" style="flex:1"></div>`).join('')}
    <div class="row" style="justify-content:flex-end"><button data-act="modal-close">Cancel</button><button class="primary" data-act="empire-create">Create</button></div>`);
});

function hslHex(h) {
  const f = (n) => {
    const k = (n + h / 30) % 12;
    const c = 0.6 - 0.35 * Math.max(Math.min(k - 3, 9 - k, 1), -1);
    return Math.round(c * 255).toString(16).padStart(2, '0');
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}

// ---------------------------------------------------------------------------------------------
// Scene interaction and the play loop
// ---------------------------------------------------------------------------------------------

function describe(pick) {
  const s = S();
  const d = D();
  if (pick.kind === 'system') {
    const x = s.systems[pick.id];
    const pops = x.planets.reduce((a, p) => a + s.planets[p].pop, 0);
    return `${x.display_name}\n${x.owner != null ? s.empires[x.owner].display_name : 'unowned'} · ${x.star_type}\n${x.planets.length} planets · ${num(pops)} pops`;
  }
  if (pick.kind === 'planet') {
    const p = s.planets[pick.id];
    const owner = p.owner != null ? s.empires[p.owner].display_name : 'unowned';
    return `${p.display_name} (${owner})\n${p.size} ${p.habitable ? 'habitable' : 'barren'}${p.moon_of ? `, moon of ${p.moon_of}` : ''}\n${num(p.pop)} pops · stability ${num(p.stats.stability)}${p.siege > 0 ? `\nunder siege ${pct(p.siege / p.siege_threshold)}` : ''}`;
  }
  if (pick.kind === 'fleet') {
    const f = fleetById(pick.id);
    if (!f) return null;
    const counts = {};
    for (const [def] of f.ships) counts[def] = (counts[def] || 0) + 1;
    const ships = Object.entries(counts).map(([def, n]) => `${n}× ${shipName(+def)}`).join(', ');
    return `${f.display_name} (${s.empires[f.owner].display_name})\n${ships}${f.in_combat ? '\nIN COMBAT' : ''}`;
  }
  if (pick.kind === 'tile') {
    const t = s.planets[pick.planet].tiles[pick.tile] || {};
    const lines = [`Tile ${pick.tile}${t.wa ? ' (water)' : ''}`];
    if (t.b != null && d.buildings[t.b].blocker) {
      lines.push(`${d.buildings[t.b].display_name} (natural)`, `clear: ${costStr(d.buildings[t.b].remove_cost)}`);
    } else if (t.b != null) {
      const b = d.buildings[t.b];
      lines.push(b.display_name + (b.max_workers ? ` (${t.w || 0}/${b.max_workers})` : ''));
      if (t.dmg) lines.push(`  damaged ${pct(t.dmg)}`);
      for (const u of t.u || []) lines.push(`  + ${d.upgrades[u].display_name}`);
    }
    if (t.c) lines.push(`constructing (${t.c.days}d)`);
    for (const [r, m] of t.m || []) lines.push(`${resName(r)} ×${m}`);
    return lines.join('\n');
  }
  return null;
}

function onPick(pick) {
  if (!pick) return;
  if (pick.kind === 'system') ui.sel = { kind: 'system', id: pick.id };
  else if (pick.kind === 'planet') ui.sel = { kind: 'planet', id: pick.id, tile: null };
  else if (pick.kind === 'tile') ui.sel = { kind: 'planet', id: pick.planet, tile: pick.tile };
  else if (pick.kind === 'fleet') ui.sel = { kind: 'fleet', id: pick.id };
  render();
}

/// Right-click: give the selected fleet an order based on what was clicked.
async function onContext(pick, point) {
  if (ui.sel.kind !== 'fleet') return;
  const f = fleetById(ui.sel.id);
  if (!f) return;
  const s = S();
  try {
    if (pick?.kind === 'planet' || pick?.kind === 'tile') {
      const pid = pick.kind === 'planet' ? pick.id : pick.planet;
      const p = s.planets[pid];
      if (p.owner !== f.owner && (p.owner == null || s.empires[f.owner].at_war.includes(p.owner))) {
        await api.command({ type: 'bombard', fleet: f.id, planet: pid, tile: pick.kind === 'tile' ? pick.tile : null });
        toast(`${f.display_name}: bombard ${p.display_name}`);
      } else {
        await api.command({ type: 'move_fleet', fleet: f.id, system: p.system, coordinates: p.coordinates });
        toast(`${f.display_name}: move to ${p.display_name}`);
      }
    } else if (pick?.kind === 'system' && ui.view.mode === 'galaxy') {
      await api.command({ type: 'move_fleet', fleet: f.id, system: pick.id });
      toast(`${f.display_name}: move to ${s.systems[pick.id].display_name}`);
    } else if (ui.view.mode === 'system' && point) {
      await api.command({ type: 'move_fleet', fleet: f.id, system: ui.view.id, coordinates: point.map((v) => Math.round(v * 10) / 10) });
    }
  } catch (e) {
    toast(e.message, true);
  }
  render();
}

function onOpen(pick) {
  if (!pick) return;
  if (pick.kind === 'system') ui.view = { mode: 'system', id: pick.id };
  else if (pick.kind === 'planet') ui.view = { mode: 'planet', id: pick.id };
  render();
}

async function step() {
  if (ui.busy) return;
  ui.busy = true;
  try {
    await tick(+$('#speed').value);
  } catch (e) {
    toast(e.message, true);
    ui.playing = false;
  } finally {
    ui.busy = false;
  }
}

async function playLoop() {
  while (ui.playing) {
    const started = performance.now();
    await step();
    // Aim for at most ~6 requests per second.
    const wait = Math.max(0, 160 - (performance.now() - started));
    await new Promise((r) => setTimeout(r, wait));
  }
  render();
}

function togglePlay() {
  ui.playing = !ui.playing;
  render();
  if (ui.playing) playLoop();
}

$('#play').addEventListener('click', togglePlay);
$('#step').addEventListener('click', () => step());
document.addEventListener('keydown', (ev) => {
  if (ev.code === 'Space' && !['INPUT', 'SELECT', 'TEXTAREA'].includes(document.activeElement?.tagName)) {
    ev.preventDefault();
    togglePlay();
  }
  if (ev.code === 'Escape') closeModal();
});

scene = new Scene($('#view'), { onPick, onOpen, onContext, describe, tooltip: $('#tooltip') });
subscribe(() => render());
loadAll().catch((e) => toast(`Could not reach the server: ${e.message}`, true));
