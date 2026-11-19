//! Predicates for Git's object-integrity policy, not transport URL parsing.
//!
//! Contract references: Git v2.56.0 [`check_submodule_url`][policy], [`url_normalize`][normalize], and
//! [`url_decode`][decode]. This predicate avoids building their intermediate normalized/decoded strings.
//!
//! [policy]: https://github.com/git/git/blob/v2.56.0/submodule-config.c
//! [normalize]: https://github.com/git/git/blob/v2.56.0/urlmatch.c
//! [decode]: https://github.com/git/git/blob/v2.56.0/url.c

use bstr::BStr;
use gix_error::{ErrorExt, Result};

/// A rejected `.gitmodules` URL under native Git's default fsck policy.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Error {
    /// The value begins with a command-line option marker.
    CommandLineOption,
    /// A relative URL could replace the remote's authority when resolved.
    RelativeRootEscape,
    /// A curl-backed location fails Git's URL-normalization preconditions.
    InvalidCurlUrl,
    /// The checked, once-decoded location contains a newline.
    Newline,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CommandLineOption => "submodule URL begins with a command-line option",
            Self::RelativeRootEscape => "relative submodule URL escapes its remote root",
            Self::InvalidCurlUrl => "submodule URL fails curl URL normalization",
            Self::Newline => "submodule URL contains a decoded newline",
        })
    }
}

impl std::error::Error for Error {}

/// Validate one **config-decoded** `.gitmodules` URL with Git 2.56's default fsck security policy.
///
/// This is not [`crate::parse()`], normalization, fetch authorization, shell quoting, or a guarantee that a
/// transport can use the value. In particular, native fsck accepts tabs, carriage returns, local paths, SCP-like
/// locations, SSH option-looking components, and arbitrary helpers (including `ext`). Callers must enforce their
/// transport/command-execution policy separately. No commands, configuration includes, or network requests run here.
///
/// Like Git's config callback, only the prefix before the first literal NUL is considered. A leading `-` is rejected.
/// Relative `./`/`../` and `git://` locations are checked for once-decoded newlines; leading parent components must not
/// expose `:` or `/`. Both slash spellings are recognized in the leading dot components, even on Unix.
/// Lowercase `http`, `https`, `ftp`, and `ftps` URL/helper forms additionally require Git's
/// URL-normalization preconditions, including host, port, escapes, and non-escaping dot segments. Newlines in removed
/// path segments do not survive normalization. Other schemes are intentionally not validated. This policy has no
/// input-length limit and does not return or allocate a normalized URL.
///
/// `check` is called before validation, at success, and throughout **every** input-sized scan. Between checks there
/// are at most 4096 constant-work scan steps, each inspecting at most eight input bytes (at most 32 KiB). Callback
/// errors propagate unchanged, retaining cancellation/deadline causes; policy errors downcast to
/// [`crate::FsckSubmoduleUrlError`]. This is a cooperative work bound, not a wall-clock or scheduling guarantee.
///
/// ```
/// # use bstr::ByteSlice;
/// gix_url::validate_fsck_submodule_url(b"https://example.invalid/a\tb".as_bstr(), &|| Ok(()))?;
/// assert!(gix_url::validate_fsck_submodule_url(b"-evil".as_bstr(), &|| Ok(())).is_err());
/// # Ok::<(), gix_error::Error>(())
/// ```
pub fn validate_fsck_submodule_url(input: &BStr, check: &dyn Fn() -> Result) -> Result {
    check()?;
    let mut poll = Poll { check, remaining: 4096 };
    let input = input.as_ref();
    let end = find(input, |b| b == 0, &mut poll)?.unwrap_or(input.len());
    let input = &input[..end];
    if input.first() == Some(&b'-') {
        return Err(Error::CommandLineOption.raise());
    }
    if dot_slash(input) || dot_dot_slash(input) || input.starts_with(b"git://") {
        // url_decode() leaves everything before the first colon undecoded, even in a relative path.
        let decode_from = find(input, |b| b == b':', &mut poll)?.unwrap_or(0);
        for (index, byte) in input.iter().enumerate() {
            poll.step()?;
            if *byte == b'\n'
                || (index >= decode_from && input[index..].starts_with(b"%0a"))
                || (index >= decode_from && input[index..].starts_with(b"%0A"))
            {
                return Err(Error::Newline.raise());
            }
        }
        let mut rest = input;
        let mut parent = false;
        loop {
            poll.step()?;
            if dot_dot_slash(rest) {
                parent = true;
                rest = &rest[3..];
            } else if dot_slash(rest) {
                rest = &rest[2..];
            } else {
                break;
            }
        }
        if parent && matches!(rest.first(), Some(b':' | b'/')) {
            return Err(Error::RelativeRootEscape.raise());
        }
    } else {
        let helper = [b"http::".as_slice(), b"https::", b"ftp::", b"ftps::"]
            .iter()
            .find_map(|prefix| input.strip_prefix(*prefix));
        let curl = helper.or_else(|| {
            [b"http://".as_slice(), b"https://", b"ftp://", b"ftps://"]
                .iter()
                .any(|prefix| input.starts_with(prefix))
                .then_some(input)
        });
        if let Some(curl) = curl {
            curl_url(curl, &mut poll)?;
        }
    }
    check()
}

struct Poll<'a> {
    check: &'a dyn Fn() -> Result,
    remaining: usize,
}

fn dot_slash(input: &[u8]) -> bool {
    input.first() == Some(&b'.') && matches!(input.get(1), Some(b'/' | b'\\'))
}

