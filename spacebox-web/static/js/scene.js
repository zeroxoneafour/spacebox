// three.js rendering of the three zoom levels: galaxy, system and planet.
//
// The content group is rebuilt from the store on every update (views are small), while the
// camera is only reset when the focus changes, so the user's orbit survives ticks.

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { CSS2DRenderer, CSS2DObject } from 'three/addons/renderers/CSS2DRenderer.js';

const STAR_COLORS = {
  'yellow-small': 0xffe9a8, yellow: 0xfff1b0, orange: 0xffb46b, 'red-dwarf': 0xff7a5c,
  white: 0xf2f6ff, 'blue-giant': 0x9cc4ff, neutron: 0xc8b8ff,
};

export const CLASS_COLORS = {
  capital: '#ffd34d', administration: '#ffe7a3', housing: '#cfd6e4', amenities: '#ff9db8',
  agriculture: '#7cd36a', power: '#f6dc4f', mining: '#c09060', forge: '#ef7a3c',
  factory: '#d68fe0', refinery: '#a58cff', research: '#5cc0ff', strategic: '#c58bff',
  'barren-mining': '#b88a5c', 'barren-power': '#f0d25a',
};

/// System coordinates (up to ~100) to scene units.
export const SYSTEM_SCALE = 0.15;

export function buildingColor(defs, id) {
  const b = defs.buildings[id];
  for (const c of b.tags) if (c !== 'barren' && CLASS_COLORS[c]) return CLASS_COLORS[c];
  let h = 0;
  for (const ch of b.name) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return `hsl(${h % 360}, 60%, 60%)`;
}

export function empireColor(state, id) {
  return id == null ? '#7a8296' : state.empires[id]?.color || '#9aa4b8';
}

function hash(n) {
  let x = (n + 1) * 2654435761;
  x = ((x >>> 16) ^ x) * 0x45d9f3b;
  return ((x >>> 16) ^ x) / 4294967296 + 0.5;
}

/// The last system the fleet's jump orders lead to (its own system if none).
export function fleetDestination(state, f) {
  return f.destination;
}

/// Where a fleet leaving system `from` for `to` charges its hyperdrive: the edge facing `to`.
export function departurePoint(state, defs, from, to) {
  const a = state.systems[from].coordinates;
  const b = state.systems[to].coordinates;
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  const len = Math.hypot(dx, dy) || 1;
  const r = defs.rules.arrival_radius;
  return [(dx / len) * r, (dy / len) * r];
}


export class Scene {
  constructor(container, handlers) {
    this.container = container;
    this.handlers = handlers;
    this.renderer = new THREE.WebGLRenderer({ antialias: true });
    this.renderer.setPixelRatio(window.devicePixelRatio);
    container.appendChild(this.renderer.domElement);

    this.labels = new CSS2DRenderer();
    Object.assign(this.labels.domElement.style, { position: 'absolute', top: '0', left: '0', pointerEvents: 'none' });
    container.appendChild(this.labels.domElement);

    this.scene = new THREE.Scene();
    this.scene.background = new THREE.Color(0x05070c);
    this.camera = new THREE.PerspectiveCamera(50, 1, 0.05, 10000);
    this.controls = new OrbitControls(this.camera, this.renderer.domElement);
    this.controls.enableDamping = true;

    this.scene.add(new THREE.AmbientLight(0xffffff, 0.55));
    const sun = new THREE.DirectionalLight(0xffffff, 1.6);
    sun.position.set(5, 8, 6);
    this.scene.add(sun);
    this.scene.add(this.starfield());

    this.content = new THREE.Group();
    this.scene.add(this.content);
    // Labels live in their own layer and are reused by key across rebuilds: three.js deletes
    // a label's DOM element when it leaves the scene, so recreating them on every update
    // made them flicker.
    this.labelLayer = new THREE.Group();
    this.scene.add(this.labelLayer);
    this.labelCache = new Map();
    this.usedLabels = new Set();
    this.pickables = [];
    this.key = null;
    this.raycaster = new THREE.Raycaster();
    this.pointer = new THREE.Vector2();

    this.bindPointer();
    new ResizeObserver(() => this.resize()).observe(container);
    this.resize();
    const loop = () => {
      this.controls.update();
      this.renderer.render(this.scene, this.camera);
      this.labels.render(this.scene, this.camera);
      requestAnimationFrame(loop);
    };
    loop();
  }

