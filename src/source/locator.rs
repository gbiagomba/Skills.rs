//! Parsing a source string into a transport and a selector.
//!
//! Ambiguity here is dangerous in both directions: treating a path as a URL
//! produces a confusing network error, and treating a URL as a path produces a
//! confusing "no such file". So the rules are explicit and ordered, and anything
//! that does not match is refused by name rather than guessed at.
//!
//! Order of resolution:
//!
//! 1. Windows drive paths (`C:\...`, `C:/...`) and UNC paths (`\\server\share`).
//! 2. `file://` URIs.
//! 3. `smb://` URIs.
//! 4. Explicit Git URL schemes (`git://`, `git+ssh://`, `ssh://`).
//! 5. SCP-style Git addresses (`user@host:path`).
//! 6. `http://` and `https://`, which are Git when the path ends in `.git` and a
//!    plain download otherwise.
//! 7. Anything else is a filesystem path.
//!
//! A bare `owner/repo` shorthand is deliberately **not** expanded to a GitHub
//! URL. It is indistinguishable from a relative directory path, and silently
//! turning a local path into a network fetch is exactly the sort of surprise this
//! tool should not produce.

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::pkg::identity::{self, SourceType};

/// A parsed source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locator {
    pub source_type: SourceType,
    /// The original string, with any credential removed.
    pub sanitized: String,
    /// Transport-specific target.
    pub target: Target,
    /// Package path inside the source, from a `#skill=` fragment or a URL path.
    pub selector: Option<String>,
    /// Requested Git ref, from a `#ref=` fragment or a `@ref` suffix.
    pub requested_ref: Option<String>,
}

/// Where the content actually is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A local path, mounted share, or UNC path.
    Path(PathBuf),
    /// A Git repository address, as Git itself should receive it.
    Git(String),
    /// An HTTP(S) URL.
    Http(url::Url),
    /// An `smb://` URL, decomposed for the SMB client.
    Smb {
        host: String,
        share: String,
        path: String,
        /// Username when the URL carried one. Never a password.
        username: Option<String>,
    },
}

impl Locator {
    /// True when acquiring this source needs the network.
    pub fn needs_network(&self) -> bool {
        self.source_type.needs_network()
    }

    /// A short description for a progress line or a disclosure.
    pub fn describe(&self) -> String {
        match &self.target {
            Target::Path(p) => format!("local path {}", p.display()),
            Target::Git(addr) => format!("git repository {addr}"),
            Target::Http(url) => format!("{} download {url}", url.scheme()),
            Target::Smb { host, share, .. } => format!("SMB share //{host}/{share}"),
        }
    }
}

/// Split a trailing `#key=value` fragment list off a source string.
///
/// Supported keys are `ref` and `skill`, matching the fragment convention other
/// skill tools already use, so a source string copied from elsewhere works here.
fn split_fragment(raw: &str) -> (&str, Option<String>, Option<String>) {
    let Some((base, fragment)) = raw.split_once('#') else {
        return (raw, None, None);
    };

    let mut git_ref = None;
    let mut selector = None;
    for part in fragment.split('&') {
        match part.split_once('=') {
            Some(("ref", value)) if !value.is_empty() => git_ref = Some(value.to_string()),
            Some(("skill", value)) if !value.is_empty() => selector = Some(value.to_string()),
            _ => {}
        }
    }
    (base, git_ref, selector)
}

