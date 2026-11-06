//! Reading inside zip, tar and gzipped tar archives: `src.zip` lists the
//! members, `src.zip:lib/a.rs` reads one, `src.zip:lib/` lists a directory.
//!
//! Members are read-only: their text comes numbered like a file's, but with
//! no snapshot tag, since nothing can edit bytes inside an archive.
//! Decompression is capped, per member and for a whole gzip stream, so a
//! small archive cannot unpack into an unbounded amount of memory or time,
//! and the long names and extended headers of tar archives are refused when
//! they are longer than any real name, before they are read into memory.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use flate2::read::GzDecoder;

use super::container::{self, Target};
use super::lines;
use super::media::{self, Media};
use super::selector::Selector;
use crate::{ToolError, ToolOutput};

/// Largest member unpacked to be read, in bytes.
const MAX_MEMBER_BYTES: u64 = 32 << 20;

/// Most bytes a gzip stream is unpacked to while listing or searching it.
const MAX_STREAM_BYTES: u64 = 1 << 30;

/// Most members listed; an archive with more is listed up to here.
const MAX_MEMBERS: usize = 100_000;

/// Longest member path listed or read, in bytes.
const MAX_PATH_BYTES: usize = 4096;

/// Largest extended header of a tar archive read, in bytes.
const MAX_EXTENDED_HEADER_BYTES: u64 = 64 << 10;

/// Size of a tar block, the unit headers and data are stored in.
const BLOCK: usize = 512;

/// Bytes of a member inspected to tell text from binary data.
const SNIFF_BYTES: usize = 8192;

/// An archive format `read` can look inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    /// A zip archive, including formats built on it such as `.jar` and `.docx`.
    Zip,
    /// An uncompressed tar archive.
    Tar,
    /// A tar archive compressed with gzip, `.tar.gz` or `.tgz`.
    TarGz,
}

impl Format {
    /// The format's short name for headers.
    fn name(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
        }
    }
}

/// Recognizes an archive from the first bytes of a file, `head`, and its
/// lower-cased file name, `name`, which tells gzipped tar files from other
/// gzip files and finds old tar files that carry no magic bytes.
pub(super) fn sniff(head: &[u8], name: &str) -> Option<Format> {
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        Some(Format::Zip)
    } else if head.starts_with(&[0x1f, 0x8b]) {
        (name.ends_with(".tar.gz") || name.ends_with(".tgz")).then_some(Format::TarGz)
    } else if head.get(257..262) == Some(b"ustar") || name.ends_with(".tar") {
        Some(Format::Tar)
    } else {
        None
    }
}

/// What a member is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A link or special file, which has no contents to read.
    Other,
}

/// One member of an archive.
#[derive(Debug, Clone)]
struct Member {
    /// The member's path as shown and looked up: no leading `./` or `/`, and
    /// no trailing `/`.
    path: String,
    /// The member's name as the archive stores it, to fetch it by.
    stored: String,
    /// The unpacked size in bytes, as the archive declares it.
    size: u64,
    /// What the member is.
    kind: Kind,
}

/// Reads what `target` asks for inside the archive of format `format`: the
/// member list, a directory of it, or one member.
///
/// # Errors
///
/// Returns [`ToolError`] when the archive cannot be read, unpacks past the
/// caps, or has no member by the name asked for.
pub(super) fn read(
    target: &Target,
    format: Format,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ToolOutput, ToolError> {
    let label = &target.label;
    let fail =
        |error: String| ToolError::Failed(format!("could not read the archive {label}: {error}"));
    let (members, complete) = list(&target.path, format).map_err(fail)?;
    let whole = normalize(&target.inner);
    let (name, selector) =
        if whole.is_empty() || find(&members, whole).is_some() || is_dir(&members, whole) {
            (
                whole,
                container::window(Selector::default(), offset, limit)?,
            )
        } else if let Some(selector) = Selector::parse(&target.inner) {
            ("", container::window(selector, offset, limit)?)
        } else {
            let (name, selector) = container::split_window(&target.inner, offset, limit)?;
            (normalize(name), selector)
        };
    if let Some(member) = find(&members, name) {
        return member_output(target, format, member, &selector);
    }
    if name.is_empty() || is_dir(&members, name) {
        return listing(target, format, &members, complete, name, &selector);
    }
    Err(ToolError::Failed(format!(
        "{label} has no member {name}; read {label} to list its members"
    )))
}

/// Strips a leading `./` or `/` and a trailing `/` from a member path.
fn normalize(path: &str) -> &str {
    let path = path.trim_start_matches("./").trim_start_matches('/');
    path.strip_suffix('/').unwrap_or(path)
}

/// Finds the member at `path` that has contents or is a link.
fn find<'a>(members: &'a [Member], path: &str) -> Option<&'a Member> {
    members
        .iter()
        .find(|member| member.path == path && member.kind != Kind::Dir)
}

