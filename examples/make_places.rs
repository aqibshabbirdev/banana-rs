//! Makes assets/places.bin, the place names Places uses to name where photos were taken,
//! offline, from GeoNames (https://www.geonames.org, CC BY 4.0):
//!
//!   curl -O https://download.geonames.org/export/dump/cities15000.zip && unzip cities15000.zip
//!   curl -O https://download.geonames.org/export/dump/countryInfo.txt
//!   cargo run --example make_places -- cities15000.txt countryInfo.txt
//!
//! Every place with more than 15,000 people (not parts of a city), and the countries. Format
//! (little-endian): b"BPLACES2", country count u16, per country: name length u8, name; place
//! count u32, per place: latitude f32, longitude f32, population u32, country u16, name length
//! u8, name.

use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (cities, countries) = (&args[1], &args[2]);

    // countryInfo.txt: ISO code, ..., name in the fifth column; comment lines start with '#'.
    let mut names = Vec::new();
    let mut index: HashMap<String, u16> = HashMap::new();
    for line in std::fs::read_to_string(countries).unwrap().lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() > 4 {
            index.insert(f[0].to_string(), names.len() as u16);
            names.push(f[4].to_string());
        }
    }

    // cities15000.txt: id, name, ascii name, alternates, lat, lon, ..., country code (9th).
    let mut places = Vec::new();
    for line in std::fs::read_to_string(cities).unwrap().lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let (Ok(lat), Ok(lon), Some(&country)) = (f[4].parse::<f32>(), f[5].parse::<f32>(), index.get(f[8])) else { continue };
        // Districts of a city (PPLX, "Paris 16 Passy") aren't what people call where they were.
        if f[7] == "PPLX" {
            continue;
        }
        // The name as written there, unless it's too long for a byte of length.
        let name = if f[1].len() < 256 { f[1] } else { f[2] };
        let population: u32 = f[14].parse().unwrap_or(0);
        places.push((lat, lon, population, country, name.to_string()));
    }
    // Parts of a city named after it ("Paris 16 Passy", "Marseille 01") within 20 km of it.
    let near = |a: &(f32, f32, u32, u16, String), b: &(f32, f32, u32, u16, String)| {
        (a.0 - b.0).abs() < 0.2 && ((a.1 - b.1) * a.0.to_radians().cos()).abs() < 0.2
    };
    let parts: std::collections::HashSet<usize> = (0..places.len())
        .filter(|&i| {
            let p = &places[i];
            places.iter().any(|big| big.2 > p.2 && p.4.starts_with(&format!("{} ", big.4)) && near(p, big))
        })
        .collect();
    let places: Vec<_> = places.into_iter().enumerate().filter(|(i, _)| !parts.contains(i)).map(|(_, p)| p).collect();

    let mut out = b"BPLACES2".to_vec();
    out.extend_from_slice(&(names.len() as u16).to_le_bytes());
    for n in &names {
        out.push(n.len() as u8);
        out.extend_from_slice(n.as_bytes());
    }
    out.extend_from_slice(&(places.len() as u32).to_le_bytes());
    for (lat, lon, population, country, name) in &places {
        out.extend_from_slice(&lat.to_le_bytes());
        out.extend_from_slice(&lon.to_le_bytes());
        out.extend_from_slice(&population.to_le_bytes());
        out.extend_from_slice(&country.to_le_bytes());
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
    }
    std::fs::write("assets/places.bin", &out).unwrap();
    println!("assets/places.bin: {} countries, {} places, {} KB", names.len(), places.len(), out.len() / 1024);
}
