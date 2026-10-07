//! Stable package identity and safe install names.
//!
//! Two problems are solved here, and keeping them apart matters:
//!
//! * A **[`PackageId`]** is how `skill` knows that two things are the same
//!   package. It is derived from the sanitised source locator plus the selector
//!   inside that source, never from the display name. Two repositories that both
//!   ship a `pr-review` skill are therefore distinct packages, which is what
//!   stops one silently replacing the other.
//! * An **[`InstallName`]** is a directory name. It comes from untrusted input,
//!   so it is validated against path-component rules, cross-platform case
//!   collisions, and the names the agents themselves reserve.

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::safepath;

/// Which transport a package came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// A local path, `file://` URI, UNC path, or mounted share.
    Filesystem,
    /// A Git repository over HTTPS, SSH, or an SCP-style address.
    Git,
    /// A single file or archive fetched over HTTP(S).
    Http,
    /// An `smb://` share fetched with the native SMB client.
    Smb,
    /// Restored from a `skill export` bundle.
    Bundle,
    /// Adopted from an existing installation we did not create.
    Adopted,
}

impl SourceType {
    /// Stable slug, used in identity derivation and in `--json`.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Git => "git",
            Self::Http => "http",
            Self::Smb => "smb",
            Self::Bundle => "bundle",
            Self::Adopted => "adopted",
        }
    }

    /// True when refreshing from this source needs the network.
    pub const fn needs_network(self) -> bool {
        matches!(self, Self::Git | Self::Http | Self::Smb)
    }
}

impl std::fmt::Display for SourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

/// A stable identifier for a package, independent of what it is called.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct PackageId(String);

impl PackageId {
    /// Derive an id from the sanitised locator and the selector within it.
    ///
    /// `selector` is the package's path inside the source: a repository
    /// subdirectory, an archive member prefix, or empty when the source *is* the
    /// package. Including it is what lets one repository hold many skills without
    /// their identities colliding.
    pub fn derive(source_type: SourceType, sanitized_locator: &str, selector: &str) -> Self {
        let mut hasher = Sha256::new();
        // Length-prefixed so that ("ab", "c") and ("a", "bc") cannot collide.
        for part in [source_type.slug(), sanitized_locator, selector] {
            hasher.update(part.len().to_le_bytes());
            hasher.update(part.as_bytes());
        }
        let digest = hex::encode(hasher.finalize());
        Self(format!("sk_{}", &digest[..24]))
    }

