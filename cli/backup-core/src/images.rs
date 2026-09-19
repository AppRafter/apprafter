// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Helper-image selection for backup/restore Jobs.
//!
//! PostgreSQL dumps require a `pg_dump` binary whose **major version** matches the
//! CNPG server. We derive the major from the Cluster CR's `spec.imageName` tag
//! (e.g. `ghcr.io/cloudnative-pg/postgresql:16.2` → major 16 → `postgres:16-alpine`).
//! Volume snapshots and Redis dumps use fixed, pinned images.

pub const DEFAULT_PG_IMAGE: &str = "postgres:16-alpine";
pub const VOLUME_IMAGE: &str = "busybox:1.36";
pub const REDIS_IMAGE: &str = "redis:7-alpine";
/// The `nats` CLI image the JetStream dump/restore helper pod runs.
///
/// A stream is dumped over the NATS wire, not off a volume — the server owns
/// its file layout and `nats stream backup` is the only supported reader — so
/// this image is part of the artifact format: the CLI that writes a snapshot
/// and the CLI that replays it have to agree. Pinned for the same reason the
/// server itself is (`component_nats.cue`): a floating tag moves that pair
/// under snapshots nobody re-reads until the day they are needed.
pub const JETSTREAM_IMAGE: &str = "natsio/nats-box:0.18.0";

/// Pick a `pg_dump` helper image whose major matches the CNPG server.
///
/// `server_image` is the value of `spec.imageName` from the CNPG Cluster CR
/// (e.g. `ghcr.io/cloudnative-pg/postgresql:16.2`). Returns `DEFAULT_PG_IMAGE`
/// when the tag is absent or the major cannot be parsed as a number.
pub fn pg_helper_image(server_image: Option<&str>) -> String {
    let major = server_image
        .and_then(|img| img.rsplit_once(':').map(|(_, tag)| tag))
        .and_then(|tag| tag.split('.').next())
        .and_then(|m| m.parse::<u32>().ok());
    match major {
        Some(m) => format!("postgres:{m}-alpine"),
        None => DEFAULT_PG_IMAGE.to_string(),
    }
}

pub fn volume_helper_image() -> &'static str {
    VOLUME_IMAGE
}

pub fn redis_helper_image() -> &'static str {
    REDIS_IMAGE
}

pub fn jetstream_helper_image() -> &'static str {
    JETSTREAM_IMAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_image_matches_server_major() {
        assert_eq!(
            pg_helper_image(Some("ghcr.io/cloudnative-pg/postgresql:16.2")),
            "postgres:16-alpine"
        );
        assert_eq!(pg_helper_image(Some("postgres:15")), "postgres:15-alpine");
    }

    #[test]
    fn pg_image_falls_back_when_unparseable() {
        assert_eq!(pg_helper_image(None), DEFAULT_PG_IMAGE);
        assert_eq!(pg_helper_image(Some("weird")), DEFAULT_PG_IMAGE);
    }

    #[test]
    fn volume_and_redis_images_are_pinned() {
        assert_eq!(volume_helper_image(), VOLUME_IMAGE);
        assert_eq!(redis_helper_image(), REDIS_IMAGE);
    }

    #[test]
    fn the_jetstream_helper_image_is_pinned_to_an_explicit_tag() {
        // `nats stream backup` writes a format the matching `nats stream
        // restore` reads. A floating tag would move that pair under snapshots
        // nobody re-reads until the day they are needed — the same reasoning
        // `component_nats.cue` gives for pinning the server itself.
        assert_eq!(jetstream_helper_image(), JETSTREAM_IMAGE);
        assert!(JETSTREAM_IMAGE.contains("nats-box:"), "{JETSTREAM_IMAGE}");
        assert!(!JETSTREAM_IMAGE.ends_with(":latest"), "{JETSTREAM_IMAGE}");
    }
}
