//! The one container image both sandboxes run in.
//!
//! The per-agent **task sandbox** (`duduclaw-gateway::task_sandbox`) and the
//! **script sandbox** (`duduclaw-container`, used by the PTC script runner and
//! the `secaudit` PoC step) both default to the platform's own published
//! image for the running version, overridable by the same
//! `config.toml [container.sandbox] image` key — one image to pull, one key to
//! document. The image is never pulled automatically; a missing image makes
//! the sandbox unavailable with a message naming `docker pull <image>`.
//!
//! The L5a computer-use container has its own image (Chromium + Xvfb, built
//! from `container/Dockerfile.computer-use`), named by
//! [`computer_use_image`] with the same version rule and the same
//! never-pulled policy; its override key is `config.toml [computer_use] image`.
//!
//! The version is supplied by the caller (`env!("CARGO_PKG_VERSION")` of the
//! calling crate — every workspace crate shares the workspace version), so
//! this crate never has to guess which binary it is linked into.

/// Repository the release workflow publishes to
/// (`.github/workflows/docker-image.yml`, on git tags `v*`).
pub const PLATFORM_IMAGE_REPOSITORY: &str = "ghcr.io/zhixuli0406/duduclaw";

/// `ghcr.io/zhixuli0406/duduclaw:v<version>`. Published tags carry the `v`
/// prefix of the git tag (the unprefixed tag does not exist); a `version`
/// that already starts with `v` is not prefixed twice. Never `:latest` — the
/// sandbox must run the image that matches this binary.
pub fn platform_image(version: &str) -> String {
    versioned(PLATFORM_IMAGE_REPOSITORY, version)
}

/// Repository the computer-use image is published to
/// (`.github/workflows/computer-use-image.yml`, on the same git tags `v*`).
/// Built from `container/Dockerfile.computer-use`.
pub const COMPUTER_USE_IMAGE_REPOSITORY: &str = "ghcr.io/zhixuli0406/duduclaw-computer-use";

/// `ghcr.io/zhixuli0406/duduclaw-computer-use:v<version>`: the default image
/// of an L5a computer-use session. Same tag rule as [`platform_image`]; never
/// `:latest`, never pulled automatically.
pub fn computer_use_image(version: &str) -> String {
    versioned(COMPUTER_USE_IMAGE_REPOSITORY, version)
}

fn versioned(repository: &str, version: &str) -> String {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    format!("{repository}:v{version}")
}

/// An image reference: non-empty, at most 256 bytes, no whitespace, no NUL,
/// no comma, and never something a container CLI could read as an option.
pub fn valid_image(image: &str) -> bool {
    !image.is_empty()
        && image.len() <= 256
        && !image.starts_with('-')
        && !image.bytes().any(|c| c.is_ascii_whitespace() || c == 0 || c == b',')
}

/// The operator-facing remedy for a missing image. Shared wording so the
/// task sandbox, the script sandbox and `doctor` all say the same thing.
pub fn image_missing_message(image: &str) -> String {
    format!("sandbox image {image} is not present locally (it is never pulled automatically); run `docker pull {image}`")
}

/// The image's own unprivileged user (`duduclaw`, uid 1000).
pub const IMAGE_DEFAULT_USER: &str = "1000:1000";

/// The `uid:gid` a script-sandbox container runs as: the host's effective
/// uid/gid, so the read-only bind-mounted script scratch directory (created
/// by this process, mode 0700) is readable inside the container; the image's
/// own unprivileged user when the host process is root or not on Unix (the
/// scratch directory and script are then handed to that user). Nothing else
/// is mounted and there is no socket. Never root.
pub fn script_sandbox_user() -> String {
    #[cfg(unix)]
    {
        // SAFETY: geteuid/getegid have no preconditions and cannot fail.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        if uid != 0 && gid != 0 {
            return format!("{uid}:{gid}");
        }
    }
    IMAGE_DEFAULT_USER.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_image_carries_the_v_prefix_exactly_once() {
        assert_eq!(platform_image("1.66.1"), "ghcr.io/zhixuli0406/duduclaw:v1.66.1");
        assert_eq!(platform_image("v1.66.1"), "ghcr.io/zhixuli0406/duduclaw:v1.66.1");
        assert_eq!(platform_image(" 1.67.0 "), "ghcr.io/zhixuli0406/duduclaw:v1.67.0");
        assert!(!platform_image("1.66.1").ends_with(":latest"));
    }

    #[test]
    fn computer_use_image_is_versioned_like_the_platform_image() {
        assert_eq!(
            computer_use_image("1.66.1"),
            "ghcr.io/zhixuli0406/duduclaw-computer-use:v1.66.1"
        );
        assert_eq!(
            computer_use_image("v1.66.1"),
            "ghcr.io/zhixuli0406/duduclaw-computer-use:v1.66.1"
        );
        assert!(!computer_use_image("1.66.1").ends_with(":latest"));
        assert!(valid_image(&computer_use_image("1.66.1")));
    }

    #[test]
    fn valid_image_refuses_option_like_and_whitespace_values() {
        assert!(valid_image("ghcr.io/zhixuli0406/duduclaw:v1.66.1"));
        assert!(valid_image("sha256:be45dfabcca9ecb7c783fd06b08a98bc33ab37255684cf14bed46327f8d98776"));
        for bad in ["", "-v", "a b", "a,b", "a\0b", &"x".repeat(257)] {
            assert!(!valid_image(bad), "{bad:?}");
        }
    }

    #[test]
    fn missing_message_names_docker_pull() {
        let m = image_missing_message("ghcr.io/x/y:v1");
        assert!(m.contains("docker pull ghcr.io/x/y:v1"), "{m}");
    }

    #[test]
    fn script_sandbox_user_is_never_root() {
        let user = script_sandbox_user();
        let (uid, gid) = user.split_once(':').expect("uid:gid");
        assert_ne!(uid, "0");
        assert_ne!(gid, "0");
    }
}
