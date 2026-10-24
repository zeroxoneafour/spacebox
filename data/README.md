# Spacebox data

All game content is TOML. The format follows `design.md`. This file documents what the engine
adds on top of it.

| File | Contents |
| --- | --- |
| `resources.toml` | 11 resources: basic (energy, minerals, food), advanced (alloy, consumer goods, research), strategic (volatile motes, exotic gases, rare crystals), influence, and on-demand shipbuilding |
| `jobs.toml` | 13 jobs in worker/specialist/ruler tiers, with appeal and per-worker upkeep |
| `buildings.toml` | 27 buildings (housing, production, industry, refineries, research, shipyards, barren-world automation) and 10 upgrades |
| `technologies.toml` | A 24-technology tree that unlocks buildings, upgrades, ship designs, empire modifiers and terraforming |
| `ships.toml` | 7 ship designs, 5 weapons and 6 outfits |
| `rules.toml` | `[ruleset.default]`: pop upkeep, growth, stability, market, expansion, fleet, bombardment and terraforming constants |
| `base.toml` | Index that imports everything (used by `spacebox generate`) |
| `mods/orbital-agriculture.toml` | Example mod: a new barren building plus an upgrade for a base-game building |
| `scenarios/sol.toml` | Hand-written scenario: Terran Federation vs. Cetian Concord |
| `saves/` | Example output of `spacebox run ... --out` |

## Imports and references

- `[import]` must be the first section, if present. Paths are relative to the importing file.
- `"alias.name"` looks `name` up in the imported file. A bare `"name"` looks it up in the
  current file. Imports do not leak: you can only use aliases your own file declares.
- The slot decides which table a reference points into. `resource = "x.y"` looks for
  `[resource.y]`, `job = "x.y"` for `[job.y]`, and so on.
- `apply_to`, adjacency `building = "..."`, and technology modifiers take a building reference.
  If no building has that name, the string is treated as a building class name (`"mining"`).
  `building_class = "..."` always means a class.
- Tile upgrade names are ordinary references, as in the design document:
  `upgrades = ["buildings.faster-forge"]`.
- Unknown top-level tables and unknown fields are errors, so typos are caught.
- Files must be standard TOML 1.0. TOML 1.1 extensions such as multi-line inline tables are
  rejected; use a `[table]` instead (as `[...tiles.N.building]` does).
- Only the current format loads. The project is pre-alpha, so renamed fields have no
  aliases and old saves are not migrated.
- Asset tables load from every reachable file. Game state (`[game]`, `[empire.*]`,
  `[system.*]`) is only read from the file you load, so a save can import a scenario for its
  assets without duplicating its systems.
- Ship designs reference weapons and outfits the same way: `weapons = ["laser"]` looks for
  `[ship-weapon.laser]` in the same file, `"alias.laser"` in an import.

## Field reference (beyond the design document)

**resource**: `base_value` (market price in energy, default 1), `tradeable` (default true),
`stockpile` (default true; research is false), `storage_cap`, `on_demand` (only produced while
something needs it; buildings that output it sit idle otherwise).

**job**: `upkeep = [{ resource, count }]`, consumed per worker per day.

**building**: `display_name` (defaults to the key), `amenities`, `build_cost`, `build_days`,
`remove_cost` (paid to demolish or clear it), `requires_tech`, `buildable` (false = scenario
only), `max_per_planet`, `hp` (bombardment damage it absorbs). Adjacency entries accept every
modifier field, and they stack once per matching neighbour.

Special building classes:

| Class | Meaning |
| --- | --- |
| `barren` | Can be built on barren planets (must have `max_workers = 0`) |
| `blocker` | Natural feature: generated with planets, never constructed, cleared by paying `remove_cost`. With `barren` it appears on barren worlds, with `water` on water tiles, otherwise on land |
| `land` | Must stand on a tile without water |
| `water` | Must stand on a water tile |
| `space` | Belongs on orbital megastructures, which are not implemented yet, so it cannot be placed |

Buildings with neither `land` nor `water` can stand on either.

**building-upgrade**: `display_name`, `cost`, `build_days`, `requires_tech`, `input_resources`,
`upkeep_resources`, `housing`, `amenities`, `housing_mult`, `amenities_mult`.

**technology**: `cost`, `prerequisites`, `category`, `pop_growth_mult`, `stability`, and
`modifiers = [{ building_class = "...", output_mult = 1.1 }]` (empire-wide).

**ship**: as in the design document, plus `display_name`, `description`, `requires_tech` and
`hyperjump_mult`. Build cost is the hull plus all weapons and outfits.

**ship-weapon**: as in the design document (`fire_rate` is days between shots, `range` is in
system units).

**ship-outfit**: `build_cost`, `upkeep_cost`, flat `hull` / `shield` / `shield_regen`, and the
multipliers `hull_mult`, `shield_mult`, `shield_regen_mult`, `speed_mult`, `hyperjump_mult`,
`damage_mult`, `range_mult` and `fire_rate_mult` (which divides the delay between shots).

**ruleset**: see the comments in `rules.toml`. Every field has a default.
`[ruleset.x.terraforming.large|small]` defines the tech, cost and duration for terraforming.

