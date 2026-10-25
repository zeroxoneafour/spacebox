## optimization

Spacebox is written from the ground up for ultra-high multithreaded performance. The order of command for each tick of the game engine should be -

1. Update planet economies/populations on a per-planet basis. This calculation should be parallelizable as it should be independent of other planets at the time of the tick. Calculate the ingress/egress of resources and population to the planet, and store this information for use in the systems calculation.
2. Update system economies/populations. Same with planets, should be parallelizable. Use the ingress/egress of resources and population, try to balance resources/pop as best as possible given the outputs/inputs of planets within the system, and then store the remaining surplus/deficit values for later use.
3. Update empire economies/populations. Follows same pattern as above.
4. Update galactic economy. After updating galactic economy, go back to each empire and check for remaining surplus/deficits. If they still exist, then put those in the empire's storage (or if it is a deficit, start an empire crisis).

Space fights should be simulated per-system.
