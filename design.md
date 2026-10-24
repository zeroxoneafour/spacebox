# design

This document outlines the design of spacebox. The code is built around it by AI, but this file should not be changed by AI.

## what is spacebox?

Spacebox is a galactic-scale economy simulation in the vein of Stellaris. I would call it a game in the same sense that Dwarf Fortress is a game.
It is meant to provide an environment for roleplay and storytelling, not really for a balanced and traditionally fun player experience.

All that to say, if you enjoy being a cog in the Spacebox, it can be pretty fun at times.

## basic outline

The galaxy in Spacebox is comprised of systems, which each have planets orbiting one system of star(s). Most systems have no habitable planets.

Planets can either be large or small. Gas giant moons can count as small planets, but gas giants themselves are not planets.
Small planets are represented as regular dodecahedrons (12 faces). Large planets are represented as truncated icosahedrons (32 faces).

Each face of a planet can be built on. Planets may either be barren or habitable (small planets always start barren).
If the planet is barren, you may only build structures that are marked as being buildable on barren planets.
You can unlock technology to first terraform big planets, and eventually terraform small ones too.

On average, a system may have around 0-2 habitable planets, 1-3 barren large planets, and 2-4 small planets/gas giant moons.

### buildings

Buildings form the core resource harvesting/production capabilities for planets.
Buildings employ up to a set amount of pops, and their production scales with the amount they employ.
Buildings can also have adjacency bonuses, where if two buildings (ex. a mine and an alloy factory) are adjacent, they get benefits.

### population

Population can be manually moved in between jobs/planets/systems. These ingress/egress changes are queued for the tick and applied in the core tick loop.

They also naturally immigrate to find jobs and opportunities. Pops will try to find jobs on their planet that have better "appeal".
If they can't, then they will work on their planet until there are no more job opportunities at all.
Then they will start looking up to the system, and eventually the galaxy.

## file structures

All of the save data/asset files are represented as toml files. This allows the game to be extremely moddable.

The toml files are designed to be combined together. At the top of each file can be an `[import]` section with relative imports, like -

```
[import]
resources = "./resources.toml" # or simply "resources.toml" since its in the same directory
```

or the like. You can later reference a resource (in an appropriate slot) with something like `output_resource = "resources.alloy"`.

Note that each of the subsections below is designed to be combined with each other, and may all appear more than once in a file (especially a save file). However, it is very important to note that the `[import]` section may only appear once (and may not appear) at the top of the file.

The engine should automatically figure out which table to take a value from. For example, if resources.toml is imported as "abc", and some `output_resource[0].resource = "abc.def"`, since the engine knows `resource` must take in a value of resource, it looks for a `[resource.def]` in resources.toml.
That is to say, everything should be implicitly strongly-typed even in toml.

Imports should not recurse, i.e. you should not be able to reference things in a file that is imported by your import unless you also import that file.

### asset data stuff

The stuff below is more likely to be used as static(ish) assets that can be referenced by save files.

### resources

Resources toml section is simple and looks like this -

```
[resource.alloy]
display_name = "Alloy"
description = "Advanced metal alloy" # optional
image = "../alloy.png" # optional
```

### jobs/people

```
[job.metallurgist]
display_name = "Metallurgist"
appeal = 70
```

### buildings

```
[building.alloy-forge]
display_name = "Alloy Forge"
max_workers = 100
job = "jobs.metallurgist"
building_class = [ "forge" ] # buildings that can be built on barren planets must have 0 max workers and "barren" in their building class

# all of these are daily
maintainence_resources = [
    { resource = "resources.electricity", count = 10 }
]

# inputs and outputs are daily and scale linearly with workers/max_workers
input_resources = [
    { resource = "resources.minerals", count = 10 }
]
output_resources = [
    { resource = "resources.alloy", count = 1 }
]

adjacency_bonus = [
    { building = "mine", input_mult = 0.95 }
]
```

```
[building.housing]
display_name = "Basic Housing"
housing = 500
building_class = [ "housing" ]
maintainence_resources = [
    { resource = "resources.electricity", count = 1 }
]
```

```
[building-upgrade.faster-forge]
apply_to = [ "alloy-forge" ] # can be building classes or buildings. Note that the `building.` prefix is unecessary because they are in the same file.

# this upgrade is base, but if it wasnt this is what dependencies would look like.
# if it is impossible to get a dependency (ex. dependency is not available for building), make this upgrade unavailable for the building too.
#needs_upgrades = [ "fast-forge" ]

# these scale all resources. Scaling is applied after all resources from all upgrades/adjacency bonuses are summed together.
maintainence_mult = 1.1
input_mult = 1.25
output_mult = 1.25

# this isnt used for this upgrade but heres an example of what adding another input/output looks like.
#output_resources = [ { resource = "resources.poop", count = 1 } ]
```

### save file stuff

The stuff below is more likely to be used in a save file than in raw asset data.

### system

```
[system.sol]
display_name = "Sol"
star_type = "yellow-small"
star_count = 1
owned_by = "empire-1"
linked_systems = ["alpha-centauri"] # optional if not linked (but this is rare)
```

### planets

```
[system.sol.planets.sol-1]
display_name = "Earth"
total_pop = 200

# figure out some way of deterministically mapping each tile number to a spot on the map of the polyhedron
[system.sol.planets.sol-1.tiles.0]
resource_multipliers = [
    { resource = "resources.alloy", mult = 1.1 },
    { resource = "resources.food", mult = 0.75 }
] # optional if no resource multiplier on tile
building = {
    name = "buildings.alloy-forge",
    upgrades = [ "faster-forge" ],
    workers = 100,
}

[system.sol.planets.sol-1.tiles.1]
building = "buildings.housing" # some buildings are passive and provide housing/amenities, etc, but do not need workers or currently have upgrades

# etc for rest of tiles... not all tiles need to be defined if not all of them are interesting
```

### empires

```
[empire.empire-1]
display_name = "Great empire of poo"
resource_stores = [
    { resource = "resources.alloy", count = 10000 } # etc
]
```

## optimization

Spacebox is written from the ground up for ultra-high multithreaded performance. The order of command for each tick of the game engine should be -

1. Update planet economies/populations on a per-planet basis. This calculation should be parallelizable as it should be independent of other planets at the time of the tick. Calculate the ingress/egress of resources and population to the planet, and store this information for use in the systems calculation.
2. Update system economies/populations. Same with planets, should be parallelizable. Use the ingress/egress of resources and population, try to balance resources/pop as best as possible given the outputs/inputs of planets within the system, and then store the remaining surplus/deficit values for later use.
3. Update empire economies/populations. Follows same pattern as above.
4. Update galactic economy. After updating galactic economy, go back to each empire and check for remaining surplus/deficits. If they still exist, then put those in the empire's storage (or if it is a deficit, start an empire crisis).
