//! Geo utilities: Nominatim geocoding, BRouter GPX route planning, coordinate parsing.

use log::warn;
use serde::Deserialize;

const USER_AGENT: &str = "BonjourDijon/0.1 (household errand planner; contact: bonjourdijon@localhost)";

// ═══════════════════════════════════════════════════════════════════════
//  Coordinate parsing
// ═══════════════════════════════════════════════════════════════════════

/// Try to parse a "lat,lon" string (e.g. "47.3220,5.0415").
/// Returns `Some((lat, lon))` if it looks like coordinates.
pub fn parse_coordinates(s: &str) -> Option<(f64, f64)> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        return None;
    }
    let lat: f64 = parts[0].trim().parse().ok()?;
    let lon: f64 = parts[1].trim().parse().ok()?;
    // Sanity check ranges
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    Some((lat, lon))
}

/// Try to extract coordinates from a bracketed suffix like
/// `"Ikea Dijon [47.340671, 5.0644321]"`.
/// Returns `Some((clean_name, lat, lon))` if the pattern matches.
pub fn parse_bracketed_coordinates(s: &str) -> Option<(String, f64, f64)> {
    let s = s.trim();
    let open = s.rfind('[')?;
    let close = s.rfind(']')?;
    if close <= open {
        return None;
    }
    let inner = &s[open + 1..close];
    let (lat, lon) = parse_coordinates(inner)?;
    let name = s[..open].trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some((name, lat, lon))
}

// ═══════════════════════════════════════════════════════════════════════
//  Nominatim geocoding (blocking, for use in sync tool dispatch)
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
struct NominatimResult {
    lat: String,
    lon: String,
    display_name: String,
}

/// Geocode a place name via the Nominatim (OpenStreetMap) API.
/// Returns `(lat, lon, display_name)`.
///
/// Uses `reqwest::blocking` so it can be called from synchronous code.
/// Respects Nominatim usage policy: includes a User-Agent header.
/// Callers should avoid calling this more than once per second.
pub fn geocode_blocking(query: &str) -> Result<(f64, f64, String), String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let url = format!(
        "https://nominatim.openstreetmap.org/search?format=json&q={}&limit=1",
        urlencod(query)
    );

    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("Nominatim request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Nominatim returned status {}", resp.status()));
    }

    let results: Vec<NominatimResult> = resp
        .json()
        .map_err(|e| format!("Failed to parse Nominatim response: {e}"))?;

    let r = results
        .into_iter()
        .next()
        .ok_or_else(|| format!("No results found for '{query}'"))?;

    let lat: f64 = r.lat.parse().map_err(|_| "Invalid lat from Nominatim".to_string())?;
    let lon: f64 = r.lon.parse().map_err(|_| "Invalid lon from Nominatim".to_string())?;

    Ok((lat, lon, r.display_name))
}

