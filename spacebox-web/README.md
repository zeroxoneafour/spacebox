# spacebox-web

An Axum server that runs the Spacebox engine (`../spacebox-core`), plus a three.js admin
client. Anyone who can reach the server has full game-master control over the simulation.

```sh
cargo run --release              # http://127.0.0.1:3000, starts with data/scenarios/sol.toml
cargo run --release -- --port 8080 --load saves/my-save.toml
```

Options: `--port N`, `--data DIR` (default `../data`), `--load FILE` (relative to `--data`),
`--static DIR` (client files, default `./static`). The server binds to 127.0.0.1 only.

The client loads three.js from the jsDelivr CDN, so the browser needs internet access.

## Client

- **Galaxy view.** Systems are coloured by owner, hyperlanes are tinted inside empires, and
  capitals have a ring. Fleets are cones beside their system, with dashed lines to their
  destination and a red ring while fighting. Click a system to inspect it; double-click to
  enter it.
- **System view.** The star(s), planets at their fixed coordinates (with owner bands and a red
  band while under siege), the hyperjump rim, and fleets (with a dot per ship at its own
  position) with a line to where their current order takes them and their jump charge.
  Double-click a planet to land on it.
- **Fleet orders.** Select a fleet, then right-click. On a system in the galaxy view it
  routes there (a queue of jumps). On a point in the system view it moves to that point. On
  an enemy (or unowned) planet or tile it bombards it. The fleet panel shows the order queue
  and has the same orders (tick "add to the end of the queue" to append instead of replace),
  plus merge, split, rename, and admin repair, transfer, teleport and disband.
- **Planet view.** The 12- or 32-face solid, with the tile numbering from the engine. A column
  marks each building and its height shows how staffed it is. Water tiles are blue, and
  natural blockers are dark outcrops that must be cleared (a removal project, at their
  `remove-cost`) before building. Wireframes mark construction, and dots mark tiles with
  resource multipliers. Click a tile to build, upgrade, demolish, cap jobs, pin workers or
  disable it. Projects show what they still need; cancelling one refunds what it has used.
- **Side panels.** Empires (create, rename, recolour, dominant species, toggle AI and tune its
  militant/expansionist/scientific sliders, migration, market access and total-war policy, set
  stockpiles, transient output and waste, add or cancel research projects, grant or revoke
  technologies, diplomacy: declare wars with a goal, set the defense's goal, follow both
  sides' exhaustion, propose, accept or enforce peace, drop claims, found, join or leave
  alliances; cancel ships under construction, spawn fleets); systems (owner, occupier,
  claimants and claiming, capital, hyperlanes, planets with owners, fleets present, add
  planets, delete); planets (owner, occupier, siege progress, ship construction, pops per species, add, remove, set or move pops,
  terraform or cancel it, toggle habitability, delete).
- **Expansion.** On an empty tile in an unowned system, choose which empire builds. The panel
  shows the extra influence and days for the claim.
- **Top bar.** Play/pause (space), step size, and the size of the last update. **World…**
  regenerates a galaxy, loads a scenario or save, or saves to `data/saves/`.

Most tile actions come in two flavours. *Order* goes through the normal game rules: costs,
technology and the barren-planet rule; the project then draws its resources over time.
*Admin* acts instantly and ignores the rules.

## REST API