fn dot_dot_slash(input: &[u8]) -> bool {
    input.starts_with(b"..") && matches!(input.get(2), Some(b'/' | b'\\'))
}

impl Poll<'_> {
    fn step(&mut self) -> Result {
        if self.remaining == 0 {
            (self.check)()?;
            self.remaining = 4096;
        }
        self.remaining -= 1;
        Ok(())
    }
}

fn find(input: &[u8], predicate: impl Fn(u8) -> bool, poll: &mut Poll<'_>) -> Result<Option<usize>> {
    for (index, byte) in input.iter().enumerate() {
        poll.step()?;
        if predicate(*byte) {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn curl_url(input: &[u8], poll: &mut Poll<'_>) -> Result {
    let scheme_end = find(input, |b| !b.is_ascii_alphanumeric() && !b"+.-".contains(&b), poll)?.unwrap_or(input.len());
    if !input.first().is_some_and(u8::is_ascii_alphabetic) || !input[scheme_end..].starts_with(b"://") {
        return Err(Error::InvalidCurlUrl.raise());
    }
    let file = input[..scheme_end].eq_ignore_ascii_case(b"file");
    let rest = &input[scheme_end + 3..];
    let authority_end = find(rest, |b| b"/?#".contains(&b), poll)?.unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host_start = match find(authority, |b| b == b'@', poll)? {
        Some(at) => {
            escapes(&authority[..at], poll)?;
            at + 1
        }
        None => 0,
    };
    let host_port = &authority[host_start..];
    let missing_host = host_port.is_empty() || host_port.first() == Some(&b':');
    if missing_host && !file {
        return Err(Error::InvalidCurlUrl.raise());
    }
    // Git uses the final ':' unless a later ']' closes an IPv6-like host. It does not validate IPv6 syntax.
    let mut port = None;
    for (index, byte) in host_port.iter().enumerate() {
        poll.step()?;
        match byte {
            b':' => port = Some(index),
            b']' => port = None,
            _ => {}
        }
    }
    let host_end = port.unwrap_or(host_port.len());
    for byte in &host_port[..host_end] {
        poll.step()?;
        if !byte.is_ascii_alphanumeric() && !b".-_[:]".contains(byte) {
            return Err(Error::InvalidCurlUrl.raise());
        }
    }
    if let Some(colon) = port {
        let bytes = &host_port[colon + 1..];
        if missing_host && !bytes.is_empty() {
            return Err(Error::InvalidCurlUrl.raise());
        }
        let mut number = 0u32;
        for byte in bytes {
            poll.step()?;
            if !byte.is_ascii_digit() {
                return Err(Error::InvalidCurlUrl.raise());
            }
            number = number * 10 + u32::from(byte - b'0');
            if number > 65535 {
                return Err(Error::InvalidCurlUrl.raise());
            }
        }
        if !bytes.is_empty() && number == 0 {
            return Err(Error::InvalidCurlUrl.raise());
        }
    }
    let path_query = &rest[authority_end..];
    let path_end = find(path_query, |b| b == b'?' || b == b'#', poll)?.unwrap_or(path_query.len());
    let path = path_query[..path_end]
        .strip_prefix(b"/")
        .unwrap_or(&path_query[..path_end]);
    let mut start = 0;
    let mut depth = 0;
    let mut newline_depth = None;
    loop {
        let end = start + find(&path[start..], |b| b == b'/', poll)?.unwrap_or(path.len() - start);
        let segment = &path[start..end];
        // Validate every escape, even in segments that normalization removes. Keep only the earliest surviving
        // newline's depth, so '..' can remove it without a path-sized stack or normalized-string allocation.
        let (dots, newline) = segment_info(segment, poll)?;
        match dots {
            Some(1) => {}
            Some(2) => {
                if depth == 0 {
                    return Err(Error::InvalidCurlUrl.raise());
                }
                depth -= 1;
                if newline_depth.is_some_and(|d| d > depth) {
                    newline_depth = None;
                }
            }
            _ => {
                if newline && newline_depth.is_none() {
                    newline_depth = Some(depth + 1);
                }
                if end < path.len() {
                    depth += 1;
                }
            }
        }
        if end == path.len() {
            break;
        }
        start = end + 1;
    }
    if newline_depth.is_some() {
        return Err(Error::Newline.raise());
    }
    escapes(&path_query[path_end..], poll)
}

fn escapes(input: &[u8], poll: &mut Poll<'_>) -> Result {
    let (_, newline) = segment_info(input, poll)?;
    if newline {
        return Err(Error::Newline.raise());
    }
    Ok(())
}

fn segment_info(input: &[u8], poll: &mut Poll<'_>) -> Result<(Option<usize>, bool)> {
    let mut index = 0;
    let mut dots = Some(0);
    let mut newline = false;
    while index < input.len() {
        poll.step()?;
        let mut byte = input[index];
        index += 1;
        if byte == b'%' {
            let hex = input
                .get(index..index.saturating_add(2))
                .ok_or_else(|| Error::InvalidCurlUrl.raise())?;
            let hi = char::from(hex[0])
                .to_digit(16)
                .ok_or_else(|| Error::InvalidCurlUrl.raise())?;
            let lo = char::from(hex[1])
                .to_digit(16)
                .ok_or_else(|| Error::InvalidCurlUrl.raise())?;
            byte = (hi * 16 + lo) as u8;
            index += 2;
        }
        newline |= byte == b'\n';
        dots = match (dots, byte) {
            (Some(count @ 0..=1), b'.') => Some(count + 1),
            _ => None,
        };
    }
    Ok((dots, newline))
}
