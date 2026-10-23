//! Naming where photos were taken, offline: the nearest town or city (more than 15,000 people)
//! from GeoNames (https://www.geonames.org, CC BY 4.0), built into the app as assets/places.bin
//! by examples/make_places.rs. Nothing is looked up online.

use std::collections::HashMap;
use std::sync::OnceLock;

static DATA: &[u8] = include_bytes!("../assets/places.bin");

/// Photos further than this from any town are left without a place.
const MAX_KM: f32 = 60.0;

struct Places {
    countries: Vec<&'static str>,
    /// Latitude, longitude, how far it reaches (km, from its population), country, name.
    all: Vec<(f32, f32, f32, u16, &'static str)>,
    /// Places by whole-degree cell, for finding the nearest quickly.
    cells: HashMap<(i16, i16), Vec<u32>>,
}

fn places() -> &'static Places {
    static PLACES: OnceLock<Places> = OnceLock::new();
    PLACES.get_or_init(|| parse(DATA).unwrap_or(Places { countries: Vec::new(), all: Vec::new(), cells: HashMap::new() }))
}

fn parse(mut b: &'static [u8]) -> Option<Places> {
    fn take<const N: usize>(b: &mut &'static [u8]) -> Option<[u8; N]> {
        let (head, rest) = b.split_first_chunk::<N>()?;
        *b = rest;
        Some(*head)
    }
    fn text(b: &mut &'static [u8]) -> Option<&'static str> {
        let [len] = take::<1>(b)?;
        let (s, rest) = b.split_at_checked(len as usize)?;
        *b = rest;
        std::str::from_utf8(s).ok()
    }
    if &take::<8>(&mut b)? != b"BPLACES2" {
        return None;
    }
    let countries = (0..u16::from_le_bytes(take(&mut b)?)).map(|_| text(&mut b)).collect::<Option<Vec<_>>>()?;
    let n = u32::from_le_bytes(take(&mut b)?);
    let mut all = Vec::with_capacity(n as usize);
    let mut cells: HashMap<(i16, i16), Vec<u32>> = HashMap::new();
    for i in 0..n {
        let (lat, lon) = (f32::from_le_bytes(take(&mut b)?), f32::from_le_bytes(take(&mut b)?));
        let population = u32::from_le_bytes(take(&mut b)?).max(15_000) as f32;
        let country = u16::from_le_bytes(take(&mut b)?);
        // A town of 15,000 is taken to reach 3 km out, a city of 2 million about 15 km.
        let reach = 3.0 * (population / 15_000.0).cbrt();
        all.push((lat, lon, reach, country, text(&mut b)?));
        cells.entry(cell(lat, lon)).or_default().push(i);
    }
    Some(Places { countries, all, cells })
}

fn cell(lat: f32, lon: f32) -> (i16, i16) {
    (lat.floor() as i16, lon.floor() as i16)
}

/// Great-circle distance in km.
fn km(a: (f32, f32), b: (f32, f32)) -> f32 {
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let (dla, dlo) = ((b.0 - a.0).to_radians(), (b.1 - a.1).to_radians());
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    12742.0 * h.sqrt().min(1.0).asin()
}

/// The town a point is in, as an id for `name`: the one it's nearest to for the town's size (so
/// a photo by the Eiffel Tower is in Paris, not in the smaller suburb a little closer), if any
/// is close enough.
pub fn nearest(lat: f32, lon: f32) -> Option<u32> {
    let p = places();
    let (cy, cx) = cell(lat, lon);
    // A degree of latitude is 111 km; one of longitude shrinks towards the poles.
    let dx = (1.0 / lat.to_radians().cos().max(0.05)).ceil().min(180.0) as i16;
    // (distance relative to the town's reach, town)
    let mut best: Option<(f32, u32)> = None;
    for y in cy - 1..=cy + 1 {
        for x in cx - dx..=cx + dx {
            // Longitude wraps around at ±180°.
            let x = (x + 180).rem_euclid(360) - 180;
            for &i in p.cells.get(&(y, x)).into_iter().flatten() {
                let (plat, plon, reach, ..) = p.all[i as usize];
                let d = km((lat, lon), (plat, plon));
                if d <= MAX_KM && best.is_none_or(|(bd, _)| d / reach < bd) {
                    best = Some((d / reach, i));
                }
            }
        }
    }
    best.map(|(_, i)| i)
}

/// "Lahore, Pakistan".
pub fn name(id: u32) -> String {
    let p = places();
    match p.all.get(id as usize) {
        Some(&(_, _, _, country, city)) => format!("{city}, {}", p.countries.get(country as usize).copied().unwrap_or("")),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_where_photos_were_taken() {
        let at = |lat, lon| nearest(lat, lon).map(name);
        assert_eq!(at(31.5204, 74.3587).as_deref(), Some("Lahore, Pakistan"));
        assert_eq!(at(48.8584, 2.2945).as_deref(), Some("Paris, France"));
        assert_eq!(at(40.7484, -73.9857).as_deref(), Some("New York City, United States"));
        assert_eq!(at(-33.8568, 151.2153).as_deref(), Some("Sydney, Australia"));
        // A town next to a big city is still itself.
        assert_eq!(at(40.7357, -74.1724).as_deref(), Some("Newark, United States"));
        assert_eq!(at(33.6007, 73.0679).as_deref(), Some("Rawalpindi, Pakistan"));
        assert_eq!(at(33.6844, 73.0479).as_deref(), Some("Islamabad, Pakistan"));
        // Across the ±180° line, and the middle of the Pacific (nothing near).
        assert!(at(-17.7134, 178.065).is_some_and(|n| n.ends_with("Fiji")));
        assert_eq!(at(0.0, -150.0), None);
    }
}