All bodies are JSON. Ids are indices into the corresponding arrays of the state.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/defs` | Static catalog: resources, jobs, species, buildings, upgrades, technologies, ships, terraforming, planet geometry. Costs are `{ resources: [[id, count]], days }`, building outputs `{ resources, amenities }` |
| GET | `/api/state` | Full state: `{ epoch, version, event_seq, state, events }` |
| POST | `/api/tick` | `{ days, since, epoch, since_event }`: advance time, then return a diff |
| POST | `/api/sync` | Same as tick, but does not advance time (use after admin edits) |
| POST | `/api/command` | Engine command with normal rules, e.g. `{ "type": "build", "planet": 3, "tile": 7, "building": 5 }` |
| POST | `/api/empires` | Create an empire: `{ display_name, ai, color, capital, starting_resources, dominant_species }`; `ai` is `{ militant, expansionist, scientific }` (0–1) or null for a player empire; `starting_resources` uses the ruleset's `base-resource-stores` |
| PATCH | `/api/empires/{id}` | `display_name`, `ai` (personality object, or `null` to hand control to the player), `color`, `accepts_migrants`, `market_access`, `total_war`, `dominant_species` |
| POST | `/api/empires/{id}/techs` | Grant or revoke a technology: `{ tech, known }` |
| POST | `/api/empires/{id}/storage` | Set a stockpile: `{ resource, count }` (not for transient resources) |
| POST | `/api/systems` | Add a system: `{ display_name, coordinates, owner, links, link_nearest, random_planets }` |
| PATCH / DELETE | `/api/systems/{id}` | Edit (`display_name`, `star_type`, `star_count`, `coordinates`) or delete |
| POST | `/api/systems/{id}/planets` | Add a planet: `{ display_name, size, habitable, pop, species, coordinates, terrain }` (`terrain` generates seas and blockers, default true) |
| PATCH / DELETE | `/api/planets/{id}` | `population` (`[[species, count]]`), `add_pops` (of `species`, default the most numerous), `habitable`, `display_name`, `coordinates`, `terraform_days_left` (0 finishes), `repair` (clears siege and damage); or delete |
| PUT | `/api/planets/{id}/tiles/{tile}` | Admin tile edit: `{ building: id or null, upgrades, cancel_construction, multipliers, water }` |
| POST | `/api/fleets` | Spawn a fleet instantly: `{ empire, system, display_name, coordinates, ships: [{ ship, count }] }` |
| PATCH / DELETE | `/api/fleets/{id}` | `display_name`, `owner`, `system` (teleport), `coordinates`, `repair`; or delete |
| POST | `/api/world/regenerate` | `{ systems, empires, seed, ai }` |
| POST | `/api/world/load` | `{ path }`, relative to the data directory |
| POST | `/api/world/save` | `{ name }`, written to `data/saves/{name}.toml` |
| GET | `/api/world/files` | Loadable scenarios and saves |

`/api/command` accepts these `type`s: `move_pops`, `assign_workers`, `set_job_cap`,
`set_enabled`, `build` (with an optional `empire`, required for expansion claims),
`demolish` (cancels a project with a refund, or removes a building), `upgrade`, `terraform`,
`cancel_terraform`, `research`, `cancel_research`, `set_population`, `set_owner`,
`set_planet_owner`, `set_capital`, `link`, `unlink`, `grant_resources`, `build_ship` (at any
owned planet; fed by that planet's shipyards), `cancel_ship`, `set_orders`
(`{ fleet, orders: [{ order: "move", coordinates } | { order: "jump", system } |
{ order: "bombard", planet, tile }], append }`), `move_fleet` and `bombard` (which replace
the queue with a route of jumps plus the move or bombardment),
`move_fleet`, `bombard`, `stop_fleet`, `merge_fleets`, `split_fleet`, `rename_fleet`,
`disband_fleet`, `declare_war` (`goal`: `"humiliate"`, `"claim"` or `"total"`),
`set_war_goal`, `propose_peace` and `enforce_peace` (`outcome`: `"offensive"`, `"defensive"`
or `"status quo"`), `accept_peace`, `claim`, `drop_claim`, `create_alliance`,
`join_alliance`, `leave_alliance`. The fields match `spacebox_core::Command`;
fleet ids are the stable `id` of each entry in `state.fleets`.

### Diffs

Each time state is requested, the server takes a snapshot: every system, planet, empire and
fleet serialized on its own, with entities unchanged since the previous snapshot sharing its
text. If anything changed it gets a new `version`, and the last 8 versions are kept. A client
that sends `since: <version>` and the current `epoch` gets back `ops`: a list of path-based
edits (`{p, v}` set, `{p, d}` delete, `{p, n}` resize array), computed by parsing and
comparing only the entities that changed. If that version has been evicted, or the world has
been replaced (new `epoch`), the client gets the full `state` instead.

`since_event` works the same way for the event log.

A snapshot costs little more than its JSON size in memory (about 10 MB for 2,000 systems) and
takes about 10 ms to build for that size. On a 120-system galaxy, a 10-day step sends about
10 KB of ops instead of the 320 KB full state.

Removing systems or planets shifts the ids that come after them. The diff handles this, but
any queued engine commands are dropped.
