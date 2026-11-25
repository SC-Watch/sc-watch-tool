//! scw-vision's name classifier and burst vote against sc-watch's Python
//! (sc_detector.classify_name, consensus.py) on the cases made by
//! tests/fixtures/make_fixtures.py: placeholder names, generic words, random
//! strings, and synthetic bursts with truncations, confusable slips and
//! range slips. Everything is compared exactly, floats to the bit.

mod common;

use common::check;
use scw_vision::names::{classify_name, vote};
use serde_json::Value;

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

/// Deliberately not the Python: a ship serial with the reader's usual damage
/// is a serial, not a player (names::damaged_serial). Built from the
/// README's example serial, SP-9695-PW, in the shapes a Linux session's
/// misreads took.
#[test]
fn damaged_serials_are_serials() {
    for raw in [
        "SP-O695-PW", // a zero read as O: the commonest
        "SP-Q695-PW", // ... or as Q
        "SP-96O5-PW",
        "SP9695-PW", // the first hyphen lost
        "SP96O5-PW", // ... and a zero read as O
        "SPQ695-PW", // ... or the hyphen gone with the first digit read as a letter
        "SPO6O5-PW",
        "S0-9695-PW", // the prefix's O read as a zero
        "S0-96",      // ... and cut short
        "sp-o695-pw.",
    ] {
        let (kind, cleaned) = classify_name(raw);
        assert_eq!(kind.as_str(), "ship_serial", "{raw}");
        assert!(cleaned.is_some());
    }
    // the exact and truncated forms stay serials, as in the Python
    for raw in ["SP-9695-PW", "SP-969"] {
        assert_eq!(classify_name(raw).0.as_str(), "ship_serial", "{raw}");
    }
}

/// ...and nothing handle-shaped is caught by it.
#[test]
fn handles_are_not_damaged_serials() {
    for raw in [
        "AB-SOLO-XY",  // no real digits in the block
        "AB-SO1O-XY",  // one real digit is not enough
        "Mike1234-JR", // too long for a serial
        "AB1234-XYZ",  // a three-letter tail
        "Kilo-99",     // too short
        "ABC-DEFG-HI", // letters that are not misread digits
        "AB12H4-CD",   // H is not a misread digit
        "A1-2345-CD",  // a 1 in the prefix is not a misread O
        "EXMPLHANDLE",
        "EXMPL-SomeName",
    ] {
        assert_eq!(classify_name(raw).0.as_str(), "player", "{raw}");
    }
}

#[test]
fn classify() {
    check("classify_name", |c| {
        let p = &c["params"];
        let (kind, cleaned) = classify_name(s(&p["raw"]));
        let want = (s(&p["kind"]), p["cleaned"].as_str());
        (want != (kind.as_str(), cleaned.as_deref())).then(|| {
            format!(
                "{:?}: {want:?} vs {:?}",
                s(&p["raw"]),
                (kind.as_str(), cleaned)
            )
        })
    });
}

#[test]
fn distance_and_similarity() {
    check("name_distance", |c| {
        let p = &c["params"];
        let (a, b) = (s(&p["a"]), s(&p["b"]));
        let d = vote::distance(a, b);
        let sim = vote::similarity(a, b);
        let same = vote::same_contact(a, b);
        let want = (
            p["distance"].as_f64().unwrap(),
            p["similarity"].as_f64().unwrap(),
            p["same"].as_bool().unwrap(),
        );
        (want.0.to_bits() != d.to_bits() || want.1.to_bits() != sim.to_bits() || want.2 != same)
            .then(|| format!("{a:?}/{b:?}: {want:?} vs {:?}", (d, sim, same)))
    });
}

#[test]
fn burst_vote() {
    check("vote", |c| {
        let p = &c["params"];
        let readings: Vec<vote::Reading> = p["readings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| vote::Reading {
                name: s(&r[0]).to_string(),
                range_km: r[1].as_f64().unwrap(),
                cx: r[2].as_f64().unwrap(),
                cy: r[3].as_f64().unwrap(),
            })
            .collect();
        let clusters: Vec<Vec<String>> = vote::cluster(&readings)
            .iter()
            .map(|g| g.iter().map(|r| r.name.clone()).collect())
            .collect();
        let want_clusters: Vec<Vec<String>> =
            serde_json::from_value(p["clusters"].clone()).unwrap();
        if clusters != want_clusters {
            return Some(format!(
                "clusters: {} vs {}",
                want_clusters.len(),
                clusters.len()
            ));
        }
        let got = vote::resolve(&readings, p["min_votes"].as_u64().unwrap() as usize);
        let want = p["expect"].as_array().unwrap();
        if want.len() != got.len() {
            return Some(format!("{} contacts vs {}", want.len(), got.len()));
        }
        for (k, (w, g)) in want.iter().zip(&got).enumerate() {
            let variants: Vec<(String, usize)> = w["variants"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| (s(&v[0]).to_string(), v[1].as_u64().unwrap() as usize))
                .collect();
            let same = s(&w["name"]) == g.name
                && w["range_km"].as_f64().unwrap().to_bits() == g.range_km.to_bits()
                && w["votes"].as_u64().unwrap() as usize == g.votes
                && w["total"].as_u64().unwrap() as usize == g.total
                && variants == g.variants;
            if !same {
                return Some(format!("contact {k}: {w} vs {g:?}"));
            }
        }
        None
    });
}
