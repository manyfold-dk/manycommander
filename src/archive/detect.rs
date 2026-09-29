#![forbid(unsafe_code)]
//! Formats and detection (P3 3.1).
//!
//! `Enter` opens a file as an archive by its name; `Alt+O` opens any file as one. The
//! listing thread opens the file through the M1 4.3 `O_PATH` sequence and then checks the
//! magic here: a zip's local or end header, a tar header (`ustar` at offset 257, or a valid
//! v7 checksum), a 7z signature, or a compressor's magic, after which the first 512
//! decompressed bytes must form a tar header too. A mismatch names the format the name
//! promised ("not a zip archive"); after `Alt+O` on another name it is "not a supported
//! archive".

/// An archive format of phase 3 (P3 3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarZst,
    TarXz,
    TarBz2,
    /// The stretch format of P3 D-6.
    SevenZ,
}

/// How a file is to be opened: by the format its name promises, or by its magic alone
/// (`Alt+O` on a name that promises none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Name(Format),
    Magic,
}

impl Want {
    /// The format a name promises, else detection by magic.
    pub fn of_name(name: &[u8]) -> Want {
        Format::by_name(name).map_or(Want::Magic, Want::Name)
    }
}

/// What a failed detection says when the name promised no format (P3 3.1).
pub const NOT_SUPPORTED: &str = "not a supported archive";

/// Name suffixes, ASCII case-insensitive (P3 3.1). Longer suffixes first, so `.tar.gz`
/// wins over a shorter match.
const SUFFIXES: &[(&[u8], Format)] = &[
    (b".tar.gz", Format::TarGz),
    (b".tar.zst", Format::TarZst),
    (b".tar.xz", Format::TarXz),
    (b".tar.bz2", Format::TarBz2),
    (b".tbz2", Format::TarBz2),
    (b".tzst", Format::TarZst),
    (b".tgz", Format::TarGz),
    (b".txz", Format::TarXz),
    (b".tbz", Format::TarBz2),
    (b".tar", Format::Tar),
    (b".zip", Format::Zip),
    (b".jar", Format::Zip),
    (b".apk", Format::Zip),
    (b".whl", Format::Zip),
    (b".7z", Format::SevenZ),
];

const GZIP: &[u8] = b"\x1f\x8b";
const ZSTD: &[u8] = b"\x28\xb5\x2f\xfd";
const XZ: &[u8] = b"\xfd7zXZ\x00";
const BZIP2: &[u8] = b"BZh";
/// The 7z signature (P3 3.1).
pub const SEVEN_Z: &[u8] = b"7z\xbc\xaf\x27\x1c";

impl Format {
    /// The format a file name promises (P3 3.1): `.pkg.tar.zst` is a `.tar.zst`.
    pub fn by_name(name: &[u8]) -> Option<Format> {
        SUFFIXES.iter().find_map(|(s, f)| {
            (name.len() > s.len() && name[name.len() - s.len()..].eq_ignore_ascii_case(s))
                .then_some(*f)
        })
    }

    /// The format's name in messages ("not a tar.zst archive").
    pub fn name(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::TarZst => "tar.zst",
            Format::TarXz => "tar.xz",
            Format::TarBz2 => "tar.bz2",
            Format::SevenZ => "7z",
        }
    }

    /// Members are read by locator: the quick view may preview them on cursor rest (V-5).
    /// A 7z member decodes its block from the block's start (P3 3.1).
    pub fn random_access(self) -> bool {
        matches!(self, Format::Zip | Format::Tar | Format::SevenZ)
    }

    /// A tar behind a stream decoder: listed only by decompressing all of it (P3 3.3).
    pub fn compressed(self) -> bool {
        matches!(
            self,
            Format::TarGz | Format::TarZst | Format::TarXz | Format::TarBz2
        )
    }

    /// The failure of a file that does not carry this format's magic.
    pub fn mismatch(self) -> String {
        format!("not a {} archive", self.name())
    }

    /// Whether `head` (the file's first bytes) starts with this format's magic. For a
    /// compressed tar the decompressed header is checked separately
    /// ([`is_tar_header`]).
    pub fn magic_matches(self, head: &[u8]) -> bool {
        match self {
            Format::Zip => head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06"),
            Format::Tar => head.len() >= 512 && is_tar_header(&head[..512]),
            Format::TarGz => head.starts_with(GZIP),
            Format::TarZst => head.starts_with(ZSTD),
            Format::TarXz => head.starts_with(XZ),
            Format::TarBz2 => head.len() > 3 && head.starts_with(BZIP2) && head[3].is_ascii_digit(),
            Format::SevenZ => head.starts_with(SEVEN_Z),
        }
    }
}

/// The format of a file by its first bytes (at least 512 when the file has them), for
/// `want`. A promised format must carry its own magic; `Want::Magic` tries every format.
pub fn detect(head: &[u8], want: Want) -> Result<Format, String> {
    match want {
        Want::Name(f) if f.magic_matches(head) => Ok(f),
        Want::Name(f) => Err(f.mismatch()),
        Want::Magic => [
            Format::Zip,
            Format::SevenZ,
            Format::TarGz,
            Format::TarZst,
            Format::TarXz,
            Format::TarBz2,
            Format::Tar,
        ]
        .into_iter()
        .find(|f| f.magic_matches(head))
        .ok_or_else(|| NOT_SUPPORTED.to_string()),
    }
}

