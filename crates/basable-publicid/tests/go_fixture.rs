//! The port must agree with the Go original byte for byte. `fixtures/go_ids.txt`
//! holds twenty UUIDs and the public ids `golang/lib/publicid.Encode` produced
//! for them under the `proj` and `org` prefixes (generated 2026-09-20 from the
//! monorepo at `a51e41e4`): the zero and all-ones UUIDs, leading-zero-byte
//! cases, high-bit cases, and assorted random values.

use basable_publicid::{Registry, Uuid, decode_with_prefix, encode};

const FIXTURES: &str = include_str!("fixtures/go_ids.txt");

fn fixtures() -> Vec<(Uuid, String, String)> {
    FIXTURES
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut f = l.split_whitespace();
            let id = Uuid::parse_str(f.next().unwrap()).unwrap();
            (
                id,
                f.next().unwrap().to_owned(),
                f.next().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn twenty_go_ids_encode_identically() {
    let all = fixtures();
    assert_eq!(all.len(), 20);
    for (id, proj, org) in &all {
        assert_eq!(&encode("proj", *id), proj, "{id}");
        assert_eq!(&encode("org", *id), org, "{id}");
    }
}

#[test]
fn twenty_go_ids_decode_back_through_the_registry_and_bare() {
    let reg = Registry::builder()
        .register("tenant", "proj")
        .register("OrganisationConfiguration", "org")
        .build()
        .unwrap();
    for (id, proj, org) in fixtures() {
        assert_eq!(reg.decode_typed("tenant", &proj), Ok(id), "{proj}");
        assert_eq!(
            reg.decode_typed("OrganisationConfiguration", &org),
            Ok(id),
            "{org}"
        );
        assert_eq!(decode_with_prefix(&proj), Ok(("proj", id)));
        assert!(!reg.is_type("tenant", &org));
    }
}
