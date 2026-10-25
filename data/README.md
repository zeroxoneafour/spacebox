# Spacebox data

All game content is TOML, in the format of the spec in `../design/` (see
`design/data/readme.md` and its example files; `design/data/savegame.toml` loads as is). This
file documents how the engine reads it and what it adds on top. Fields marked *(addition)* are
not in the spec.

| File | Contents |
| --- | --- |
| `resources.toml` | 11 resources: basic (energy, minerals, food), advanced (alloy, consumer goods), strategic (volatile motes, exotic gases, rare crystals), influence, and the transient research and shipbuilding |
| `jobs.toml` | 13 jobs with appeal, tags and associated resources |
| `species.toml` | Humans, Lithoids (eat minerals) and Hive Drones (no comfort needs) |
| `buildings.toml` | Housing, production, industry, refineries, research, shipyards, barren-world automation, natural blockers, and building upgrades |
| `technologies.toml` | A 24-technology tree that unlocks buildings, upgrades, ship designs and terraforming, with pop, ship and building effects |
| `ships.toml` | 7 ship designs, 5 weapons and 6 outfits |
| `rules.toml` | `[ruleset.default]` |
| `base.toml` | Index that imports everything (used by `spacebox generate`) |
| `mods/orbital-agriculture.toml` | Example mod: a barren farm carrying the base game's `agriculture` tag, plus an upgrade for a base-game building |
| `scenarios/sol.toml` | Hand-written scenario: Terran Federation vs. Cetian Concord |

## Imports and references

- `[import]` must be the first section, if present. Paths are relative to the importing file.
- `"alias.name"` looks `name` up in the imported file. A bare `"name"` looks it up in the
  current file. Imports do not recurse: you can only use aliases your own file declares.
- The slot decides which table a reference points into: `resource = "x.y"` looks for
  `[resource.y]`, `job = "x.y"` for `[job.y]`, an upgrade slot for `[building-upgrade.y]`,
  and so on.
- **Tags** (`building-tags`, `job-tags`, `research-tags`) are scoped the same way: `"forge"`
  is the `forge` tag of the current file, `"base.forge"` that of the file imported as `base`.
  The reserved building tags (`barren`, `blocker`, `land`, `water`, `space`) are global. Tags
  need no declaration, so the loader warns about tags that upgrades, adjacency bonuses or
  technologies target but no building has.
- Anything that targets buildings has separate `buildings = [...]` and `building-tags = [...]`
  lists (upgrades, adjacency bonuses, technology building effects).
- Keys are kebab-case. Unknown top-level tables and unknown fields are errors, so typos are
  caught.
- Files must be standard TOML 1.0. TOML 1.1 extensions such as multi-line inline tables are
  rejected.
- Only the current format loads. The project is pre-alpha, so old saves are not migrated.
- Asset tables load from every reachable file. Game state (`[game]`, `[empire.*]`,
  `[system.*]`) is only read from the file you load, so a save can import a scenario for its
  assets without duplicating its systems.

## Lists with special entries

- **Costs** (`build-cost`, `remove-cost`, terraforming `cost`, `remaining`) mix
  `{ resource, count }` with `{ days = N }`.
- **Outputs** (`outputs`, `passive-outputs`, `output-add`, `passive-output-add`) mix
  `{ resource, count }` with `{ amenities = N }`.
- **`paid`** lists only normal resources: transient resources and days can't be refunded.

## Housing

There is no base housing: a planet only houses what its buildings provide. Pops can't be moved
to a planet without free housing, and migrants only go where there is housing, so a new
colony needs a housing building first.

## Projects