/// Whether `path` is a directory of the archive, listed or implied by the
/// members under it.
fn is_dir(members: &[Member], path: &str) -> bool {
    members.iter().any(|member| {
        (member.kind == Kind::Dir && member.path == path)
            || member
                .path
                .strip_prefix(path)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Lists the members of the archive at `path`, of format `format`, up to
/// [`MAX_MEMBERS`], and says whether that is all of them.
///
/// # Errors
///
/// Returns a description of the problem when the archive cannot be read or a
/// gzip stream unpacks past [`MAX_STREAM_BYTES`].
fn list(path: &Path, format: Format) -> Result<(Vec<Member>, bool), String> {
    match format {
        Format::Zip => list_zip(path),
        Format::Tar | Format::TarGz => {
            let mut members = Vec::new();
            let mut archive = tar::Archive::new(open_tar(path, format)?);
            for entry in archive.entries().map_err(|e| e.to_string())? {
                if members.len() == MAX_MEMBERS {
                    return Ok((members, false));
                }
                let entry = entry.map_err(|e| e.to_string())?;
                members.push(tar_member(&entry)?);
            }
            Ok((members, true))
        }
    }
}

/// Lists the members of the zip archive at `path` from its central
/// directory, unpacking nothing.
///
/// # Errors
///
/// Returns a description of the problem when the archive cannot be read.
fn list_zip(path: &Path) -> Result<(Vec<Member>, bool), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| e.to_string())?;
    let total = archive.len();
    let mut members = Vec::with_capacity(total.min(MAX_MEMBERS));
    for index in 0..total.min(MAX_MEMBERS) {
        let entry = archive.by_index_raw(index).map_err(|e| e.to_string())?;
        let stored = entry.name().map_err(|e| e.to_string())?.into_owned();
        members.push(Member {
            path: normalize(&stored).to_string(),
            kind: if entry.is_dir() {
                Kind::Dir
            } else {
                Kind::File
            },
            size: entry.size(),
            stored,
        });
    }
    Ok((members, total <= MAX_MEMBERS))
}

/// Describes the tar entry `entry` as a member.
///
/// # Errors
///
/// Returns a description of the problem when its header cannot be read.
fn tar_member<R: Read>(entry: &tar::Entry<'_, R>) -> Result<Member, String> {
    let stored = entry
        .path()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    if stored.len() > MAX_PATH_BYTES {
        return Err(format!(
            "a member's path is {} bytes long, longer than the {MAX_PATH_BYTES} read lists",
            stored.len()
        ));
    }
    let kind = match entry.header().entry_type() {
        tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => {
            Kind::File
        }
        tar::EntryType::Directory => Kind::Dir,
        _ => Kind::Other,
    };
    Ok(Member {
        path: normalize(&stored).to_string(),
        size: entry.header().size().map_err(|e| e.to_string())?,
        stored,
        kind,
    })
}

/// Opens the tar archive at `path` for streaming, unpacking it when it is
/// gzipped, with the gzip stream capped at [`MAX_STREAM_BYTES`] and its
/// extended headers checked by [`Headers`].
///
/// # Errors
///
/// Returns a description of the problem when the file cannot be opened.
fn open_tar(path: &Path, format: Format) -> Result<Box<dyn Read>, String> {
    let file = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    Ok(match format {
        Format::TarGz => Box::new(Headers::new(Capped {
            inner: GzDecoder::new(file),
            left: MAX_STREAM_BYTES,
        })),
        _ => Box::new(Headers::new(file)),
    })
}

/// Where a [`Headers`] reader is in the tar stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// Inside a header block, with this many of its bytes seen.
    Header(usize),
    /// Inside a block of extra sparse-file entries after a header, with this
    /// many of its bytes seen.
    Sparse(usize),
    /// Inside an entry's data and padding, with this many bytes left.
    Data(u64),
}

