//! spa.rs's pods against SPA's own builder: `SCW_SPA_ORACLE` names the
//! compiled tests/spa_oracle.c (CI builds it on Linux against the system's
//! SPA headers). Skipped without it.

use scw_capture::spa;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn pods_match_spas_own_builder() {
    let Ok(oracle) = std::env::var("SCW_SPA_ORACLE") else {
        println!("::warning title=spa pods::skipped - SCW_SPA_ORACLE not set");
        return;
    };
    let out = std::process::Command::new(&oracle).output().unwrap();
    assert!(out.status.success(), "the oracle failed");
    let text = String::from_utf8(out.stdout).unwrap();
    let mut got = std::collections::HashMap::new();
    for line in text.lines() {
        let (name, bytes) = line.split_once(' ').unwrap();
        got.insert(name.to_string(), bytes.to_string());
    }
    assert_eq!(hex(&spa::enum_format()), got["enum_format"], "EnumFormat");
    assert_eq!(hex(&spa::buffers()), got["buffers"], "Buffers");
    assert_eq!(
        spa::parse_format(&unhex(&got["format"])),
        Some(spa::VideoFormat {
            format: 12,
            width: 5120,
            height: 1440
        }),
        "a Format from spa_format_video_raw_build"
    );
    println!(
        "::notice title=spa pods::ran - EnumFormat and Buffers byte-identical to SPA's builder, its Format parsed"
    );
}