Constructions, removals, upgrades, terraforming, research and ships are all *projects* with a
list of what they still need. Every day, after the economy settles, each empire's projects
draw normal resources from its stockpile (which has just received the day's income) as soon as
they are available, recording them in `paid`, in a fixed order: tile projects and
terraforming planet by planet, then ships, then research. Their days tick down at the same
time, and a project finishes when nothing is left. A project can be started without the
resources in hand; it just waits.

Transient resources (`transient = true`: research, shipbuilding) are never stored:

- A ship is only fed by the transient output (shipbuilding) of the planet it is built at, so
  ships need a shipyard on their own planet.
- Research uses the rest of the empire's transient output, scaled by `research-speed-mult`.
- Whatever no project uses is wasted.

Cancelling a project refunds its `paid` resources, except the expansion resource (influence),
which is never refunded. Research has no `paid` list; its refund is its cost minus what
remains.

Research is a list of technologies. A technology can be added once its prerequisites are known
*or* in the list; it waits (unfunded) until they are known. Cancelling one also cancels what
waits on it.

## Fleets

A fleet works through its `orders` queue:

- `{ order = "move", coords }` flies to a point in the current system.
- `{ order = "jump", system }` flies to the edge of the system facing an adjacent system
  (`arrival-radius` from the star), then charges every ship's `jump-charge` from 0 to 1 while
  standing still there (taking `hyperjump-days × hyperjump-mult` days per ship). Once all are
  charged the fleet jumps, arrives on the rim of the next system facing where it came from,
  and the charges reset.
- `{ order = "bombard", planet, tile }` closes on a planet in the current system and fires on
  it until it surrenders.

A fleet is `in-combat` while an enemy fleet shares its system (and either is armed). Its orders
wait until the combat is over. New ships always form a fleet of their own; fleets in the same
system can be merged. A fleet holds at most `base-fleet-size` plus its empire's
`fleet-size-add` slots, and each ship takes its design's `fleet-size`. Bombarding an unowned
planet in another empire's system is not allowed.

Jumps must follow hyperlanes: in a save, each jump is checked against the system the fleet
will be in by then, and so is each bombardment target.

## Field reference (beyond the spec)

**building**: buildings without `max-workers` run at 100%; with it, outputs and inputs scale
with the share of jobs filled. A building with a `job` must have `max-workers`. Modifiers also
accept `housing-add` / `housing-mult` *(addition)*. Adjacency entries stack once per matching
neighbour.

| Reserved tag | Meaning |
| --- | --- |
| `barren` | Can be built on barren planets |
| `blocker` | Generated with planets, never constructed; cleared with a removal project (`remove-cost`). With `barren` it appears on barren worlds, with `water` on water tiles, otherwise on habitable land |
| `land` | Can stand on land tiles. Implicit unless the building has `water` or `space` |
| `water` | Can stand on water tiles |
| `space` | Belongs on orbital megastructures, which are not implemented yet |

**building-upgrade**: `display-name`, `description`, `buildings`, `building-tags`,
`after-upgrades`, `requires-tech`, `build-cost`, and modifiers: `upkeep-cost-add/-mult`,
`input-add/-mult`, `output-add/-mult`, `passive-output-add/-mult`, `max-workers-add/-mult`,
`housing-add/-mult` and `job` (replaces the building's job). All additions from upgrades,
adjacency and technologies are summed before all multipliers are applied.

**resource**: `base-value` is relative to the currency's value: a resource worth 1 costs
`1 / currency base-value` currency on the galactic market (before supply and demand).

**planet**: `siege` and `terraform = { paid, remaining }` *(additions)*. Without an `owner`
a planet is unowned, even in an owned system.

**empire AI** (`[empire.x.ai]`): `militant`, `expansionist` and `scientific` are each 0–1
(default 0.5):

- **militant**: fleet size (0.4×–1.6× the baseline), more ships in production, declaring war
  on a neighbour within three jumps once its fleet outmatches the target's by
  `1.8 - 1.2 × militant`, one or (above 0.75) two wars at a time, attacking with smaller
  fleets, fighting longer before war-weariness (360 + 1440 × militant days), and favouring
  technologies tagged `military`.
- **expansionist**: claims systems up to 1–3 jumps out, keeps less influence in reserve
  (`2 - expansionist` × the cost), and runs up to three claims at once.
- **scientific**: more technologies in research at once, and a lean towards non-military
  research.

AI empires build housing before anything else on empty worlds, keep enough research queued to
use their output, switch off labs and shipyards whose output nothing would use, gather their
new ships at the capital and merge them, and go after populated enemy worlds.

## Terrain

Generated habitable planets have seas covering roughly a third of their tiles (grown outward
from a few seeds, so water forms connected bodies) and natural blockers on roughly a third of
their tiles. Barren planets are covered in barren blockers except for 0–2 tiles. A blocker is
cleared with a removal project paid by the planet's owner (or the system's owner, for an
unowned planet); blockers in unowned systems can't be cleared. Bombardment ignores blockers.
Terraforming makes a planet habitable but does not change its terrain.

## Expansion and war

- Every empire gains `expansion-resource-income` of the expansion resource each day (5
  influence, capped at 1000 by `storage-caps`).
- Building in an unowned system adds `expansion-base-cost * 2^n` influence and
  `expansion-base-days * 2^(n/2)` days to the project, where n is the number of jumps from the
  empire's borders minus one. You cannot build in a system another empire owns. Finishing the
  building claims that planet and the system (its other planets stay unowned, but only the
  owner can claim them) and cancels every rival project there, refunding what they paid
  except influence.
- A system only changes owner once its owner holds none of its planets.
- Fleets of empires at war fight whenever they share a system, in `combat-subticks` steps per
  day, before any economy runs. Shields absorb shield damage first, and whatever breaks
  through hits the hull. Ships regenerate shields, and repair in friendly systems, as long as
  their upkeep is paid.
- Bombardment damages buildings (they work at `1 - damage` and stop past
  `building-disable-damage`), costs the planet amenities, and builds up siege. A planet
  surrenders once siege exceeds `surrender-resistance * (1 + pops/500) * (0.5 +
  stability/100)`. Barren or empty planets surrender immediately.

## Tile numbering

Tile 0 is the north pole. Tiles go by latitude band from north to south, then eastward by
longitude within a band. The last tile is the south pole. Run `spacebox tiles small` or
`spacebox tiles large` to print each tile's position and neighbours.
