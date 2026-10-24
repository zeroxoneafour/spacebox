// Server communication and the client-side copy of the game state.
//
// The client keeps the last state it received plus its version. Ticks and syncs send that
// version, and the server answers with a list of diff ops (or the full state if it no longer
// has that version cached). See spacebox-web/src/diff.rs for the op format.

export const store = {
  defs: null,
  state: null,
  version: null,
  epoch: null,
  eventSeq: 0,
  events: [],
  lastBytes: 0,
  lastOps: null,
  listeners: new Set(),
};

export function subscribe(fn) {
  store.listeners.add(fn);
}

function notify(reason) {
  for (const fn of store.listeners) fn(reason);
}

async function request(method, url, body) {
  const res = await fetch(url, {
    method,
    headers: body === undefined ? {} : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let data;
  try {
    data = text ? JSON.parse(text) : {};
  } catch {
    throw new Error(`${res.status}: ${text}`);
  }
  if (!res.ok) throw new Error(data.error || `${res.status} ${res.statusText}`);
  return { data, bytes: text.length };
}

/// Applies diff ops to `root` in place.
export function applyOps(root, ops) {
  for (const op of ops) {
    const path = op.p;
    if (path.length === 0) continue;
    let cur = root;
    for (let i = 0; i < path.length - 1; i++) cur = cur[path[i]];
    const last = path[path.length - 1];
    if ('v' in op) cur[last] = op.v;
    else if ('d' in op) delete cur[last];
    else if ('n' in op) cur[last].length = op.n;
  }
}

function pushEvents(events) {
  if (!events?.length) return;
  store.events.push(...events);
  if (store.events.length > 500) store.events.splice(0, store.events.length - 500);
  store.eventSeq = Math.max(store.eventSeq, ...events.map((e) => e.seq));
}

export async function loadDefs() {
  store.defs = (await request('GET', '/api/defs')).data;
}

export async function loadAll() {
  await loadDefs();
  const { data, bytes } = await request('GET', '/api/state');
  store.state = data.state;
  store.version = data.version;
  store.epoch = data.epoch;
  store.events = [];
  store.eventSeq = 0;
  pushEvents(data.events);
  store.eventSeq = data.event_seq;
  store.lastBytes = bytes;
  store.lastOps = null;
  notify('full');
}

async function applyResponse({ data, bytes }) {
  if (data.epoch !== store.epoch) {
    // A different world was loaded: definitions may have changed too.
    await loadAll();
    return;
  }
  store.lastBytes = bytes;
  if (data.state) {
    store.state = data.state;
    store.lastOps = null;
  } else {
    applyOps(store.state, data.ops);
    store.lastOps = data.ops.length;
  }
  store.version = data.version;
  pushEvents(data.events);
  store.eventSeq = Math.max(store.eventSeq, data.event_seq);
  notify(data.state ? 'full' : 'diff');
}

function known() {
  return { since: store.version, epoch: store.epoch, since_event: store.eventSeq };
}

/// Advances the simulation and pulls the resulting diff.
export async function tick(days) {
  await applyResponse(await request('POST', '/api/tick', { days, ...known() }));
}

/// Pulls changes without advancing time (after admin edits).
export async function sync() {
  await applyResponse(await request('POST', '/api/sync', known()));
}

async function mutate(method, url, body) {
  const { data } = await request(method, url, body);
  await sync();
  return data;
}

export const api = {
  command: (cmd) => mutate('POST', '/api/command', cmd),
  createEmpire: (body) => mutate('POST', '/api/empires', body),
  patchEmpire: (id, body) => mutate('PATCH', `/api/empires/${id}`, body),
  setTech: (id, tech, isKnown) => mutate('POST', `/api/empires/${id}/techs`, { tech, known: isKnown }),
  setStorage: (id, resource, count) => mutate('POST', `/api/empires/${id}/storage`, { resource, count }),
  createSystem: (body) => mutate('POST', '/api/systems', body),
  patchSystem: (id, body) => mutate('PATCH', `/api/systems/${id}`, body),
  deleteSystem: (id) => mutate('DELETE', `/api/systems/${id}`),
  createPlanet: (system, body) => mutate('POST', `/api/systems/${system}/planets`, body),
  patchPlanet: (id, body) => mutate('PATCH', `/api/planets/${id}`, body),
  deletePlanet: (id) => mutate('DELETE', `/api/planets/${id}`),
  editTile: (planet, tile, body) => mutate('PUT', `/api/planets/${planet}/tiles/${tile}`, body),
  createFleet: (body) => mutate('POST', '/api/fleets', body),
  patchFleet: (id, body) => mutate('PATCH', `/api/fleets/${id}`, body),
  deleteFleet: (id) => mutate('DELETE', `/api/fleets/${id}`),
  regenerate: (body) => mutate('POST', '/api/world/regenerate', body),
  load: (path) => mutate('POST', '/api/world/load', { path }),
  save: async (name) => (await request('POST', '/api/world/save', { name })).data,
  files: async () => (await request('GET', '/api/world/files')).data.files,
};