/// Resolve a `where_to_buy` string to coordinates.
/// Tries, in order:
///   1. Pure coordinate string: "47.32,5.04"
///   2. Bracketed suffix:       "Ikea Dijon [47.34, 5.06]"
///   3. Nominatim geocoding:    "Ikea Dijon"
/// Returns `None` if everything fails (logs a warning).
pub fn resolve_location(where_to_buy: &str) -> Option<(f64, f64)> {
    // 1. Try direct coordinate parsing
    if let Some(coords) = parse_coordinates(where_to_buy) {
        return Some(coords);
    }
    // 2. Try extracting coordinates from bracket suffix
    if let Some((_name, lat, lon)) = parse_bracketed_coordinates(where_to_buy) {
        return Some((lat, lon));
    }
    // 3. Try Nominatim geocoding (strip any bracket junk first)
    let query = where_to_buy
        .rfind('[')
        .map(|i| where_to_buy[..i].trim())
        .unwrap_or(where_to_buy.trim());
    match geocode_blocking(query) {
        Ok((lat, lon, _)) => Some((lat, lon)),
        Err(e) => {
            warn!("Geocoding failed for '{}': {}", query, e);
            None
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  BRouter GPX route planning (blocking)
// ═══════════════════════════════════════════════════════════════════════

/// Plan a route between waypoints using the BRouter public API.
/// Returns raw GPX XML string.
///
/// `profile` is a BRouter profile name:
///   - "trekking" — walking
///   - "safety" — safe cycling
///   - "fastbike" — fast cycling
///
/// Waypoints are `(lat, lon)` tuples. At least 2 are required.
pub fn plan_route_gpx_blocking(
    waypoints: &[(f64, f64)],
    profile: &str,
) -> Result<String, String> {
    if waypoints.len() < 2 {
        return Err("At least 2 waypoints are required for route planning.".to_string());
    }

    // BRouter wants "lon,lat" pairs separated by "|"
    let lonlats: Vec<String> = waypoints
        .iter()
        .map(|(lat, lon)| format!("{lon},{lat}"))
        .collect();
    let lonlats_param = lonlats.join("|");

    let url = format!(
        "https://brouter.de/brouter?lonlats={}&profile={}&alternativeidx=0&format=gpx",
        lonlats_param, profile
    );

    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("BRouter request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("BRouter returned status {status}: {body}"));
    }

    resp.text()
        .map_err(|e| format!("Failed to read BRouter response: {e}"))
}

/// Build a BRouter-web URL that opens the route in the browser on an interactive map.
///
/// The URL format is:
///   `https://brouter.de/brouter-web/#map=Z/LAT/LON/cyclosm&lonlats=LON,LAT|…&profile=PROFILE`
///
/// `waypoints` should be the full route (home → stores → home).
pub fn brouter_web_url(waypoints: &[(f64, f64)], profile: &str) -> String {
    if waypoints.len() < 2 {
        return String::from("https://brouter.de/brouter-web/");
    }

    // BRouter-web wants "lon,lat" pairs separated by "|"
    let lonlats: Vec<String> = waypoints
        .iter()
        .map(|(lat, lon)| format!("{lon},{lat}"))
        .collect();
    let lonlats_param = lonlats.join("|");

    // Centre the map on the first waypoint
    let center_lat = waypoints[0].0;
    let center_lon = waypoints[0].1;

    format!(
        "https://brouter.de/brouter-web/#map=14/{center_lat:.4}/{center_lon:.4}/cyclosm&lonlats={lonlats_param}&profile={profile}"
    )
}

// ═══════════════════════════════════════════════════════════════════════
//  Nearest-neighbour greedy route ordering
// ═══════════════════════════════════════════════════════════════════════

/// Order waypoints using a simple nearest-neighbour heuristic starting from `start`.
/// Returns the ordered waypoints (not including `start` or `end` — those are
/// prepended/appended by the caller).
pub fn nearest_neighbour_order(start: (f64, f64), points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if points.is_empty() {
        return vec![];
    }
    let mut remaining: Vec<(f64, f64)> = points.to_vec();
    let mut ordered = Vec::with_capacity(remaining.len());
    let mut current = start;

    while !remaining.is_empty() {
        let (idx, _) = remaining
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = haversine_distance(current, **a);
                let db = haversine_distance(current, **b);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();
        current = remaining.remove(idx);
        ordered.push(current);
    }

    ordered
}

/// Haversine distance in meters between two (lat, lon) points.
fn haversine_distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    let r = 6_371_000.0; // Earth radius in metres
    let d_lat = (b.0 - a.0).to_radians();
    let d_lon = (b.1 - a.1).to_radians();
    let lat1 = a.0.to_radians();
    let lat2 = b.0.to_radians();

    let a_val = (d_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a_val.sqrt().asin();
    r * c
}

// ═══════════════════════════════════════════════════════════════════════
//  Google Maps navigation URL
// ═══════════════════════════════════════════════════════════════════════

/// Build a Google Maps Directions URL from ordered waypoints.
///
/// The URL uses the Directions API link format:
///   `https://www.google.com/maps/dir/?api=1&origin=…&destination=…&waypoints=…|…&travelmode=…`
///
/// `waypoints` should be the full route including start and end (home → stores → home).
/// `mode` is a Google Maps travel mode: "walking", "driving", "bicycling", or "transit".
///
/// Google Maps allows up to ~25 waypoints in the URL. If there are more,
/// they are silently truncated to keep the URL functional.
pub fn google_maps_directions_url(waypoints: &[(f64, f64)], mode: &str) -> String {
    if waypoints.len() < 2 {
        return String::from("https://www.google.com/maps");
    }

    let origin = &waypoints[0];
    let destination = &waypoints[waypoints.len() - 1];

    let mut url = format!(
        "https://www.google.com/maps/dir/?api=1&origin={},{}&destination={},{}&travelmode={}",
        origin.0, origin.1, destination.0, destination.1, mode
    );

    // Intermediate waypoints (everything between first and last)
    if waypoints.len() > 2 {
        let intermediates: Vec<String> = waypoints[1..waypoints.len() - 1]
            .iter()
            .take(23) // Google Maps limit is ~25 total including origin/destination
            .map(|(lat, lon)| format!("{lat},{lon}"))
            .collect();
        url.push_str(&format!("&waypoints={}", intermediates.join("|")));
    }

    url
}

// ═══════════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════════

/// Minimal URL-encoding for query strings (encodes spaces and common special chars).
fn urlencod(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('+', "%2B")
        .replace('#', "%23")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_coordinates() {
        assert_eq!(parse_coordinates("47.3220,5.0415"), Some((47.322, 5.0415)));
        assert_eq!(parse_coordinates(" 47.3220 , 5.0415 "), Some((47.322, 5.0415)));
        assert_eq!(parse_coordinates("Carrefour"), None);
        assert_eq!(parse_coordinates("100.0,200.0"), None); // out of range
        assert_eq!(parse_coordinates("-33.8688,151.2093"), Some((-33.8688, 151.2093))); // Sydney
    }

    #[test]
    fn test_parse_bracketed_coordinates() {
        let r = parse_bracketed_coordinates("Ikea Dijon [47.340671, 5.0644321]");
        assert!(r.is_some());
        let (name, lat, lon) = r.unwrap();
        assert_eq!(name, "Ikea Dijon");
        assert!((lat - 47.340671).abs() < 1e-6);
        assert!((lon - 5.0644321).abs() < 1e-6);

        // No brackets → None
        assert!(parse_bracketed_coordinates("Carrefour").is_none());
        // Bare brackets with no name → None
        assert!(parse_bracketed_coordinates("[47.3,5.0]").is_none());
        // Bad coords inside brackets → None
        assert!(parse_bracketed_coordinates("Store [abc, def]").is_none());
    }

    #[test]
    fn test_google_maps_url() {
        let waypoints = vec![
            (47.322, 5.041),   // home
            (47.340, 5.064),   // store 1
            (47.310, 5.030),   // store 2
            (47.322, 5.041),   // home (return)
        ];
        let url = google_maps_directions_url(&waypoints, "walking");
        assert!(url.starts_with("https://www.google.com/maps/dir/?api=1"));
        assert!(url.contains("origin=47.322,5.041"));
        assert!(url.contains("destination=47.322,5.041"));
        assert!(url.contains("travelmode=walking"));
        assert!(url.contains("waypoints=47.34,5.064|47.31,5.03"));
    }

    #[test]
    fn test_google_maps_url_two_points() {
        let waypoints = vec![(47.322, 5.041), (47.340, 5.064)];
        let url = google_maps_directions_url(&waypoints, "driving");
        assert!(url.contains("origin=47.322,5.041"));
        assert!(url.contains("destination=47.34,5.064"));
        assert!(!url.contains("waypoints")); // no intermediates
    }

    #[test]
    fn test_google_maps_url_empty() {
        let url = google_maps_directions_url(&[], "walking");
        assert_eq!(url, "https://www.google.com/maps");
    }

    #[test]
    fn test_brouter_web_url() {
        let waypoints = vec![
            (47.322, 5.041),
            (47.340, 5.064),
            (47.322, 5.041),
        ];
        let url = brouter_web_url(&waypoints, "trekking");
        assert!(url.starts_with("https://brouter.de/brouter-web/#map=14/"));
        assert!(url.contains("lonlats=5.041,47.322|5.064,47.34|5.041,47.322"));
        assert!(url.contains("profile=trekking"));
        assert!(url.contains("cyclosm"));
    }

    #[test]
    fn test_brouter_web_url_empty() {
        let url = brouter_web_url(&[], "trekking");
        assert_eq!(url, "https://brouter.de/brouter-web/");
    }

    #[test]
    fn test_nearest_neighbour() {
        let start = (47.32, 5.04);
        let points = vec![(47.33, 5.05), (47.31, 5.03), (47.35, 5.06)];
        let ordered = nearest_neighbour_order(start, &points);
        // Should visit nearest first
        assert_eq!(ordered.len(), 3);
        // First should be (47.31, 5.03) or (47.33, 5.05) — both close
        // Just verify all points are present
        for p in &points {
            assert!(ordered.contains(p));
        }
    }
}