/// A reader that follows the blocks of a tar stream as they pass through it
/// and fails on a long name or extended header larger than any real one,
/// before the tar reader would load it whole into memory.
struct Headers<R> {
    /// The tar stream.
    inner: R,
    /// Where the stream is.
    part: Part,
    /// The header or sparse block being read.
    block: [u8; BLOCK],
    /// The data of the local extended header being read, kept to find a
    /// size it sets for the next entry.
    extended: Option<Vec<u8>>,
    /// How many bytes of the current entry's data are its contents, the rest
    /// being padding.
    contents: u64,
    /// The size the last local extended header set for the next entry.
    next_size: Option<u64>,
}

impl<R: Read> Headers<R> {
    /// Follows the tar stream `inner` from its start.
    fn new(inner: R) -> Self {
        Self {
            inner,
            part: Part::Header(0),
            block: [0; BLOCK],
            extended: None,
            contents: 0,
            next_size: None,
        }
    }

    /// Follows `bytes`, the next bytes of the stream.
    ///
    /// # Errors
    ///
    /// Returns an error when a header announces a long name or extended
    /// header larger than [`MAX_PATH_BYTES`] or
    /// [`MAX_EXTENDED_HEADER_BYTES`].
    fn follow(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            match self.part {
                Part::Header(seen) | Part::Sparse(seen) => {
                    let sparse = matches!(self.part, Part::Sparse(_));
                    let take = (BLOCK - seen).min(bytes.len());
                    self.block[seen..seen + take].copy_from_slice(&bytes[..take]);
                    bytes = &bytes[take..];
                    let seen = seen + take;
                    self.part = if seen < BLOCK {
                        if sparse {
                            Part::Sparse(seen)
                        } else {
                            Part::Header(seen)
                        }
                    } else if !sparse {
                        self.read_header()?
                    } else if self.block[504] != 0 {
                        Part::Sparse(0)
                    } else {
                        self.data()
                    };
                }
                Part::Data(left) => {
                    let take = usize::try_from(left).unwrap_or(usize::MAX).min(bytes.len());
                    if let Some(extended) = &mut self.extended {
                        let wanted = usize::try_from(self.contents)
                            .unwrap_or(usize::MAX)
                            .saturating_sub(extended.len())
                            .min(take);
                        extended.extend_from_slice(&bytes[..wanted]);
                    }
                    bytes = &bytes[take..];
                    let left = left - take as u64;
                    self.part = if left == 0 {
                        self.end_data()
                    } else {
                        Part::Data(left)
                    };
                }
            }
        }
        Ok(())
    }

    /// Reads the header block just completed and says what follows it.
    ///
    /// # Errors
    ///
    /// Returns an error for a long name or extended header that is too large.
    fn read_header(&mut self) -> io::Result<Part> {
        if self.block.iter().all(|byte| *byte == 0) {
            return Ok(Part::Header(0));
        }
        let kind = self.block[156];
        let size = block_size(&self.block[124..136]);
        let too_long = |what: &str, limit: u64| {
            io::Error::other(format!(
                "{what} is {size} bytes long, longer than the {limit} read accepts"
            ))
        };
        match kind {
            b'L' | b'K' if size > MAX_PATH_BYTES as u64 + 1 => {
                return Err(too_long("a member's long name", MAX_PATH_BYTES as u64));
            }
            b'x' | b'g' if size > MAX_EXTENDED_HEADER_BYTES => {
                return Err(too_long("an extended header", MAX_EXTENDED_HEADER_BYTES));
            }
            _ => {}
        }
        self.contents = if matches!(kind, b'L' | b'K' | b'x' | b'g') {
            size
        } else {
            self.next_size.take().unwrap_or(size)
        };
        self.extended = (kind == b'x').then(Vec::new);
        if kind == b'S' && self.block[482] != 0 {
            return Ok(Part::Sparse(0));
        }
        Ok(self.data())
    }

    /// The data of the current entry, padded to whole blocks, or the next
    /// header when it has none.
    fn data(&mut self) -> Part {
        let padded = self.contents.div_ceil(BLOCK as u64) * BLOCK as u64;
        if padded == 0 {
            self.end_data()
        } else {
            Part::Data(padded)
        }
    }

    /// Finishes the current entry's data: notes the size a local extended
    /// header set for the next entry, and goes on to the next header.
    fn end_data(&mut self) -> Part {
        if let Some(extended) = self.extended.take() {
            self.next_size = extended_size(&extended);
        }
        Part::Header(0)
    }
}

