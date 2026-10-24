//! Planet surface geometry.
//!
//! Small planets are regular dodecahedra (12 pentagonal faces); large planets are truncated
//! icosahedra (12 pentagons + 20 hexagons = 32 faces). Each face is a buildable tile.
//!
//! # Deterministic tile numbering
//!
//! Both solids are derived from the same icosahedron, oriented so that one vertex points at the
//! north pole (+Z). Dodecahedron faces sit on icosahedron vertices; truncated-icosahedron
//! pentagons sit on icosahedron vertices and its hexagons sit on icosahedron face centres.
//!
//! Tiles are numbered by latitude band from north to south, and within a band by longitude
//! (eastward from +X, in `[0°, 360°)`). Tile 0 is therefore always the north-polar pentagon and
//! the last tile is always the south-polar pentagon. For a small planet that gives:
//!
//! ```text
//!   tile 0          north pole
//!   tiles 1..=5     northern band (latitude ≈ +26.6°)
//!   tiles 6..=10    southern band (latitude ≈ −26.6°)
//!   tile 11         south pole
//! ```
//!
//! and for a large planet (P = pentagon, H = hexagon):
//!
//! ```text
//!   tile 0          P  north pole
//!   tiles 1..=5     H  latitude ≈ +52.6°
//!   tiles 6..=10    P  latitude ≈ +26.6°
//!   tiles 11..=15   H  latitude ≈ +10.8°
//!   tiles 16..=20   H  latitude ≈ −10.8°
//!   tiles 21..=25   P  latitude ≈ −26.6°
//!   tiles 26..=30   H  latitude ≈ −52.6°
//!   tile 31         P  south pole
//! ```
//!
//! `spacebox tiles small|large` prints the full adjacency tables.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlanetSize {
    Small,
    Large,
}

impl PlanetSize {
    pub fn tile_count(self) -> usize {
        self.shape().tile_count()
    }

    pub fn shape(self) -> &'static Polyhedron {
        static SMALL: OnceLock<Polyhedron> = OnceLock::new();
        static LARGE: OnceLock<Polyhedron> = OnceLock::new();
        match self {
            PlanetSize::Small => SMALL.get_or_init(Polyhedron::dodecahedron),
            PlanetSize::Large => LARGE.get_or_init(Polyhedron::truncated_icosahedron),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PlanetSize::Small => "small",
            PlanetSize::Large => "large",
        }
    }
}

/// Face layout of a planet solid.
#[derive(Debug)]
pub struct Polyhedron {
    /// Unit vector from the planet centre to each face centre.
    pub centers: Vec<[f64; 3]>,
    /// Number of edges of each face (5 or 6).
    pub sides: Vec<u8>,
    /// Indices of the faces sharing an edge with each face, sorted ascending.
    pub neighbors: Vec<Vec<u8>>,
    /// Corner positions of each face (circumradius 1), counter-clockwise seen from outside.
    pub faces: Vec<Vec<[f64; 3]>>,
}

const PHI: f64 = std::f64::consts::GOLDEN_RATIO;

impl Polyhedron {
    pub fn tile_count(&self) -> usize {
        self.centers.len()
    }

    pub fn neighbors(&self, tile: usize) -> &[u8] {
        &self.neighbors[tile]
    }

    pub fn are_adjacent(&self, a: usize, b: usize) -> bool {
        self.neighbors[a].binary_search(&(b as u8)).is_ok()
    }

    /// Latitude and longitude of a tile centre, in degrees.
    pub fn lat_lon(&self, tile: usize) -> (f64, f64) {
        let [x, y, z] = self.centers[tile];
        let lat = z.clamp(-1.0, 1.0).asin().to_degrees();
        (lat, longitude(x, y).to_degrees())
    }

    fn dodecahedron() -> Self {
        let (verts, tris) = icosahedron();
        let edges = icosa_adjacency(&verts);
        // Dodecahedron corners are the (normalised) centres of the icosahedron's faces.
        let faces: Vec<Face> = verts
            .iter()
            .enumerate()
            .map(|(i, v)| Face {
                center: *v,
                sides: 5,
                neighbors: edges[i].clone(),
                corners: tris
                    .iter()
                    .filter(|t| t.contains(&i))
                    .map(|t| normalize(add3(add3(verts[t[0]], verts[t[1]]), verts[t[2]])))
                    .collect(),
            })
            .collect();
        order_faces(faces)
    }

