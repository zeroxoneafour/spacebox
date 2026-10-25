# basic outline

The galaxy in Spacebox is comprised of systems, which each have planets orbiting one system of star(s). Most systems have no habitable planets.

Planets can either be large or small. Gas giant moons can count as small planets, but gas giants themselves are not planets.
Small planets are represented as regular dodecahedrons (12 faces). Large planets are represented as truncated icosahedrons (32 faces).

Each face of a planet can be built on. Planets may either be barren or habitable (small planets always start barren).
If the planet is barren, you may only build structures that are marked as being buildable on barren planets.
You can unlock technology to first terraform big planets, and eventually terraform small ones too.

On average, a system may have around 0-2 habitable planets, 1-3 barren large planets, and 2-4 small planets/gas giant moons.
Habitable planets should have around ~1/3rd of their surface covered by blockers, while barren planets should have only 0-2 tiles not covered.
Habitable planets additionally have around ~1/3rd of their surface covered by water, which some buildings can be placed on.

## buildings

Buildings form the core resource harvesting/production capabilities for planets.
Buildings employ up to a set amount of pops, and their production scales with the amount they employ.
Buildings can also have adjacency bonuses, where if two buildings (ex. a mine and an alloy factory) are adjacent, they get benefits.

Special types of buildings include - 
- barren - Can be built on barren planets
- blocker - Naturally generates (and by convention has a remove cost). Combine with "barren" to generate on barren worlds, "water" to generate on water, etc.
- space - Can be built on orbital megastructures (not implemented yet)
- land - Must be built over land tiles (without water on them).
- water - Must be built over tiles with water on them.

## population

Population can be manually moved in between jobs/planets/systems. These ingress/egress changes are queued for the tick and applied in the core tick loop.

They also naturally immigrate to find jobs and opportunities. Pops will try to find jobs on their planet that have better "appeal".
If they can't, then they will work on their planet until there are no more job opportunities at all.
Then they will start looking up to the system, and eventually the galaxy.

## expansion

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

## resources

Resources can either be transient or normal.
All resources are produced at all times when possible, but transient resources that are not redirected into something are left wasted.

Normal resources stockpile, and when something new needs to be done, it takes out of the stockpile first before taking from the daily income.
Actions that use normal resources will also refund the resources if canceled. This is not true for transient resources.

Transient resources include shipbuilding (which is used to create ships) and research (which is used for research).
If you have shipbuilders and researchers employed but no ships to build or things to research, you will be wasting the transient resources.
So either disable your shipyards and labs, or better yet, put them to good use!

Super special resources include days and amenities.
Days remaining on a project tick down once per day (as you would expect), and generated amenities simply add to an amenities sum per-planet.