    /// Reconstruct an id read back from the state store.
    pub fn from_stored(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Names an agent reserves for itself, which a personal skill must never take.
///
/// `synced` and the `anthropic-skills` family come from the Claude Code
/// documentation: a folder called `synced` is where claude.ai-synced skills live,
/// and a skill named `anthropic-skills` or prefixed `anthropic-skills:` is not
/// loaded at all. Installing under one of these would produce a skill that either
/// never loads or collides with account sync.
const AGENT_RESERVED: [&str; 3] = ["synced", "anthropic-skills", "trash"];

/// A validated directory name for a deployed or stored package.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct InstallName(String);

impl InstallName {
    /// Validate an untrusted name for use as a single directory component.
    pub fn parse(raw: &str) -> Result<Self> {
        let refuse = |reason: &str| Error::UnsafeName {
            name: raw.to_string(),
            reason: reason.to_string(),
        };

        if raw.is_empty() {
            return Err(refuse("empty name"));
        }
        if raw.chars().count() > 64 {
            return Err(refuse("longer than 64 characters"));
        }
        if raw.contains('\0') || raw.chars().any(char::is_control) {
            return Err(refuse("contains a NUL or control character"));
        }
        if raw.contains('/') || raw.contains('\\') {
            return Err(refuse("contains a path separator"));
        }
        if raw.contains(':') {
            return Err(refuse(
                "contains ':' (drive letter or alternate data stream separator)",
            ));
        }
        if raw == "." || raw == ".." {
            return Err(refuse("is a relative path component"));
        }
        if raw.starts_with('.') {
            return Err(refuse(
                "starts with '.', which agents treat as a hidden or internal directory",
            ));
        }
        if raw.ends_with('.') || raw.ends_with(' ') || raw.starts_with(' ') {
            return Err(refuse("starts or ends with a dot or space"));
        }
        if safepath::is_windows_reserved(raw) {
            return Err(refuse("is a reserved Windows device name"));
        }

        let folded = raw.to_lowercase();
        if AGENT_RESERVED.contains(&folded.as_str()) {
            return Err(refuse(
                "is reserved by an agent (Claude Code uses `synced` and `trash` internally)",
            ));
        }
        if folded.starts_with("anthropic-skills") {
            return Err(refuse(
                "is reserved: Claude Code does not load skills named or prefixed `anthropic-skills`",
            ));
        }

        Ok(Self(raw.to_string()))
    }

    /// Derive a safe install name from an arbitrary string, or fail saying why.
    ///
    /// Used when a source supplies a directory name that is close to valid.
    /// Deliberately conservative: it lowercases and replaces runs of unsupported
    /// characters with a single hyphen, but it never invents a name out of
    /// nothing, because an operator should notice when their skill gets renamed.
    pub fn sanitize(raw: &str) -> Result<Self> {
        let mut out = String::with_capacity(raw.len());
        let mut last_was_sep = false;
        for ch in raw.chars() {
            let lowered = ch.to_ascii_lowercase();
            if lowered.is_ascii_alphanumeric() || lowered == '.' || lowered == '_' {
                out.push(lowered);
                last_was_sep = false;
            } else if !last_was_sep {
                out.push('-');
                last_was_sep = true;
            }
        }
        let trimmed = out.trim_matches(['-', '.', '_']).to_string();
        if trimmed.is_empty() {
            return Err(Error::UnsafeName {
                name: raw.to_string(),
                reason: "no usable characters remain after sanitising".into(),
            });
        }
        Self::parse(&trimmed)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Case-folded form, used to detect collisions on case-insensitive filesystems.
    pub fn folded(&self) -> String {
        self.0.to_lowercase()
    }
}

impl std::fmt::Display for InstallName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Remove anything credential-shaped from a locator before it is persisted.
///
/// Called on every locator on the way into the state store, an export manifest,
/// or a log line. A password or token in a URL is dropped entirely rather than
/// masked, so there is nothing to leak even if the field is later printed. The
/// username is kept, because it is part of how the source is addressed and is
/// needed to reproduce the fetch.
pub fn sanitize_locator(raw: &str) -> String {
    // Only URL-shaped locators can carry userinfo.
    if let Ok(mut url) = url::Url::parse(raw) {
        if url.password().is_some() {
            let _ = url.set_password(None);
        }
        // Strip query and fragment: tokens are commonly passed there.
        if url.query().is_some() {
            url.set_query(None);
        }
        if url.fragment().is_some() {
            url.set_fragment(None);
        }
        return url.to_string();
    }

    // SCP-style Git address, `user@host:path`. There is no password slot in that
    // form, so it is already safe, but normalise whitespace.
    raw.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_distinguishes_same_name_different_origin() {
        let a = PackageId::derive(
            SourceType::Git,
            "https://github.com/one/repo.git",
            "pr-review",
        );
        let b = PackageId::derive(
            SourceType::Git,
            "https://github.com/two/repo.git",
            "pr-review",
        );
        assert_ne!(a, b, "same skill name from different repos must differ");
    }

    #[test]
    fn identity_distinguishes_packages_within_one_source() {
        let locator = "https://github.com/one/repo.git";
        let a = PackageId::derive(SourceType::Git, locator, "skills/alpha");
        let b = PackageId::derive(SourceType::Git, locator, "skills/beta");
        assert_ne!(a, b);
    }

    #[test]
    fn identity_is_stable_and_length_prefixed() {
        let a = PackageId::derive(SourceType::Git, "ab", "c");
        let b = PackageId::derive(SourceType::Git, "a", "bc");
        assert_ne!(
            a, b,
            "length prefixing must prevent concatenation collisions"
        );

        let again = PackageId::derive(SourceType::Git, "ab", "c");
        assert_eq!(a, again, "derivation must be deterministic");
        assert!(a.as_str().starts_with("sk_"));
    }

    #[test]
    fn accepts_ordinary_install_names() {
        for ok in ["my-skill", "pr-review", "skill2", "a_b.c"] {
            assert!(InstallName::parse(ok).is_ok(), "should accept {ok:?}");
        }
    }

    #[test]
    fn rejects_unsafe_install_names() {
        for bad in [
            "", "..", ".", "a/b", r"a\b", "a:b", ".hidden", "name.", "name ", " name", "NUL",
            "com1", "a\0b",
        ] {
            assert!(InstallName::parse(bad).is_err(), "should reject {bad:?}");
        }
        let long = "a".repeat(65);
        assert!(InstallName::parse(&long).is_err());
    }

    #[test]
    fn rejects_names_the_agents_reserve() {
        // Documented Claude Code behaviour: these either never load or collide
        // with account sync.
        for bad in [
            "synced",
            "Synced",
            "SYNCED",
            "anthropic-skills",
            "anthropic-skills-extra",
            "trash",
        ] {
            let err = InstallName::parse(bad).unwrap_err();
            assert!(
                matches!(err, Error::UnsafeName { .. }),
                "should reserve {bad:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn sanitize_produces_a_usable_name_or_fails_loudly() {
        assert_eq!(
            InstallName::sanitize("My Skill!").unwrap().as_str(),
            "my-skill"
        );
        assert_eq!(
            InstallName::sanitize("Convex Best Practices")
                .unwrap()
                .as_str(),
            "convex-best-practices"
        );
        // Collapses runs rather than emitting `---`.
        assert_eq!(InstallName::sanitize("a///b").unwrap().as_str(), "a-b");
        // Nothing usable: fail rather than invent a placeholder.
        assert!(InstallName::sanitize("///").is_err());
        assert!(InstallName::sanitize("").is_err());
        // Sanitising into a reserved name must still be refused.
        assert!(InstallName::sanitize("Synced").is_err());
    }

    #[test]
    fn folded_names_reveal_case_collisions() {
        let a = InstallName::parse("My-Skill").unwrap();
        let b = InstallName::parse("my-skill").unwrap();
        assert_ne!(a, b);
        assert_eq!(a.folded(), b.folded());
    }

    #[test]
    fn locator_sanitisation_drops_credentials() {
        let cases = [
            (
                "https://user:secret@example.com/s.tar.gz",
                "https://user@example.com/s.tar.gz",
            ),
            (
                "https://example.com/s.tar.gz?token=abc123",
                "https://example.com/s.tar.gz",
            ),
            (
                "https://x:y@example.com/a?t=1#frag",
                "https://x@example.com/a",
            ),
        ];
        for (raw, expected) in cases {
            let clean = sanitize_locator(raw);
            assert_eq!(clean, expected, "sanitising {raw}");
            assert!(!clean.contains("secret"));
            assert!(!clean.contains("abc123"));
        }
    }

    #[test]
    fn locator_sanitisation_keeps_scp_style_git_addresses() {
        let raw = "git@github.com:owner/repo.git";
        assert_eq!(sanitize_locator(raw), raw);
    }

    #[test]
    fn source_types_declare_their_network_need() {
        assert!(SourceType::Git.needs_network());
        assert!(SourceType::Http.needs_network());
        assert!(SourceType::Smb.needs_network());
        assert!(!SourceType::Filesystem.needs_network());
        assert!(!SourceType::Bundle.needs_network());
    }
}