/// True when `raw` looks like a Windows drive-qualified path.
fn is_windows_drive_path(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// True when `raw` is an SCP-style Git address such as `git@host:owner/repo.git`.
///
/// Distinguished from a Windows drive path by requiring an `@` before the colon
/// and a host containing a dot or being a known shorthand.
fn is_scp_style_git(raw: &str) -> bool {
    if is_windows_drive_path(raw) {
        return false;
    }
    let Some((userhost, path)) = raw.split_once(':') else {
        return false;
    };
    if path.is_empty() || userhost.is_empty() {
        return false;
    }
    // Must have user@host, and the path must not start with a slash (that would
    // be a URL-ish form we do not accept here).
    match userhost.split_once('@') {
        Some((user, host)) => !user.is_empty() && !host.is_empty() && !host.contains('/'),
        None => false,
    }
}

/// Parse a source string.
pub fn parse(raw: &str) -> Result<Locator> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::UnsupportedSource {
            locator: raw.to_string(),
            reason: "empty source".into(),
        });
    }

    let (base, fragment_ref, fragment_skill) = split_fragment(trimmed);
    let sanitized = identity::sanitize_locator(base);

    // 1. Windows drive and UNC paths, before anything tries to read `C:` as a scheme.
    if is_windows_drive_path(base) || base.starts_with(r"\\") {
        return Ok(Locator {
            source_type: SourceType::Filesystem,
            sanitized,
            target: Target::Path(PathBuf::from(base)),
            selector: fragment_skill,
            requested_ref: fragment_ref,
        });
    }

    // 2 to 6: anything with a scheme we recognise.
    if let Some((scheme, _)) = base.split_once("://") {
        return match scheme {
            "file" => parse_file_url(base, sanitized, fragment_skill, fragment_ref),
            "smb" => parse_smb_url(base, sanitized, fragment_skill, fragment_ref),
            "git" | "git+ssh" | "ssh" => Ok(Locator {
                source_type: SourceType::Git,
                sanitized,
                target: Target::Git(base.to_string()),
                selector: fragment_skill,
                requested_ref: fragment_ref,
            }),
            "http" | "https" => parse_web_url(base, sanitized, fragment_skill, fragment_ref),
            other => Err(Error::UnsupportedSource {
                locator: raw.to_string(),
                reason: format!(
                    "scheme {other:?} is not supported. Supported transports are a local path, \
                     file://, git/ssh, http(s)://, and smb://"
                ),
            }),
        };
    }

    // 5. SCP-style Git, checked after schemes so `ssh://` wins.
    if is_scp_style_git(base) {
        return Ok(Locator {
            source_type: SourceType::Git,
            sanitized,
            target: Target::Git(base.to_string()),
            selector: fragment_skill,
            requested_ref: fragment_ref,
        });
    }

    // A lone colon that is neither a drive, a scheme, nor SCP-style is almost
    // certainly a typo, and treating it as a filename would be unhelpful.
    if base.contains(':') {
        return Err(Error::UnsupportedSource {
            locator: raw.to_string(),
            reason: "contains ':' but is not a recognised URL, drive path, or git address".into(),
        });
    }

    // 7. A filesystem path.
    Ok(Locator {
        source_type: SourceType::Filesystem,
        sanitized,
        target: Target::Path(PathBuf::from(base)),
        selector: fragment_skill,
        requested_ref: fragment_ref,
    })
}

fn parse_file_url(
    base: &str,
    sanitized: String,
    selector: Option<String>,
    requested_ref: Option<String>,
) -> Result<Locator> {
    let url = url::Url::parse(base).map_err(|err| Error::UnsupportedSource {
        locator: base.to_string(),
        reason: format!("not a valid file:// URI: {err}"),
    })?;
    let path = url.to_file_path().map_err(|()| Error::UnsupportedSource {
        locator: base.to_string(),
        reason: "file:// URI does not name a path on this platform".into(),
    })?;
    Ok(Locator {
        source_type: SourceType::Filesystem,
        sanitized,
        target: Target::Path(path),
        selector,
        requested_ref,
    })
}

fn parse_smb_url(
    base: &str,
    sanitized: String,
    selector: Option<String>,
    requested_ref: Option<String>,
) -> Result<Locator> {
    let url = url::Url::parse(base).map_err(|err| Error::UnsupportedSource {
        locator: base.to_string(),
        reason: format!("not a valid smb:// URI: {err}"),
    })?;

    let host = url
        .host_str()
        .ok_or_else(|| Error::UnsupportedSource {
            locator: base.to_string(),
            reason: "smb:// URI has no server name".into(),
        })?
        .to_string();

    let mut segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(Error::UnsupportedSource {
            locator: base.to_string(),
            reason: "smb:// URI needs a share, as in smb://server/share/path".into(),
        });
    }
    let share = segments.remove(0).to_string();

    let username = match url.username() {
        "" => None,
        user => Some(percent_decode(user)),
    };

    Ok(Locator {
        source_type: SourceType::Smb,
        sanitized,
        target: Target::Smb {
            host,
            share,
            path: segments.join("/"),
            username,
        },
        selector,
        requested_ref,
    })
}

