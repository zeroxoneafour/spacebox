//! Dense integer handles for every kind of definition and game object.
//!
//! Everything is resolved to these at load time so the tick loop never touches strings.

macro_rules! id_types {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            #[inline]
            pub fn idx(self) -> usize {
                self.0 as usize
            }

            #[inline]
            pub fn from_idx(i: usize) -> Self {
                Self(i as u32)
            }
        }
    )*};
}

id_types!(
    /// A loaded TOML file.
    FileId,
    ResourceId,
    JobId,
    SpeciesId,
    BuildingId,
    /// A building upgrade (a `[building.*]` table with `applies-to`).
    UpgradeId,
    TechId,
    RulesetId,
    /// An interned building tag (e.g. `"forge"`, `"barren"`).
    TagId,
    EmpireId,
    SystemId,
    PlanetId,
    /// An economic pool: a connected set of systems that share resources.
    PoolId,
    ShipId,
    WeaponId,
    OutfitId,
    /// A fleet. Unlike other ids this is stable: it never shifts when fleets are removed.
    FleetId,
);