**game** (saves): `day`, `seed`, `ruleset`.

**empire**: `capital` (system), `technologies`, `researching`, `research_progress`,
`accepts_migrants`, `market_access`, `color`, `crises`,
`wars = [{ empire, since_day }]`, `truces = [{ empire, until_day }]`,
`ship_queue = [{ ship, planet = "system/planet", remaining, fleet }]`.

**empire AI** (`[empire.x.ai]`): the table's presence makes the empire AI-run. `militant`,
`expansionist` and `scientific` are each 0–1 (default 0.5):

- **militant**: fleet size (0.4×–1.6× the baseline), deeper ship queues, declaring war on a
  neighbour within three jumps once its fleet outmatches the target's by
  `1.8 - 1.2 × militant`, one or (above 0.75) two wars at a time, attacking with smaller
  fleets, fighting longer before war-weariness (360 + 1440 × militant days), and favouring
  military research.
- **expansionist**: claims systems up to 1–3 jumps out, keeps less influence in reserve
  (`2 - expansionist` × the cost), and runs up to three claims at once. Near 0, it never
  expands.
- **scientific**: how highly research labs rank among its building priorities, and a lean
  towards non-military research.

Every AI empire defends its systems, makes peace when it is clearly losing (after 120 days),
when the enemy has no worlds left, or when war-weariness sets in, and respects truces after
peace.

**empire fleets** (`[empire.x.fleets.y]`): `display_name`, `ships = [{ ship, hull, shield }]`
(hull and shield default to full), `system` (default: the capital), `coordinates`,
`jump_charge`, and `order = { type = "move", system, coordinates }` or
`{ type = "bombard", planet = "system/planet", tile }`.

**system**: `coordinates = { x, y }` (galactic; the generator keeps them within 100 of the
centre; required). Hyperlanes are undirected, so listing a link on one side is enough.

**planet**: `size = "small" | "large"` (default large), `habitable` (default: true if large),
`coordinates = { x, y }` (fixed, relative to the star; placed automatically if missing),
`owned_by` (default: the system's owner), `moon_of`, `growth_progress`, `terraform_days_left`,
`siege`.

**tile**: `water = true` for sea tiles. `building` is either a bare reference or a
`[...tiles.N.building]` table with `type`, `upgrades`, `workers`, `job_cap`, `pinned`,
`enabled` and `damage`. Natural blockers are ordinary buildings
(`building = "buildings.wetlands"`). `construction = { building = "...", days_left = N,
empire, paid }` (or `upgrade = "..."`). `empire` is who is building it (it may differ from
the planet's owner while staking a claim), and `paid` is refunded if a rival's claim cancels
the site.

## Terrain

Generated habitable planets have seas covering roughly a third of their tiles (grown outward
from a few seeds, so water forms connected bodies) and natural blockers on roughly a third of
their tiles. Barren planets are covered in barren blockers except for 0–2 tiles. A tile with a
blocker can't be built on until the blocker is cleared, which costs its `remove_cost` and is
paid by the planet's owner (or the system's owner, for an unowned planet). Blockers in
unowned systems can't be cleared. Bombardment ignores blockers. Terraforming makes a planet
habitable but does not change its terrain.

## Expansion, ships and war

- Every empire gains `empire_income` each day (5 influence, capped at 1000).
- Building in an unowned system costs `expansion_base_cost * 2^n` influence on top of the
  building's cost and takes `expansion_base_days * 2^(n/2)` extra days, where n is the
  number of jumps from the empire's borders minus one. You cannot build in a system another
  empire owns. Finishing the building claims the system and its unowned planets, and cancels
  and refunds every rival site there.
- A system only changes owner once its owner holds none of its planets.
- Ships are ordered at a planet with a shipyard. The non-shipbuilding part of the cost is paid
  up front. Shipyards only staff up while ships are queued, and their shipbuilding output feeds
  the queue oldest-first. Finished ships join a fleet in the shipyard's system.
- Fleets travel along hyperlanes. Before each jump they charge for `hyperjump_days` (times each
  ship's `hyperjump_mult`, so the whole fleet waits for its slowest ship), then arrive on the
  system rim facing where they came from.
- Fleets of empires at war fight whenever they share a system, in `combat_subticks` steps per
  day, before any economy runs. Shields absorb shield damage first, and whatever breaks
  through hits the hull. Ships regenerate shields, and repair in friendly systems, as long as
  their upkeep is paid.
- Fleets bombard planets of empires they are at war with (or unowned ones) only when no
  enemy fleet is present. Bombardment damages buildings (they work at `1 - damage` and stop
  past `building_disable_damage`), costs the planet amenities, and builds up siege. A planet
  surrenders once siege exceeds `surrender_resistance * (1 + pops/500) * (0.5 +
  stability/100)`. Barren or empty planets surrender immediately.

## Tile numbering

Tile 0 is the north pole. Tiles go by latitude band from north to south, then eastward by
longitude within a band. The last tile is the south pole. Run `spacebox tiles small` or
`spacebox tiles large` to print each tile's position and neighbours.
