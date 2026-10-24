# spacebox-core

The Spacebox simulation engine: TOML loading, the parallel tick loop, saves, and galaxy
generation. See `../design.md` for the concept and `../data/README.md` for the data format.

## CLI

```sh
cargo build --release
B=target/release/spacebox

$B check ../data/scenarios/sol.toml                     # validate a file
$B run ../data/scenarios/sol.toml --days 720 --report 180 --out ../data/saves/sol-720.toml
$B generate ../data/base.toml --systems 500 --empires 8 --seed 42 --out ../data/saves/galaxy.toml
$B info ../data/saves/sol-720.toml --planet sol/sol-3   # tiles, jobs, stability, flows
$B tiles large                                          # tile numbering and adjacency
```

## Library

```rust
let (mut world, warnings) = spacebox_core::load_world("data/scenarios/sol.toml")?;
let earth = world.planet_by_name("sol/sol-3").unwrap();
let farm = world.defs.find_building("farm").unwrap();
world.queue(spacebox_core::Command::Build { planet: earth, tile: 20, building: farm });
world.run(360);
for event in world.drain_events() { println!("{event}"); }
spacebox_core::save::save(&world, "data/saves/out.toml")?;
```

## How a tick works

One tick is one day (`src/sim/mod.rs`):

0. **Commands.** Player orders, game-master edits and last tick's governor decisions are
   applied in order. Invalid commands become `CommandRejected` events.
1. **Space** (parallel per system). Fleets move and charge hyperdrives. Fleets at war fight
   in sub-ticks, and idle warships bombard their targets. Bombardment is then applied
   serially: building damage, siege, surrenders. This runs before any economy, as the design
   requires.
2. **Planets** (parallel). Construction and terraforming progress. Pops fill jobs: pinned jobs
   first, then by appeal. Each building's inputs, outputs and upkeep come from its
   upgrades, adjacent buildings, technology modifiers, tile multipliers, workforce, stability,
   shortages and bombardment damage. Shipyards only staff up while ships are queued. Pops
   eat, grow or starve. Unemployed or homeless pops offer to emigrate, and open jobs with free
   housing become vacancies.
3. **Systems** (parallel). Sum planet flows per owner. Match emigrants to vacancies inside
   the system.
4. **Empires** (parallel). Planets belong to whoever owns them, so a system can contain several
   ownership groups. An empire's systems (those it owns, or owns planets in) that connect to
   its capital form the core pool. That pool receives the empire's daily income (influence)
   and pays fleet upkeep. Cut-off systems form isolated pools with no access to storage.
   Surplus shipped out of a non-capital system loses `logistics_loss`. Leftover migrants are
   matched across the empire.
5. **Galaxy** (serial). On the galactic market, empires sell stock above the reserve and buy
   what storage cannot cover, paying in energy. Remaining migrants are matched across
   empires that accept them.
6. **Settlement** (parallel). Net flows go into storage. A stockpiled resource that runs dry
   with unmet demand starts a crisis, which costs stability empire-wide until it is resolved.
   Research advances, and shipyards' on-demand shipbuilding feeds the ship queue. Each pool
   publishes its *satisfaction* (the fraction of demand it can meet) and *scarcity* per
   resource for the next tick. Then migrations are committed, finished ships launch, and
   completed expansion sites claim their systems (refunding rival sites).
7. **Governors** (parallel). AI empires (those with an `[empire.x.ai]` personality) queue
   construction, upgrades, terraforming, expansion claims, research and shipbuilding. They
   declare war on neighbours they can beat (depending on how militant they are), make peace
   when losing or war-weary, defend their systems, merge their fleets, and send them to
   bombard the nearest enemy world. After a planet surrenders, an AI fleet moves on to the
   next enemy or unowned planet in the same system until the whole system is theirs; player
   fleets stand down instead. Decisions use a deterministic per-day roll, so runs stay
   reproducible.

Shortages feed back with a one-tick lag. A consumer gets `satisfaction × demand` of what it
needs, so planets never read each other's state inside a tick. Scarcity raises the appeal of
jobs that produce scarce resources, by up to `scarcity_appeal`. That is how low-appeal but
essential jobs, like farming during a famine, get staffed. Amenity jobs get the same bonus
when their own planet is short on amenities.

## Performance

Planets sit in one flat `Vec`, stored contiguously per system. Building profiles are cached
until a tile or the owner's technologies change. All per-entity work is `rayon` parallel, and
the results are deterministic: reductions happen in a fixed order.

On a 16-thread machine, a generated galaxy with 20,000 systems, ~110,000 planets and 100 AI
empires (expanding, clearing land, building fleets and waging wars) simulates in about 30 ms
per day.
