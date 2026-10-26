# spacebox-core

The Spacebox simulation engine: TOML loading, the parallel tick loop, saves, and galaxy
generation. See `../design/` for the spec and `../data/README.md` for how the engine reads it.

## CLI

```sh
cargo build --release
B=target/release/spacebox

$B check ../data/scenarios/sol.toml                     # validate a file
$B run ../data/scenarios/sol.toml --days 720 --report 180 --out /tmp/sol-720.toml
$B generate ../data/base.toml --systems 500 --empires 8 --seed 42 --out /tmp/galaxy.toml
$B info /tmp/sol-720.toml --planet sol/sol-3            # tiles, jobs, species, stability, flows
$B tiles large                                          # tile numbering and adjacency
```

## Library

```rust
let (mut world, warnings) = spacebox_core::load_world("data/scenarios/sol.toml")?;
let earth = world.planet_by_name("sol/sol-3").unwrap();
let farm = world.defs.find_building("farm").unwrap();
world.queue(spacebox_core::Command::Build { planet: earth, tile: 20, building: farm, empire: None });
world.run(360);
for event in world.drain_events() { println!("{event}"); }
spacebox_core::save::save(&world, "data/saves/out.toml")?;
```

## How a tick works

One tick is one day (`src/sim/mod.rs`):

0. **Commands.** Player orders, game-master edits and last tick's governor decisions are
   applied in order. Invalid commands become `CommandRejected` events.
1. **Space** (parallel per system). Fleets work through their order queues: moving within
   the system, flying to the edge facing their next jump and charging there, or bombarding a
   planet. Fleets at war fight in sub-ticks instead, their orders waiting until the combat is
   over. Bombardment is then applied serially: building damage, siege, surrenders. This runs
   before any economy, as the design requires.
2. **Planets** (parallel). The days of tile projects and terraforming tick down, and
   projects with nothing left complete. Pops fill jobs: pinned jobs first, then by appeal.
   Each building's inputs, outputs, passive outputs, amenities and upkeep come from its
   upgrades, adjacent buildings, technology modifiers, tile multipliers, workforce,
   stability, shortages and bombardment damage. Each species eats its own upkeep and grows
   or starves at its own rates. Unemployed or homeless pops offer to emigrate, and open jobs
   with free housing become vacancies.
3. **Systems** (parallel). Sum planet flows per owner. Match emigrants to vacancies inside
   the system.
4. **Empires** (parallel). Planets belong to whoever owns them, so a system can contain several
   ownership groups. An empire's systems (those it owns, or owns planets in) that connect to
   its capital form the core pool. That pool receives the empire's daily expansion resource
   (influence) and pays fleet upkeep. Cut-off systems form isolated pools with no access to storage.
   Surplus shipped out of a non-capital system loses `logistics_loss`. Leftover migrants are
   matched across the empire.
5. **Galaxy** (serial). On the galactic market, empires sell stock above the reserve and buy
   what storage cannot cover, paying in energy. Remaining migrants are matched across
   empires that accept them.
6. **Settlement** (parallel). Net flows go into storage. A stockpiled resource that runs dry
   with unmet demand starts a crisis, which costs stability empire-wide until it is resolved.
   Each pool publishes its *satisfaction* (the fraction of demand it can meet) and
   *scarcity* per resource for the next tick. Then (serially) every project draws normal
   resources from its empire's stockpile (recording what it paid); ships take their own
   planet's shipbuilding, research the rest of the empire's transient output, and whatever is
   left is wasted. Research completes, migrations are committed, finished ships launch as new
   fleets, and completed expansion sites claim their planet and system (refunding rival
   projects). Wars then update (`src/sim/war.rs`): humiliation counts down, exhaustion
   rises, and wars where one side has no planets left end.
7. **Governors** (parallel). AI empires (those with an `[empire.x.ai]` personality) queue
   housing on empty worlds, construction, upgrades, terraforming, expansion claims, research
   and shipbuilding, and switch off labs and shipyards whose output nothing would use. They
   set their total-war policy from their militancy, claim neighbouring systems, declare
   humiliation, claim or total wars on neighbours they can beat (depending on how militant
   they are), choose their goal when attacked, force, propose and accept peace by
   exhaustion, defend their systems, liberate occupied worlds, merge their fleets, and send them to
   bombard the nearest enemy world. After a planet surrenders, an AI fleet moves on to the
   next enemy or unowned planet in the same system until the whole system is theirs; player
   fleets stand down instead. Decisions use a deterministic per-day roll, so runs stay
   reproducible.

Shortages feed back with a one-tick lag. A consumer gets `satisfaction × demand` of what it
needs, so planets never read each other's state inside a tick. Scarcity raises the appeal of
jobs whose `associated-resource` is scarce, by up to `scarcity-appeal`. That is how low-appeal but
essential jobs, like farming during a famine, get staffed. Amenity jobs get the same bonus
when their own planet is short on amenities.

## Performance

Planets sit in one flat `Vec`, stored contiguously per system. Building profiles are cached
until a tile or the owner's technologies change. All per-entity work is `rayon` parallel, and
the results are deterministic: reductions happen in a fixed order.

Most planets in a large galaxy are empty, unowned rocks. A planet whose last tick had nothing
to simulate (no active buildings, pops, projects, siege or damage) is skipped until something
changes, and so are systems and resource pools made only of such planets. Steps that need only
the busy planets (projects, claims, events) find them with one parallel pass.

Large saves are parsed piecewise and in parallel (see `src/split.rs`), and saves are streamed
to disk.

On a 16-thread machine, a generated galaxy with 20,000 systems, ~110,000 planets and 100 AI
empires (expanding, clearing land, building fleets and waging wars):

| | Time | Peak memory |
| --- | --- | --- |
| Simulate one day | ~8 ms | |
| Load its 200 MB save | 1.7 s | 560 MB (the world itself is ~300 MB) |
| Generate it | 2.8 s | 290 MB |