    fn truncated_icosahedron() -> Self {
        let (verts, tris) = icosahedron();
        let edges = icosa_adjacency(&verts);
        let n = verts.len();
        let mut faces: Vec<Face> = verts
            .iter()
            .enumerate()
            .map(|(i, v)| Face {
                center: *v,
                sides: 5,
                // A pentagon borders the hexagon of every icosahedron face that uses its vertex.
                neighbors: tris
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.contains(&i))
                    .map(|(f, _)| n + f)
                    .collect(),
                // Truncation cuts every icosahedron edge at its thirds.
                corners: edges[i]
                    .iter()
                    .map(|&j| lerp(*v, verts[j], 1.0 / 3.0))
                    .collect(),
            })
            .collect();
        for (f, tri) in tris.iter().enumerate() {
            let c = normalize(add3(add3(verts[tri[0]], verts[tri[1]]), verts[tri[2]]));
            let mut neighbors: Vec<usize> = tri.to_vec();
            // Hexagons border each other where their icosahedron faces share an edge.
            for (g, other) in tris.iter().enumerate() {
                if g != f && other.iter().filter(|v| tri.contains(v)).count() == 2 {
                    neighbors.push(n + g);
                }
            }
            let mut corners = Vec::with_capacity(6);
            for k in 0..3 {
                let (a, b) = (verts[tri[k]], verts[tri[(k + 1) % 3]]);
                corners.push(lerp(a, b, 1.0 / 3.0));
                corners.push(lerp(a, b, 2.0 / 3.0));
            }
            faces.push(Face {
                center: c,
                sides: 6,
                neighbors,
                corners,
            });
        }
        debug_assert!(edges.iter().all(|e| e.len() == 5));
        order_faces(faces)
    }
}

struct Face {
    center: [f64; 3],
    sides: u8,
    neighbors: Vec<usize>,
    corners: Vec<[f64; 3]>,
}

fn lerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Sorts corners counter-clockwise around the outward normal `center`.
fn sort_corners(center: [f64; 3], corners: &mut [[f64; 3]]) {
    let helper = if center[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = normalize(cross(center, helper));
    let w = cross(center, u);
    corners.sort_by(|a, b| {
        let ang = |p: &[f64; 3]| dot(*p, w).atan2(dot(*p, u));
        ang(a).total_cmp(&ang(b))
    });
}

/// Icosahedron with a vertex rotated onto +Z. Returns unit vertices and triangle faces.
fn icosahedron() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let mut raw = Vec::with_capacity(12);
    for a in [1.0, -1.0] {
        for b in [PHI, -PHI] {
            raw.push([0.0, a, b]);
            raw.push([a, b, 0.0]);
            raw.push([b, 0.0, a]);
        }
    }
    // Rotate about X so that (0, 1, φ) lands on the north pole.
    let alpha = (1.0f64).atan2(PHI);
    let (s, c) = alpha.sin_cos();
    let verts: Vec<[f64; 3]> = raw
        .iter()
        .map(|&[x, y, z]| normalize([x, y * c - z * s, y * s + z * c]))
        .collect();
    let adj = icosa_adjacency(&verts);
    let mut tris = Vec::with_capacity(20);
    for i in 0..12 {
        for &j in &adj[i] {
            for &k in &adj[i] {
                if i < j && j < k && adj[j].contains(&k) {
                    tris.push([i, j, k]);
                }
            }
        }
    }
    assert_eq!(tris.len(), 20);
    (verts, tris)
}

fn icosa_adjacency(verts: &[[f64; 3]]) -> Vec<Vec<usize>> {
    // Neighbouring unit-icosahedron vertices have dot product 1/√5 ≈ 0.447; all others ≤ −0.447.
    (0..verts.len())
        .map(|i| {
            (0..verts.len())
                .filter(|&j| j != i && dot(verts[i], verts[j]) > 0.2)
                .collect()
        })
        .collect()
}