  starfield() {
    const n = 2500;
    const pos = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) {
      const v = new THREE.Vector3(hash(i * 3) - 0.5, hash(i * 3 + 1) - 0.5, hash(i * 3 + 2) - 0.5).normalize().multiplyScalar(3000);
      pos.set([v.x, v.y, v.z], i * 3);
    }
    const g = new THREE.BufferGeometry();
    g.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    return new THREE.Points(g, new THREE.PointsMaterial({ color: 0x8890a8, size: 2, sizeAttenuation: false }));
  }

  resize() {
    const w = this.container.clientWidth;
    const h = this.container.clientHeight;
    this.renderer.setSize(w, h);
    this.labels.setSize(w, h);
    this.camera.aspect = w / Math.max(h, 1);
    this.camera.updateProjectionMatrix();
  }

  bindPointer() {
    const el = this.renderer.domElement;
    let down = null;
    const pick = (ev) => {
      const r = el.getBoulabelsndingClientRect();
      this.pointer.set(((ev.clientX - r.left) / r.width) * 2 - 1, -((ev.clientY - r.top) / r.height) * 2 + 1);
      this.raycaster.setFromCamera(this.pointer, this.camera);
      this.raycaster.params.Line = { threshold: 0 };
      const hit = this.raycaster.intersectObjects(this.pickables, false)[0];
      return hit?.object.userData.pick ?? null;
    };
    el.addEventListener('pointerdown', (ev) => (down = [ev.clientX, ev.clientY]));
    el.addEventListener('pointerup', (ev) => {
      if (down && Math.hypot(ev.clientX - down[0], ev.clientY - down[1]) < 5) this.handlers.onPick(pick(ev), ev);
      down = null;
    });
    el.addEventListener('dblclick', (ev) => this.handlers.onOpen(pick(ev)));
    // Right-click issues orders: the picked object, plus the point on the system plane.
    el.addEventListener('contextmenu', (ev) => {
      ev.preventDefault();
      const target = pick(ev);
      const hit = new THREE.Vector3();
      const onPlane = this.raycaster.ray.intersectPlane(new THREE.Plane(new THREE.Vector3(0, 1, 0), 0), hit);
      this.handlers.onContext?.(target, onPlane ? [hit.x / SYSTEM_SCALE, hit.z / SYSTEM_SCALE] : null);
    });
    el.addEventListener('pointermove', (ev) => {
      const p = pick(ev);
      const tip = this.handlers.tooltip;
      const text = p ? this.handlers.describe(p) : null;
      if (!text) {
        tip.hidden = true;
        el.style.cursor = '';
        return;
      }
      const r = this.container.getBoundingClientRect();
      tip.textContent = text;
      tip.hidden = false;
      tip.style.left = `${ev.clientX - r.left + 14}px`;
      tip.style.top = `${ev.clientY - r.top + 10}px`;
      el.style.cursor = 'pointer';
    });
    el.addEventListener('pointerleave', () => (this.handlers.tooltip.hidden = true));
  }

  clear() {
    this.content.traverse((o) => {
      if (o.geometry && !o.userData.shared) o.geometry.dispose();
      if (o.material && !o.userData.shared) (Array.isArray(o.material) ? o.material : [o.material]).forEach((m) => m.dispose());
    });
    this.content.clear();
    this.content.rotation.set(0, 0, 0);
    this.pickables = [];
  }

  /// Renders `view` = { mode: 'galaxy'|'system'|'planet', id } with `sel` highlighted.
  show(view, store, sel) {
    const key = `${store.epoch}:${view.mode}:${view.id}`;
    const reset = key !== this.key;
    this.key = key;
    this.clear();
    this.usedLabels = new Set();
    if (view.mode === 'galaxy') this.galaxy(store, sel, reset);
    else if (view.mode === 'system') this.system(store, view.id, sel, reset);
    else this.planet(store, view.id, sel, reset);
    // Drop labels the new frame didn't ask for (this removes their DOM elements).
    for (const [k, obj] of this.labelCache) {
      if (!this.usedLabels.has(k)) {
        this.labelLayer.remove(obj);
        this.labelCache.delete(k);
      }
    }
  }

  /// A persistent label for `key` (scoped to the current view), updated in place.
  label(key, text, dim = false) {
    const k = `${this.key}|${key}`;
    let obj = this.labelCache.get(k);
    if (!obj) {
      const el = document.createElement('div');
      obj = new CSS2DObject(el);
      this.labelCache.set(k, obj);
      this.labelLayer.add(obj);
    }
    const cls = dim ? 'label dim' : 'label';
    if (obj.element.className !== cls) obj.element.className = cls;
    if (obj.element.textContent !== text) obj.element.textContent = text;
    this.usedLabels.add(k);
    return obj;
  }

  setCamera(pos, target = new THREE.Vector3()) {
    this.camera.position.copy(pos);
    this.controls.target.copy(target);
    this.controls.update();
  }

  galaxy({ state }, sel, reset) {
    const g = this.content;
    const xs = state.systems.map((s) => s.coordinates[0]);
    const zs = state.systems.map((s) => s.coordinates[1]);
    const mid = new THREE.Vector3((Math.min(...xs, 0) + Math.max(...xs, 0)) / 2, 0, (Math.min(...zs, 0) + Math.max(...zs, 0)) / 2);
    const extent = Math.max(10, ...state.systems.map((s) => Math.hypot(s.coordinates[0] - mid.x, s.coordinates[1] - mid.z)));
    // Stars grow with the galaxy so they stay visible from the default camera distance.
    const k = Math.max(1, extent / 25);
    const starGeo = new THREE.SphereGeometry(0.55 * k, 16, 12);
    const ringGeo = new THREE.TorusGeometry(1.0 * k, 0.07 * k, 6, 32);
    const capitals = new Set(state.empires.map((e) => e.capital).filter((c) => c != null));
    const showAll = state.systems.length <= 60;

    for (const s of state.systems) {
      const [x, z] = s.coordinates;
      const owned = s.owner != null;
      const color = owned ? empireColor(state, s.owner) : STAR_COLORS[s.star_type] ?? 0xffffff;
      const m = new THREE.Mesh(starGeo, new THREE.MeshBasicMaterial({ color, transparent: !owned, opacity: owned ? 1 : 0.45 }));
      m.position.set(x, 0, z);
      m.scale.setScalar((s.star_count > 1 ? 1.3 : 1) * (owned ? 1 : 0.65));
      m.userData.pick = { kind: 'system', id: s.id };
      g.add(m);
      this.pickables.push(m);
      if (capitals.has(s.id) || (sel.kind === 'system' && sel.id === s.id)) {
        const selected = sel.kind === 'system' && sel.id === s.id;
        const ring = new THREE.Mesh(ringGeo, new THREE.MeshBasicMaterial({ color: selected ? 0xffffff : color }));
        ring.rotation.x = Math.PI / 2;
        ring.position.set(x, 0, z);
        ring.scale.setScalar(selected ? 1.5 : 1);
        g.add(ring);
      }
      if (showAll || owned || (sel.kind === 'system' && sel.id === s.id)) {
        this.label(`sys:${s.id}`, s.display_name, !owned).position.set(x, 0, z);
      }
    }
    // Hyperlanes, tinted when both ends share an owner.
    const pos = [];
    const col = [];
    const c = new THREE.Color();
    for (const s of state.systems) {
      for (const l of s.links) {
        if (l < s.id) continue;
        const o = state.systems[l];
        pos.push(s.coordinates[0], 0, s.coordinates[1], o.coordinates[0], 0, o.coordinates[1]);
        c.set(s.owner != null && s.owner === o.owner ? empireColor(state, s.owner) : '#3a4560');
        col.push(c.r, c.g, c.b, c.r, c.g, c.b);
      }
    }
    const lg = new THREE.BufferGeometry();
    lg.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3));
    lg.setAttribute('color', new THREE.Float32BufferAttribute(col, 3));
    g.add(new THREE.LineSegments(lg, new THREE.LineBasicMaterial({ vertexColors: true, transparent: true, opacity: 0.7 })));

    // Fleets: markers stacked beside their system, with a line to where they are headed.
    const perSystem = new Map();
    const fleetGeo = new THREE.ConeGeometry(0.45 * k, 1.1 * k, 4);
    for (const f of state.fleets) {
      const s = state.systems[f.system];
      if (!s) continue;
      const n = perSystem.get(f.system) ?? 0;
      perSystem.set(f.system, n + 1);
      const [x, z] = s.coordinates;
      const selected = sel.kind === 'fleet' && sel.id === f.id;
      const m = new THREE.Mesh(fleetGeo, new THREE.MeshBasicMaterial({ color: selected ? 0xffffff : empireColor(state, f.owner) }));
      m.position.set(x + 1.6 * k + n * 1.1 * k, 0.8 * k, z - 1.2 * k);
      m.userData.pick = { kind: 'fleet', id: f.id };
      g.add(m);
      this.pickables.push(m);
      if (f.in_combat) {
        const ring = new THREE.Mesh(new THREE.TorusGeometry(0.8 * k, 0.08 * k, 6, 24), new THREE.MeshBasicMaterial({ color: 0xff4040 }));
        ring.rotation.x = Math.PI / 2;
        ring.position.copy(m.position);
        g.add(ring);
      }
      const dest = fleetDestination(state, f);
      if (dest != null && dest !== f.system) {
        const d = state.systems[dest].coordinates;
        const line = new THREE.Line(
          new THREE.BufferGeometry().setFromPoints([m.position.clone(), new THREE.Vector3(d[0], 0.8 * k, d[1])]),
          new THREE.LineDashedMaterial({ color: empireColor(state, f.owner), dashSize: k, gapSize: k * 0.6 }),
        );
        line.computeLineDistances();
        g.add(line);
      }
    }

    if (reset) this.setCamera(mid.clone().add(new THREE.Vector3(0, extent * 2.1 + 8, extent * 0.75 + 6)), mid);
  }

  system({ state, defs }, id, sel, reset) {
    const s = state.systems[id];
    if (!s) return;
    const g = this.content;
    const S = SYSTEM_SCALE;
    const starColor = STAR_COLORS[s.star_type] ?? 0xffffff;
    for (let k = 0; k < s.star_count; k++) {
      const star = new THREE.Mesh(new THREE.SphereGeometry(0.9, 32, 24), new THREE.MeshBasicMaterial({ color: starColor }));
      star.position.x = s.star_count > 1 ? (k === 0 ? -0.8 : 0.8) : 0;
      star.userData.pick = { kind: 'system', id };
      g.add(star);
      this.pickables.push(star);
    }
    g.add(new THREE.PointLight(starColor, 60, 0, 1.2));
    this.label('star', `${s.display_name}${s.owner != null ? ' — ' + state.empires[s.owner].display_name : ''}`).position.set(0, 1.4, 0);
    // The hyperjump arrival rim.
    const rim = new THREE.EllipseCurve(0, 0, 90 * S, 90 * S).getPoints(128).map((v) => new THREE.Vector3(v.x, 0, v.y));
    g.add(new THREE.LineLoop(new THREE.BufferGeometry().setFromPoints(rim), new THREE.LineDashedMaterial({ color: 0x1d2538 })));

    for (const pid of s.planets) {
      const p = state.planets[pid];
      const [px, pz] = p.coordinates;
      const radius = Math.hypot(px, pz) * S;
      const orbit = new THREE.EllipseCurve(0, 0, radius, radius).getPoints(96).map((v) => new THREE.Vector3(v.x, 0, v.y));
      g.add(new THREE.LineLoop(new THREE.BufferGeometry().setFromPoints(orbit), new THREE.LineBasicMaterial({ color: 0x1f2740 })));

      const size = p.size === 'large' ? 0.5 : 0.3;
      const color = p.habitable ? (p.pop > 0 ? 0x3fa36b : 0x4a8c7a) : p.size === 'large' ? 0x8f8173 : 0x9d978e;
      const mat = new THREE.MeshStandardMaterial({ color, roughness: 0.8, emissive: p.terraform ? 0x4a3a00 : 0x000000 });
      const m = new THREE.Mesh(new THREE.SphereGeometry(size, 24, 16), mat);
      m.position.set(px * S, 0, pz * S);
      m.userData.pick = { kind: 'planet', id: pid };
      g.add(m);
      this.pickables.push(m);
      // Owner band, and a red one while under siege.
      const band = (r, c) => {
        const ring = new THREE.Mesh(new THREE.TorusGeometry(r, 0.035, 6, 32), new THREE.MeshBasicMaterial({ color: c }));
        ring.rotation.x = Math.PI / 2;
        ring.position.copy(m.position);
        g.add(ring);
      };
      if (p.owner != null) band(size + 0.12, empireColor(state, p.owner));
      if (p.siege > 0) band(size + 0.24, 0xff4040);
      if (sel.kind === 'planet' && sel.id === pid) band(size + 0.36, 0xffffff);
      const built = p.tiles.filter((t) => t.b != null).length;
      const pl = this.label(`planet:${pid}`, `${p.display_name}${p.pop ? ` · ${p.pop}` : ''}${built ? ` · ${built}▣` : ''}`, !p.habitable);
      pl.position.copy(m.position);
      pl.position.y += size;
    }

    // Fleets.
    const fleetGeo = new THREE.ConeGeometry(0.28, 0.7, 4);
    const shipDotGeo = new THREE.SphereGeometry(0.06, 6, 4);
    for (const f of state.fleets) {
      if (f.system !== id) continue;
      const [fx, fz] = f.coordinates;
      const selected = sel.kind === 'fleet' && sel.id === f.id;
      const m = new THREE.Mesh(fleetGeo, new THREE.MeshStandardMaterial({
        color: empireColor(state, f.owner),
        emissive: selected ? 0x666666 : 0x111111,
      }));
      m.position.set(fx * S, 0.5, fz * S);
      m.userData.pick = { kind: 'fleet', id: f.id };
      g.add(m);
      this.pickables.push(m);
      // Each ship at its own position (the formation around the fleet).
      const shipMat = new THREE.MeshBasicMaterial({ color: empireColor(state, f.owner) });
      for (const [, , , x, y] of f.ships) {
        const dot = new THREE.Mesh(shipDotGeo, shipMat);
        dot.position.set(x * S, 0.5, y * S);
        g.add(dot);
      }
      if (f.in_combat || selected) {
        const ring = new THREE.Mesh(
          new THREE.TorusGeometry(0.55, 0.04, 6, 24),
          new THREE.MeshBasicMaterial({ color: f.in_combat ? 0xff4040 : 0xffffff }),
        );
        ring.rotation.x = Math.PI / 2;
        ring.position.copy(m.position);
        g.add(ring);
      }
      // Where it is heading inside this system.
      let target = null;
      const o = f.orders[0];
      if (o?.order === 'move') target = o.coordinates;
      if (o?.order === 'bombard') target = state.planets[o.planet].coordinates;
      if (o?.order === 'jump') target = departurePoint(state, defs, id, o.system);
      if (target) {
        const line = new THREE.Line(
          new THREE.BufferGeometry().setFromPoints([m.position.clone(), new THREE.Vector3(target[0] * S, 0.5, target[1] * S)]),
          new THREE.LineDashedMaterial({ color: o.order === 'bombard' ? 0xff6060 : empireColor(state, f.owner), dashSize: 0.3, gapSize: 0.2 }),
        );
        line.computeLineDistances();
        g.add(line);
      }
      let text = `${f.display_name} (${f.ships.length})`;
      if (o?.order === 'jump') text += ` → ${state.systems[o.system].display_name}${f.jump_progress > 0 ? ` ${Math.round(f.jump_progress * 100)}%` : ''}`;
      const fl = this.label(`fleet:${f.id}`, text);
      fl.position.copy(m.position);
      fl.position.y += 0.4;
    }
    if (reset) this.setCamera(new THREE.Vector3(0, 21, 17));
  }

  planet({ state, defs }, id, sel, reset) {
    const p = state.planets[id];
    if (!p) return;
    const g = this.content;
    // Geometry has the north pole on +Z; turn it so the pole points up.
    g.rotation.x = -Math.PI / 2;
    const shape = defs.geometry[p.size];
    const R = 3;
    const base = new THREE.Color();
    const up = new THREE.Vector3(0, 1, 0);

    shape.faces.forEach((corners, i) => {
      const t = p.tiles[i] || {};
      const center = new THREE.Vector3(...shape.centers[i]);
      const faceCenter = corners.reduce((a, c) => a.add(new THREE.Vector3(...c)), new THREE.Vector3()).divideScalar(corners.length);
      // Inset corners slightly so tiles read as separate cells.
      const pts = corners.map((c) => new THREE.Vector3(...c).sub(faceCenter).multiplyScalar(0.96).add(faceCenter).multiplyScalar(R));
      const fc = faceCenter.clone().multiplyScalar(R);
      const verts = [];
      for (let k = 0; k < pts.length; k++) {
        const a = pts[k];
        const b = pts[(k + 1) % pts.length];
        verts.push(fc.x, fc.y, fc.z, a.x, a.y, a.z, b.x, b.y, b.z);
      }
      const geo = new THREE.BufferGeometry();
      geo.setAttribute('position', new THREE.Float32BufferAttribute(verts, 3));
      geo.computeVertexNormals();

      const v = hash(i + id * 37) * 0.12;
      const blocker = t.b != null && defs.buildings[t.b].blocker;
      if (t.wa) base.setHSL(0.58, 0.55, 0.3 + v);
      else if (p.habitable) base.setHSL(0.33 - v, 0.38, 0.32 + v);
      else base.setHSL(0.08, 0.12 + v, 0.36 + v);
      if (blocker) base.multiplyScalar(0.55);
      else if (t.b != null) base.lerp(new THREE.Color(buildingColor(defs, t.b)), 0.45);
      const selected = sel.kind === 'planet' && sel.id === id && sel.tile === i;
      const mat = new THREE.MeshStandardMaterial({
        color: base.clone(),
        roughness: 0.9,
        flatShading: true,
        emissive: selected ? 0x3a6ea8 : 0x000000,
      });
      const mesh = new THREE.Mesh(geo, mat);
      mesh.userData.pick = { kind: 'tile', planet: id, tile: i };
      g.add(mesh);
      this.pickables.push(mesh);

      const outline = new THREE.LineLoop(
        new THREE.BufferGeometry().setFromPoints(pts),
        new THREE.LineBasicMaterial({ color: selected ? 0xffffff : 0x0b0e14 }),
      );
      g.add(outline);

      // Building marker: a column whose height shows how staffed it is.
      const normal = center.clone().normalize();
      if (blocker) {
        // Natural blockers: a low outcrop rather than a building column.
        const rock = new THREE.Mesh(
          new THREE.ConeGeometry(0.32, 0.22, 5),
          new THREE.MeshStandardMaterial({ color: base.clone().multiplyScalar(0.8), flatShading: true }),
        );
        rock.quaternion.setFromUnitVectors(up, normal);
        rock.position.copy(fc).addScaledVector(normal, 0.1);
        rock.userData.pick = { kind: 'tile', planet: id, tile: i };
        g.add(rock);
        this.pickables.push(rock);
      } else if (t.b != null || t.c) {
        const def = t.b != null ? defs.buildings[t.b] : null;
        const staffed = def && def.max_workers > 0 ? (t.w || 0) / def.max_workers : 1;
        const h = t.b != null ? 0.15 + 0.45 * staffed : 0.3;
        const geo2 = t.b != null ? new THREE.CylinderGeometry(0.2, 0.26, h, 6) : new THREE.BoxGeometry(0.35, h, 0.35);
        const mat2 = t.b != null
          ? new THREE.MeshStandardMaterial({ color: buildingColor(defs, t.b), roughness: 0.5, emissive: t.off ? 0x000000 : 0x111111 })
          : new THREE.MeshBasicMaterial({ color: 0xffc857, wireframe: true });
        if (t.off) mat2.color.multiplyScalar(0.35);
        if (t.dmg) mat2.color.lerp(new THREE.Color(0xff2020), Math.min(1, t.dmg * 1.5));
        const col = new THREE.Mesh(geo2, mat2);
        col.quaternion.setFromUnitVectors(up, normal);
        col.position.copy(fc).addScaledVector(normal, h / 2);
        col.userData.pick = { kind: 'tile', planet: id, tile: i };
        g.add(col);
        this.pickables.push(col);
        if (t.c && t.b != null) {
          // Upgrade in progress: wireframe cap.
          const cap = new THREE.Mesh(new THREE.BoxGeometry(0.4, 0.12, 0.4), new THREE.MeshBasicMaterial({ color: 0xffc857, wireframe: true }));
          cap.quaternion.copy(col.quaternion);
          cap.position.copy(fc).addScaledVector(normal, h + 0.08);
          g.add(cap);
        }
      }
      if (t.m?.length) {
        // A small marker for resource-rich tiles.
        const dot = new THREE.Mesh(
          new THREE.SphereGeometry(0.06, 8, 6),
          new THREE.MeshBasicMaterial({ color: t.m.some(([, m]) => m >= 1) ? 0x9ff0ff : 0xff8080 }),
        );
        const offset = new THREE.Vector3(...corners[0]).multiplyScalar(R).sub(fc).multiplyScalar(0.55);
        dot.position.copy(fc).add(offset).addScaledVector(normal, 0.03);
        g.add(dot);
      }
    });

    if (reset) this.setCamera(new THREE.Vector3(0, 3.5, 8.5));
  }
}
