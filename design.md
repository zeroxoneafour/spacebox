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
Habitable planets should have around ~1/3rd of their surface covered by blockers, while barren planets should have only 0-2 tiles not covered.
Habitable planets additionally have around ~1/3rd of their surface covered by water, which some buildings can be placed on.

### buildings

Buildings form the core resource harvesting/production capabilities for planets.
Buildings employ up to a set amount of pops, and their production scales with the amount they employ.
Buildings can also have adjacency bonuses, where if two buildings (ex. a mine and an alloy factory) are adjacent, they get benefits.

Special types of buildings include - 
- barren - Can be built on barren planets
- blocker - Naturally generates (and by convention has a remove cost). Combine with "barren" to generate on barren worlds, "water" to generate on water, etc.
- space - Can be built on orbital megastructures (not implemented yet)
- land - Must be built over land tiles (without water on them).
- water - Must be built over tiles with water on them.

### population

Population can be manually moved in between jobs/planets/systems. These ingress/egress changes are queued for the tick and applied in the core tick loop.

They also naturally immigrate to find jobs and opportunities. Pops will try to find jobs on their planet that have better "appeal".
If they can't, then they will work on their planet until there are no more job opportunities at all.
Then they will start looking up to the system, and eventually the galaxy.

### expansion

Empires have a resource called influence, and can expend this resource to expand their borders.
Empires automatically get 5 influence per day, and have a cap of 1000.

Building on a planet in an unowned system costs 100 * 2^n influence, where n is 1 - the amount of jumps away the system is from the current empire borders.
(ex. for an adjacent system, n = 0).
You cannot build in a system you do not own. Finishing a building automatically claims the system.
It takes 30 * 2^(n/2) additional days to build a building in an unowned system.
Successfully finishing a building automatically cancels all other empire's claims/building sites in the system and refunds their materials.

Empires can also build fleets and target buildings with their fleets.
This will cause destruction, lower amenities, and disable/eventually destroy those buildings.
At some point (factoring in planet stability, etc) the planet will surrender to the invaders. Barren/autonomous planets surrender immediately.
Empire fleets will always prioritize fighting enemy fleets over targeting buildings, so if you are being invaded, fly in your fleet for defense.

Fleets should hyper jump in a set amount of time (given by a game rule, and modifiable per-ship by hyper jump multipliers), and should wait for the entire fleet to be ready to hyper jump before jumping to an adjacent system.
Fleet combat may run on a sub-tick basis. The calculation for fleet combat should be run once per tick (divided into subticks as necessar) before the calculations for planet economy (and thus before the calculations for any economy).

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
upkeep_resources = [
    { resource = "resources.energy", count = 10 }
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
upkeep_resources = [
    { resource = "resources.energy", count = 1 }
]
```

```
[building-upgrade.faster-forge]
apply_to = [ "alloy-forge" ] # can be building classes or buildings. Note that the `building.` prefix is unecessary because they are in the same file.

# this upgrade is base, but if it wasnt this is what dependencies would look like.
# if it is impossible to get a dependency (ex. dependency is not available for building), make this upgrade unavailable for the building too.
#needs_upgrades = [ "fast-forge" ]

# these scale all resources. Scaling is applied after all resources from all upgrades/adjacency bonuses are summed together.
upkeep_mult = 1.1
input_mult = 1.25
output_mult = 1.25

# this isnt used for this upgrade but heres an example of what adding another input/output looks like.
#output_resources = [ { resource = "resources.poop", count = 1 } ]
```

Buildings can also have build/remove costs.
Natural blockers are defined as buildings.

```
[building.wetlands]
building_class = [ "blocker" ]
remove_cost = [
    { resource = "resources.energy", count = 1000 }
]
```

### ships

```
[ship.fighter]
build_cost = [
    { resource = "resources.alloy", count = 50 }, # shipbuilding sites only turn labor into shipbuilding as the resource cost is accounted for here
    { resource = "resources.shipbuilding", count = 100 } # this is a special resource generated by shipbuilding buildings on-demand
]
upkeep_cost = [
    { resource = "resources.energy", count = 1 }
]
hull = 500
shield = 350
# system internal units per day
base_speed = 10
shield_regen = 10
weapons = [ "laser", "laser" ]
outfits = [ "shield-booster", "afterburner" ]
```

```
[ship-weapon.laser]
build_cost = [
    { resource = "resources.alloy", count = 10 }
]
shield_damage = 20
hull_damage = 50
# can fire once every 0.5 days. fire rate mult divides rather than adding
fire_rate = 0.5
range = 10
```

```
[ship-outfit.shield-booster]
build_cost = [
    { resource = "resources.alloy", count = 25 }
]
shield = 50
shield_regen_mult = 1.5
```

### save file stuff

The stuff below is more likely to be used in a save file than in raw asset data.

### system

```
[system.sol]
display_name = "Sol"
star_type = "yellow-small"
star_count = 1
coordinates = { x = 10, y = 10 } # give systems coordinates relative to the galaxy center. When generating, by default, keep this under 100 radially
owned_by = "empire-1" # this only changes if all planets in the system are either unowned or owned by a different empire
linked_systems = ["alpha-centauri"] # optional if not linked (but this is rare)
```

### planets

```
[system.sol.planets.sol-1]
display_name = "Earth"
total_pop = 200
# planets should stay fixed in place and not orbit their star
coordinates = { x = 10, y = 10 } # give planets coordinates relative to the center star. Keep this under 100 radially by default
owned_by = "empire-1" # this changes if the planet is successfully invaded.

# figure out some way of deterministically mapping each tile number to a spot on the map of the polyhedron
[system.sol.planets.sol-1.tiles.0]
resource_multipliers = [
    { resource = "resources.alloy", mult = 1.1 },
    { resource = "resources.food", mult = 0.75 }
] # optional if no resource multiplier on tile

[system.sol.planets.sol-1.tiles.0.building]
type = "buildings.alloy-forge"
upgrades = [ "buildings.faster-forge" ]
workers = 100

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

```
[empire.empire-1.ai] # presence of this table means the empire is ai
militant = 0.9 # scale 0 to 1 for all these parameters
expansionist = 0.5
scientific = 0.4
```

```
[empire.empire-1.fleets.fleet-1]
display_name = "Great fleet of poo"
ships = [
    { ship = "ships.fighter", hull = 500, shield = 350 }
]
```

## optimization

Spacebox is written from the ground up for ultra-high multithreaded performance. The order of command for each tick of the game engine should be -

1. Update planet economies/populations on a per-planet basis. This calculation should be parallelizable as it should be independent of other planets at the time of the tick. Calculate the ingress/egress of resources and population to the planet, and store this information for use in the systems calculation.
2. Update system economies/populations. Same with planets, should be parallelizable. Use the ingress/egress of resources and population, try to balance resources/pop as best as possible given the outputs/inputs of planets within the system, and then store the remaining surplus/deficit values for later use.
3. Update empire economies/populations. Follows same pattern as above.
4. Update galactic economy. After updating galactic economy, go back to each empire and check for remaining surplus/deficits. If they still exist, then put those in the empire's storage (or if it is a deficit, start an empire crisis).

Space fights should be simulated per-system.
