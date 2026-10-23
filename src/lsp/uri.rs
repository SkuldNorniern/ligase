//! file paths to `file://` uris and back. windows drives come back upper
//! case, so `c%3A` from a server and `C:` from the explorer name one file.

use std::path::{Path, PathBuf};

/// bytes kept as they are in a uri path; everything else is `%XX`.
fn plain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':')
}

pub fn from_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        if plain(b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // a host part, if any, is skipped; local files have none
    let rest = &rest[rest.find('/')?..];
    let mut bytes = Vec::with_capacity(rest.len());
    let mut it = rest.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hi = char::from(it.next()?).to_digit(16)?;
            let lo = char::from(it.next()?).to_digit(16)?;
            bytes.push(u8::try_from(hi * 16 + lo).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let text = String::from_utf8(bytes).ok()?;
    // "/C:/x" is a windows path; drop the slash, upper the drive
    let chars: Vec<char> = text.chars().take(3).collect();
    if let ['/', drive, ':'] = chars.as_slice()
        && drive.is_ascii_alphabetic()
    {
        let rest = &text[3..];
        let path = format!("{}:{}", drive.to_ascii_uppercase(), rest);
        return Some(PathBuf::from(
            path.replace('/', std::path::MAIN_SEPARATOR_STR),
        ));
    }
    Some(PathBuf::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_round_trip() {
        let uri = from_path(Path::new(r"D:\Project\my app\가.rs"));
        assert_eq!(uri, "file:///D:/Project/my%20app/%EA%B0%80.rs");
        let back = to_path(&uri).unwrap();
        assert_eq!(
            back.to_string_lossy().replace('\\', "/"),
            "D:/Project/my app/가.rs"
        );
    }

    #[test]
    fn server_lower_drive_and_encoded_colon() {
        let path = to_path("file:///d%3A/Project/a.rs").unwrap();
        assert_eq!(path.to_string_lossy().replace('\\', "/"), "D:/Project/a.rs");
    }

    #[test]
    fn unix_paths_and_junk() {
        assert_eq!(from_path(Path::new("/home/x/a b")), "file:///home/x/a%20b");
        assert_eq!(
            to_path("file:///home/x/a%20b"),
            Some(PathBuf::from("/home/x/a b"))
        );
        assert_eq!(to_path("http://x/y"), None);
        assert_eq!(to_path("file:///bad%zz"), None);
        assert_eq!(to_path("file:///cut%4"), None);
    }
}