/// Minimal percent-decoding for a URL username.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_web_url(
    base: &str,
    sanitized: String,
    selector: Option<String>,
    requested_ref: Option<String>,
) -> Result<Locator> {
    let url = url::Url::parse(base).map_err(|err| Error::UnsupportedSource {
        locator: base.to_string(),
        reason: format!("not a valid URL: {err}"),
    })?;

    // A `.git` path is a repository, not a file to download.
    let is_git = url.path().trim_end_matches('/').ends_with(".git");

    Ok(Locator {
        source_type: if is_git {
            SourceType::Git
        } else {
            SourceType::Http
        },
        sanitized,
        target: if is_git {
            Target::Git(base.to_string())
        } else {
            Target::Http(url)
        },
        selector,
        requested_ref,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relative_and_absolute_paths() {
        for raw in ["./my-skill", "../skills", "/abs/path", "my-skill"] {
            let loc = parse(raw).unwrap();
            assert_eq!(loc.source_type, SourceType::Filesystem, "{raw}");
            assert_eq!(loc.target, Target::Path(PathBuf::from(raw)));
            assert!(!loc.needs_network());
        }
    }

    #[test]
    fn parses_a_file_uri() {
        let loc = parse("file:///absolute/path/my-skill/SKILL.md").unwrap();
        assert_eq!(loc.source_type, SourceType::Filesystem);
        assert_eq!(
            loc.target,
            Target::Path(PathBuf::from("/absolute/path/my-skill/SKILL.md"))
        );
    }

    #[test]
    fn parses_windows_and_unc_paths_as_paths_not_urls() {
        // `C:` must not be read as a URL scheme.
        let drive = parse(r"C:\Users\me\skills").unwrap();
        assert_eq!(drive.source_type, SourceType::Filesystem);
        assert_eq!(
            drive.target,
            Target::Path(PathBuf::from(r"C:\Users\me\skills"))
        );

        let unc = parse(r"\\server\share\skills").unwrap();
        assert_eq!(unc.source_type, SourceType::Filesystem);
        assert_eq!(
            unc.target,
            Target::Path(PathBuf::from(r"\\server\share\skills"))
        );
    }

    #[test]
    fn parses_git_urls_in_every_documented_form() {
        for raw in [
            "https://github.com/owner/repo.git",
            "git://example.com/repo.git",
            "ssh://git@example.com/owner/repo.git",
            "git+ssh://git@example.com/owner/repo.git",
            "git@github.com:owner/repo.git",
        ] {
            let loc = parse(raw).unwrap();
            assert_eq!(loc.source_type, SourceType::Git, "{raw}");
            assert!(loc.needs_network());
        }
    }

    #[test]
    fn an_https_url_without_dot_git_is_a_download() {
        let loc = parse("https://example.com/my-skill.tar.gz").unwrap();
        assert_eq!(loc.source_type, SourceType::Http);
        assert!(matches!(loc.target, Target::Http(_)));
    }

    #[test]
    fn parses_an_smb_url_into_its_parts() {
        let loc = parse("smb://server/share/path/to/skill").unwrap();
        assert_eq!(loc.source_type, SourceType::Smb);
        match loc.target {
            Target::Smb {
                host,
                share,
                path,
                username,
            } => {
                assert_eq!(host, "server");
                assert_eq!(share, "share");
                assert_eq!(path, "path/to/skill");
                assert_eq!(username, None);
            }
            other => panic!("expected an SMB target, got {other:?}"),
        }
    }

    #[test]
    fn an_smb_url_keeps_the_username_but_never_the_password() {
        let loc = parse("smb://alice:hunter2@server/share/skill").unwrap();
        match &loc.target {
            Target::Smb { username, .. } => assert_eq!(username.as_deref(), Some("alice")),
            other => panic!("expected an SMB target, got {other:?}"),
        }
        assert!(
            !loc.sanitized.contains("hunter2"),
            "a password must never survive into the stored locator: {}",
            loc.sanitized
        );
    }

    #[test]
    fn an_smb_url_needs_a_share() {
        let err = parse("smb://server").unwrap_err();
        assert!(err.to_string().contains("share"), "{err}");
    }

    #[test]
    fn credentials_are_stripped_from_the_stored_locator() {
        let loc = parse("https://user:token@example.com/s.tar.gz").unwrap();
        assert_eq!(loc.sanitized, "https://user@example.com/s.tar.gz");
        assert!(!loc.sanitized.contains("token"));
    }

    #[test]
    fn fragments_select_a_ref_and_a_skill() {
        let loc = parse("https://github.com/o/r.git#ref=main&skill=my-skill").unwrap();
        assert_eq!(loc.requested_ref.as_deref(), Some("main"));
        assert_eq!(loc.selector.as_deref(), Some("my-skill"));
        assert_eq!(
            loc.sanitized, "https://github.com/o/r.git",
            "the fragment must not be part of the stored locator"
        );
    }

    #[test]
    fn rejects_unsupported_schemes_by_name() {
        for raw in [
            "ftp://example.com/s.tar.gz",
            "s3://bucket/key",
            "rsync://h/m",
        ] {
            let err = parse(raw).unwrap_err();
            let text = err.to_string();
            assert!(matches!(err, Error::UnsupportedSource { .. }), "{raw}");
            assert!(
                text.contains("Supported transports"),
                "must say what is supported: {text}"
            );
        }
    }

    #[test]
    fn a_bare_owner_repo_stays_a_path() {
        // Expanding this to a GitHub URL would turn a local directory into a
        // network fetch without the operator asking.
        let loc = parse("owner/repo").unwrap();
        assert_eq!(loc.source_type, SourceType::Filesystem);
        assert!(!loc.needs_network());
    }

    #[test]
    fn an_ambiguous_colon_is_refused_rather_than_guessed() {
        let err = parse("not-a-url:thing").unwrap_err();
        assert!(matches!(err, Error::UnsupportedSource { .. }), "{err:?}");
    }

    #[test]
    fn rejects_an_empty_source() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }
}