fn order_faces(faces: Vec<Face>) -> Polyhedron {
    let key = |f: &Face| {
        let [x, y, z] = f.center;
        let band = -(z * 1e6).round() as i64;
        let lon = (longitude(x, y).to_degrees() * 1e4).round() as i64;
        (band, lon)
    };
    let mut order: Vec<usize> = (0..faces.len()).collect();
    order.sort_by_key(|&i| key(&faces[i]));
    let mut new_index = vec![0usize; faces.len()];
    for (new, &old) in order.iter().enumerate() {
        new_index[old] = new;
    }
    let mut centers = Vec::with_capacity(faces.len());
    let mut sides = Vec::with_capacity(faces.len());
    let mut neighbors = Vec::with_capacity(faces.len());
    let mut corners = Vec::with_capacity(faces.len());
    let radius = faces
        .iter()
        .flat_map(|f| f.corners.iter())
        .map(|c| dot(*c, *c).sqrt())
        .fold(0.0, f64::max);
    for &old in &order {
        let f = &faces[old];
        centers.push(f.center);
        sides.push(f.sides);
        let mut n: Vec<u8> = f.neighbors.iter().map(|&o| new_index[o] as u8).collect();
        n.sort_unstable();
        neighbors.push(n);
        let mut c: Vec<[f64; 3]> = f.corners.iter().map(|p| p.map(|x| x / radius)).collect();
        sort_corners(f.center, &mut c);
        corners.push(c);
    }
    Polyhedron {
        centers,
        sides,
        neighbors,
        faces: corners,
    }
}

/// Longitude in radians, in `[0, 2π)`. The poles report 0.
fn longitude(x: f64, y: f64) -> f64 {
    if x.hypot(y) < 1e-9 {
        0.0
    } else {
        y.atan2(x).rem_euclid(std::f64::consts::TAU)
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn add3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let l = dot(v, v).sqrt();
    [v[0] / l, v[1] / l, v[2] / l]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_symmetric(p: &Polyhedron) {
        for (i, ns) in p.neighbors.iter().enumerate() {
            for &n in ns {
                assert!(p.are_adjacent(n as usize, i), "{i} -> {n} not symmetric");
            }
            assert_eq!(ns.len(), p.sides[i] as usize, "tile {i} neighbour count");
            assert_eq!(
                p.faces[i].len(),
                p.sides[i] as usize,
                "tile {i} corner count"
            );
            // Corners wind counter-clockwise around the outward normal.
            let f = &p.faces[i];
            let e1 = [f[1][0] - f[0][0], f[1][1] - f[0][1], f[1][2] - f[0][2]];
            let e2 = [f[2][0] - f[0][0], f[2][1] - f[0][1], f[2][2] - f[0][2]];
            assert!(dot(cross(e1, e2), p.centers[i]) > 0.0, "tile {i} winding");
        }
    }

    #[test]
    fn dodecahedron_layout() {
        let p = PlanetSize::Small.shape();
        assert_eq!(p.tile_count(), 12);
        check_symmetric(p);
        assert!(p.sides.iter().all(|&s| s == 5));
        assert!(p.centers[0][2] > 0.999);
        assert!(p.centers[11][2] < -0.999);
        assert_eq!(p.neighbors(0), &[1, 2, 3, 4, 5]);
        assert_eq!(p.neighbors(11), &[6, 7, 8, 9, 10]);
    }

    #[test]
    fn truncated_icosahedron_layout() {
        let p = PlanetSize::Large.shape();
        assert_eq!(p.tile_count(), 32);
        check_symmetric(p);
        assert_eq!(p.sides.iter().filter(|&&s| s == 5).count(), 12);
        assert_eq!(p.sides.iter().filter(|&&s| s == 6).count(), 20);
        assert_eq!(p.sides[0], 5);
        assert_eq!(p.sides[31], 5);
        // Pentagons never touch each other.
        for i in 0..32 {
            for &n in p.neighbors(i) {
                assert!(!(p.sides[i] == 5 && p.sides[n as usize] == 5));
            }
        }
        assert_eq!(p.neighbors(0), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn no_longitude_near_seam() {
        for size in [PlanetSize::Small, PlanetSize::Large] {
            let p = size.shape();
            for t in 1..p.tile_count() - 1 {
                let (_, lon) = p.lat_lon(t);
                assert!(
                    lon > 1.0 && lon < 359.0,
                    "tile {t} lon {lon} too close to seam"
                );
            }
        }
    }
}