/// Whether a 512-byte block is a tar header: `ustar` at offset 257 with a valid checksum,
/// or a valid v7 checksum over a block that is not all zeros. An all-zero block is the
/// end of an (empty) archive and counts as a header too.
pub fn is_tar_header(block: &[u8]) -> bool {
    if block.len() < 512 {
        return false;
    }
    if block[..512].iter().all(|&b| b == 0) {
        return true;
    }
    let Some(stored) = octal(&block[148..156]) else {
        return false;
    };
    let unsigned: u64 = block[..512]
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                b as u64
            }
        })
        .sum();
    // Some old writers summed signed bytes.
    let signed: i64 = block[..512]
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                b as i8 as i64
            }
        })
        .sum();
    stored == unsigned || stored as i64 == signed
}

/// A tar numeric field: octal digits, surrounded by spaces or NULs.
fn octal(field: &[u8]) -> Option<u64> {
    let t: Vec<u8> = field
        .iter()
        .copied()
        .skip_while(|&b| b == b' ')
        .take_while(|&b| b != 0 && b != b' ')
        .collect();
    if t.is_empty() || !t.iter().all(|b| (b'0'..=b'7').contains(b)) {
        return None;
    }
    u64::from_str_radix(std::str::from_utf8(&t).ok()?, 8).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_promise_formats_case_insensitively() {
        for (n, f) in [
            (&b"a.zip"[..], Some(Format::Zip)),
            (b"A.JAR", Some(Format::Zip)),
            (b"x.whl", Some(Format::Zip)),
            (b"x.apk", Some(Format::Zip)),
            (b"core-1-x86_64.pkg.tar.zst", Some(Format::TarZst)),
            (b"a.TZST", Some(Format::TarZst)),
            (b"a.tar", Some(Format::Tar)),
            (b"a.tar.gz", Some(Format::TarGz)),
            (b"a.tgz", Some(Format::TarGz)),
            (b"a.tar.xz", Some(Format::TarXz)),
            (b"a.txz", Some(Format::TarXz)),
            (b"a.tar.bz2", Some(Format::TarBz2)),
            (b"a.tbz2", Some(Format::TarBz2)),
            (b"a.tbz", Some(Format::TarBz2)),
            (b"a.7z", Some(Format::SevenZ)),
            (b"A.7Z", Some(Format::SevenZ)),
            (b".7z", None),
            (b"a.gz", None),
            (b"a.epub", None),
            (b".zip", None),
            (b"zip", None),
        ] {
            assert_eq!(Format::by_name(n), f, "{}", String::from_utf8_lossy(n));
        }
        assert_eq!(Want::of_name(b"x.docx"), Want::Magic);
    }

    fn tar_block() -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[..5].copy_from_slice(b"hello");
        b[100..108].copy_from_slice(b"0000644\0");
        b[124..136].copy_from_slice(b"00000000000\0");
        b[156] = b'0';
        let sum: u64 = b
            .iter()
            .enumerate()
            .map(|(i, &x)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    x as u64
                }
            })
            .sum();
        let s = format!("{sum:06o}\0 ");
        b[148..156].copy_from_slice(s.as_bytes());
        b
    }

    #[test]
    fn magic_decides_and_a_promise_must_hold() {
        let tar = tar_block();
        assert!(is_tar_header(&tar));
        let mut bad = tar.clone();
        bad[0] = b'j';
        assert!(!is_tar_header(&bad), "the checksum no longer matches");
        assert!(is_tar_header(&[0u8; 512]), "an empty archive");
        assert_eq!(detect(&tar, Want::Magic), Ok(Format::Tar));
        assert_eq!(detect(b"PK\x03\x04rest", Want::Magic), Ok(Format::Zip));
        assert_eq!(
            detect(b"\x28\xb5\x2f\xfd...", Want::Magic),
            Ok(Format::TarZst)
        );
        assert_eq!(detect(b"\xfd7zXZ\x00..", Want::Magic), Ok(Format::TarXz));
        assert_eq!(detect(b"BZh9...", Want::Magic), Ok(Format::TarBz2));
        assert_eq!(detect(b"\x1f\x8b\x08", Want::Magic), Ok(Format::TarGz));
        assert_eq!(
            detect(b"7z\xbc\xaf\x27\x1c\x00\x04", Want::Magic),
            Ok(Format::SevenZ)
        );
        assert_eq!(
            detect(b"PK\x03\x04", Want::Name(Format::SevenZ)),
            Err("not a 7z archive".to_string())
        );
        assert_eq!(
            detect(b"plain text", Want::Magic),
            Err(NOT_SUPPORTED.to_string())
        );
        assert_eq!(
            detect(b"plain text", Want::Name(Format::Zip)),
            Err("not a zip archive".to_string())
        );
        assert_eq!(
            detect(b"PK\x03\x04", Want::Name(Format::TarZst)),
            Err("not a tar.zst archive".to_string())
        );
        assert_eq!(detect(&tar, Want::Name(Format::Tar)), Ok(Format::Tar));
    }
}