impl<R: Read> Read for Headers<R> {
    /// Reads from the tar stream, following its blocks.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.follow(&buf[..read])?;
        Ok(read)
    }
}

/// The number in the size field `field` of a tar header: octal digits, or a
/// big-endian binary number when its first byte has the high bit set; 0
/// when it is neither.
fn block_size(field: &[u8]) -> u64 {
    if field.first().is_some_and(|byte| byte & 0x80 != 0) {
        return field.iter().enumerate().fold(0u64, |size, (index, byte)| {
            let byte = if index == 0 { byte & 0x7f } else { *byte };
            size.saturating_mul(256).saturating_add(u64::from(byte))
        });
    }
    let text = String::from_utf8_lossy(field);
    u64::from_str_radix(text.trim_matches(|c: char| c == '\0' || c == ' '), 8).unwrap_or(0)
}

/// The `size` record of the extended header `data`, made of records such
/// as `17 size=123456789\n`, when it has one.
fn extended_size(data: &[u8]) -> Option<u64> {
    String::from_utf8_lossy(data).lines().find_map(|record| {
        let (_, field) = record.split_once(' ')?;
        field.strip_prefix("size=")?.trim().parse().ok()
    })
}

/// A reader that fails once more than a set number of bytes come out of it,
/// which stops decompression bombs.
struct Capped<R> {
    /// The reader whose output is counted.
    inner: R,
    /// How many more bytes may be read.
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    /// Reads from the inner reader, failing once the cap is passed.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.left = self.left.checked_sub(read as u64).ok_or_else(|| {
            io::Error::other(format!(
                "it unpacks to more than {}, which read does not go past",
                media::size(MAX_STREAM_BYTES as usize)
            ))
        })?;
        Ok(read)
    }
}

