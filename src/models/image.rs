use serde::Deserialize;

/// DTO for parsing `docker images --format json` / `podman images --format json` output.
/// Immediately flattened into `HashSet<String>` of image names by
/// [`Docker::downloaded_images`](crate::backends::docker::Docker) — never stored or
/// used as an application model.
#[derive(Debug, Clone, Deserialize, Hash, Eq, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Image {
    pub id: String,
    pub names: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_image_with_no_names() {
        let json = r#"{"Id":"sha256:def456","Names":null}"#;
        let image: Image = serde_json::from_str(json).unwrap();

        assert_eq!(image.id, "sha256:def456");
        assert_eq!(image.names, None);
    }

    #[test]
    fn deserialize_image_with_empty_names() {
        let json = r#"{"Id":"sha256:ghi789","Names":[]}"#;
        let image: Image = serde_json::from_str(json).unwrap();

        assert_eq!(image.id, "sha256:ghi789");
        assert_eq!(image.names, Some(vec![]));
    }

    #[test]
    fn deserialize_missing_names_field() {
        let json = r#"{"Id":"sha256:abc123"}"#;
        let image: Image = serde_json::from_str(json).unwrap();

        assert_eq!(image.id, "sha256:abc123");
        assert_eq!(image.names, None);
    }

    #[test]
    fn deserialize_podman_format() {
        // Podman `images --format json` uses the same PascalCase keys as Docker
        // but adds extra fields (`Created`, `Size`) that we must ignore.
        let json = r#"{"Id":"sha256:abc123","Names":["quay.io/podman/hello:latest"],"Created":"3 days ago","Size":"585 kB"}"#;
        let image: Image = serde_json::from_str(json).unwrap();

        assert_eq!(image.id, "sha256:abc123");
        assert_eq!(
            image.names,
            Some(vec!["quay.io/podman/hello:latest".to_string()])
        );
    }
}
