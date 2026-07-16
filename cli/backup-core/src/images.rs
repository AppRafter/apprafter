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
}