/// Unpacks the member stored as `stored` from the archive at `path`, of
/// format `format`, reading at most [`MAX_MEMBER_BYTES`] and one byte more,
/// so that a member past the cap shows as longer than it.
///
/// # Errors
///
/// Returns a description of the problem when the archive or member cannot be
/// read, or the member is encrypted.
fn fetch(path: &Path, format: Format, stored: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    match format {
        Format::Zip => {
            let file = File::open(path).map_err(|e| e.to_string())?;
            let mut archive =
                zip::ZipArchive::new(BufReader::new(file)).map_err(|e| e.to_string())?;
            let entry = archive.by_name(stored).map_err(|e| e.to_string())?;
            entry
                .take(MAX_MEMBER_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
        }
        Format::Tar | Format::TarGz => {
            let mut archive = tar::Archive::new(open_tar(path, format)?);
            for entry in archive.entries().map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.path().is_ok_and(|p| p.to_string_lossy() == stored) {
                    entry
                        .take(MAX_MEMBER_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    break;
                }
            }
        }
    }
    Ok(bytes)
}

/// Shows `member` of the archive `target` names: its text numbered as
/// `selector` picks, an image or a PDF's text, or a note that it is binary.
///
/// # Errors
///
/// Returns [`ToolError`] when the member cannot be unpacked, is larger than
/// [`MAX_MEMBER_BYTES`], or `selector` does not apply to what it holds.
fn member_output(
    target: &Target,
    format: Format,
    member: &Member,
    selector: &Selector,
) -> Result<ToolOutput, ToolError> {
    let label = format!("{}:{}", target.label, member.path);
    if member.kind == Kind::Other {
        return Ok(ToolOutput::new(format!(
            "[{label} is a link or special file, with no contents to read]"
        )));
    }
    let bytes = fetch(&target.path, format, &member.stored)
        .map_err(|error| ToolError::Failed(format!("could not unpack {label}: {error}")))?;
    if bytes.len() as u64 > MAX_MEMBER_BYTES {
        return Err(ToolError::Failed(format!(
            "{label} unpacks to more than {}, which read does not unpack",
            media::size(MAX_MEMBER_BYTES as usize)
        )));
    }
    let size = media::size(bytes.len());
    let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
    match media::sniff(head) {
        Some(Media::Image(media_type)) => {
            if selector.picks_lines() {
                return Err(ToolError::InvalidArguments(format!(
                    "{label} is an image; line selectors apply to text"
                )));
            }
            media::image(&bytes, media_type, &label)
        }
        Some(Media::Pdf) => {
            let (text, pages) = media::pdf_text(&bytes, &label)?;
            let numbered =
                lines::number(|| Ok(io::Cursor::new(text.as_bytes())), selector, &label)?;
            Ok(ToolOutput::new(format!(
                "[pdf {label}, {}; inside an archive, read-only]\n{numbered}",
                media::pages(pages)
            )))
        }
        None if head.contains(&0) || std::str::from_utf8(&bytes).is_err() => Ok(ToolOutput::new(
            format!("[{label} is binary, {size}; read shows text members]"),
        )),
        None => {
            let numbered = lines::number(|| Ok(io::Cursor::new(&bytes[..])), selector, &label)?;
            Ok(ToolOutput::new(format!(
                "[{label}, {size}; inside an archive, read-only: no tag, cannot be edited]\n{numbered}"
            )))
        }
    }
}

/// Lists the members of the archive `target` names that lie under the
/// directory `dir`, or all of them when it is empty, one numbered line each,
/// paged as `selector` picks.
///
/// # Errors
///
/// Returns [`ToolError`] when the listing cannot be numbered.
fn listing(
    target: &Target,
    format: Format,
    members: &[Member],
    complete: bool,
    dir: &str,
    selector: &Selector,
) -> Result<ToolOutput, ToolError> {
    let label = &target.label;
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let shown: Vec<&Member> = members
        .iter()
        .filter(|member| member.path.starts_with(&prefix) && member.path.len() > prefix.len())
        .collect();
    let files = shown.iter().filter(|m| m.kind == Kind::File).count();
    let unpacked: u64 = shown
        .iter()
        .filter(|m| m.kind == Kind::File)
        .map(|m| m.size)
        .sum();
    let place = if dir.is_empty() {
        format!("archive {label}, {}", format.name())
    } else {
        format!("{label}:{dir}/, in a {} archive", format.name())
    };
    let more = if complete { "" } else { ", listing stops here" };
    let mut header = format!(
        "[{place}, {} {} of {} entries, {} unpacked{more}; read {label}:<member> for one]\n",
        files,
        if files == 1 { "file" } else { "files" },
        shown.len(),
        media::size(usize::try_from(unpacked).unwrap_or(usize::MAX)),
    );
    if shown.is_empty() {
        header.push_str("(no members)\n");
        return Ok(ToolOutput::new(header));
    }
    let text: String = shown
        .iter()
        .map(|member| match member.kind {
            Kind::Dir => format!("{:>9}  {}/\n", "-", member.path),
            Kind::File => format!(
                "{:>9}  {}\n",
                media::size(usize::try_from(member.size).unwrap_or(usize::MAX)),
                member.path
            ),
            Kind::Other => format!("{:>9}  {}\n", "link", member.path),
        })
        .collect();
    let name = if dir.is_empty() {
        label.clone()
    } else {
        format!("{label}:{dir}/")
    };
    let numbered = lines::number(|| Ok(io::Cursor::new(text.as_bytes())), selector, &name)?;
    Ok(ToolOutput::new(header + &numbered))
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Write as _;

    use super::*;

    /// A zip archive of `(name, contents)` pairs, deflated; names ending in
    /// `/` become directories.
    pub(in super::super) fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in members {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(contents).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    /// A tar archive of `(name, contents)` pairs, gzipped when `gzip` is set.
    pub(in super::super) fn tar(members: &[(&str, &[u8])], gzip: bool) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, contents) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *contents).unwrap();
        }
        let bytes = builder.into_inner().unwrap();
        if !gzip {
            return bytes;
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// Archives are told apart by magic bytes and, for gzip, by name.
    #[test]
    fn sniffs_archives() {
        assert_eq!(sniff(&zip(&[("a", b"x")]), "a.bin"), Some(Format::Zip));
        assert_eq!(
            sniff(&tar(&[("a", b"x")], false), "a.bin"),
            Some(Format::Tar)
        );
        assert_eq!(
            sniff(&tar(&[("a", b"x")], true), "a.tgz"),
            Some(Format::TarGz)
        );
        assert_eq!(sniff(&tar(&[("a", b"x")], true), "a.gz"), None);
        assert_eq!(sniff(b"plain text", "notes.txt"), None);
    }

    /// Member paths are compared without leading `./` or trailing `/`.
    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("./src/a.rs"), "src/a.rs");
        assert_eq!(normalize("/src/"), "src");
    }

    /// Appends to `builder` an entry of type `kind`, such as a long name or
    /// an extended header, holding `data`.
    fn special(builder: &mut tar::Builder<Vec<u8>>, kind: tar::EntryType, data: &[u8]) {
        let mut header = tar::Header::new_ustar();
        header.set_path("././@LongLink").unwrap();
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, data).unwrap();
    }

    /// Appends to `builder` a regular file `name` holding `data`.
    fn file(builder: &mut tar::Builder<Vec<u8>>, name: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, data).unwrap();
    }

    /// Writes `bytes` to the file `name` of a fresh scratch directory and
    /// lists it as a tar archive.
    fn list_tar(name: &str, bytes: &[u8]) -> Result<Vec<String>, String> {
        let dir = crate::testutil::scratch(name);
        let path = dir.join("a.tar");
        std::fs::write(&path, bytes).unwrap();
        list(&path, Format::Tar).map(|(members, _)| members.into_iter().map(|m| m.path).collect())
    }

    /// Long names and extended headers of a sensible size are read; ones
    /// larger than any real name are refused before they are loaded.
    #[test]
    fn refuses_huge_tar_headers() {
        let long = format!("{}/end.txt", "d".repeat(300));
        let mut builder = tar::Builder::new(Vec::new());
        file(&mut builder, &long, b"x");
        special(
            &mut builder,
            tar::EntryType::XHeader,
            b"30 mtime=1700000000.123456789\n",
        );
        file(&mut builder, "after.txt", b"after\n");
        let fine = list_tar("tar-headers-fine", &builder.into_inner().unwrap()).unwrap();
        assert_eq!(fine, vec![long, "after.txt".to_string()]);
        let mut builder = tar::Builder::new(Vec::new());
        special(
            &mut builder,
            tar::EntryType::GNULongName,
            &vec![b'a'; 8 << 20],
        );
        file(&mut builder, "x", b"x");
        let error = list_tar("tar-headers-long", &builder.into_inner().unwrap()).unwrap_err();
        assert!(error.contains("long name is 8388608 bytes long"), "{error}");
        let mut builder = tar::Builder::new(Vec::new());
        let record = format!("path={}\n", "p".repeat(1 << 20));
        let record = format!("{} {record}", record.len() + 8);
        special(&mut builder, tar::EntryType::XHeader, record.as_bytes());
        file(&mut builder, "x", b"x");
        let error = list_tar("tar-headers-pax", &builder.into_inner().unwrap()).unwrap_err();
        assert!(error.contains("an extended header is"), "{error}");
        let mut builder = tar::Builder::new(Vec::new());
        let record = format!("path={}\n", "q".repeat(5000));
        let record = format!("{} {record}", record.len() + 5);
        special(&mut builder, tar::EntryType::XHeader, record.as_bytes());
        file(&mut builder, "x", b"x");
        let error = list_tar("tar-headers-path", &builder.into_inner().unwrap()).unwrap_err();
        assert!(error.contains("path is 5000 bytes long"), "{error}");
    }

    /// A size set by an extended header moves the reader past the right
    /// number of data blocks.
    #[test]
    fn follows_sizes_in_extended_headers() {
        let mut builder = tar::Builder::new(Vec::new());
        special(&mut builder, tar::EntryType::XHeader, b"13 size=1000\n");
        let mut header = tar::Header::new_ustar();
        header.set_path("big.bin").unwrap();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &[][..]).unwrap();
        let mut bytes = builder.get_ref().clone();
        bytes.extend_from_slice(&[7u8; 1024]);
        let mut tail = tar::Builder::new(Vec::new());
        file(&mut tail, "next.txt", b"n");
        bytes.extend_from_slice(&tail.into_inner().unwrap());
        let mut reader = Headers::new(&bytes[..]);
        io::copy(&mut reader, &mut io::sink()).unwrap();
        assert_eq!(reader.next_size, None);
        assert_eq!(reader.part, Part::Header(0));
        let listed = list_tar("tar-headers-size", &bytes).unwrap();
        assert_eq!(listed, vec!["big.bin".to_string(), "next.txt".to_string()]);
    }

    /// A gzip stream that unpacks past the cap is stopped with an error.
    #[test]
    fn caps_unpacked_streams() {
        let zeros = vec![0u8; 1 << 20];
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&zeros).unwrap();
        let packed = encoder.finish().unwrap();
        let mut capped = Capped {
            inner: GzDecoder::new(&packed[..]),
            left: 1000,
        };
        let error = io::copy(&mut capped, &mut io::sink()).unwrap_err();
        assert!(
            error.to_string().contains("unpacks to more than"),
            "{error}"
        );
    }
}
