//! Parses NTFS structures from raw bytes and builds a tree from the master
//! file table (MFT). Reading the MFT in large sequential blocks is much faster
//! on a cold disk than listing every directory. The Windows glue that reads
//! the volume is in `mft.rs`. This module has no platform calls, so its tests
//! run everywhere.

use super::Progress;
use crate::tree::{Dir, Kind, flag};
use std::sync::atomic::Ordering::Relaxed;

/// Records 0 to 23 are NTFS metafiles ($MFT, $Bitmap, $Extend and reserved
/// records). A directory listing never shows them.
const FIRST_USER_RECORD: u32 = 24;
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;
const RECORD_IN_USE: u16 = 0x01;
const RECORD_DIRECTORY: u16 = 0x02;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const ATTR_FLAG_COMPRESSED: u16 = 0x0001;
const ATTR_FLAG_SPARSE: u16 = 0x8000;
const NAMESPACE_DOS: u8 = 2;
/// Fixups protect every 512 bytes of a record, whatever the sector size.
const FIXUP_STRIDE: usize = 512;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A record number is the low 48 bits of a file reference. The high 16 bits
/// hold the sequence number of the record when the reference was made.
fn split_reference(reference: u64) -> (u64, u16) {
    (reference & 0xFFFF_FFFF_FFFF, (reference >> 48) as u16)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Boot {
    pub bytes_per_sector: u64,
    pub cluster_size: u64,
    pub mft_offset: u64,
    pub record_size: usize,
}

pub fn parse_boot(sector: &[u8]) -> Option<Boot> {
    if sector.get(3..11)? != b"NTFS    " {
        return None;
    }
    let bytes_per_sector = u64::from(u16_at(sector, 0x0B)?);
    let raw = *sector.get(0x0D)?;
    // Values above 0x80 are a negative power of two: 2^(256 - value) sectors.
    let sectors_per_cluster = if raw > 0x80 {
        1u64.checked_shl(256 - u32::from(raw))?
    } else {
        u64::from(raw)
    };
    let cluster_size = bytes_per_sector.checked_mul(sectors_per_cluster)?;
    let mft_lcn = u64_at(sector, 0x30)?;
    let per_record = *sector.get(0x40)? as i8;
    // A negative value means 2^(-value) bytes, otherwise a number of clusters.
    let record_size = if per_record < 0 {
        1usize.checked_shl(u32::from(per_record.unsigned_abs()))?
    } else {
        usize::try_from(cluster_size.checked_mul(per_record as u64)?).ok()?
    };
    if bytes_per_sector == 0
        || cluster_size == 0
        || record_size < 256
        || !record_size.is_multiple_of(FIXUP_STRIDE)
    {
        return None;
    }
    Some(Boot {
        bytes_per_sector,
        cluster_size,
        mft_offset: mft_lcn.checked_mul(cluster_size)?,
        record_size,
    })
}

/// One extent of a non-resident attribute. `lcn` is `None` for a sparse run.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Run {
    pub lcn: Option<u64>,
    pub clusters: u64,
}

/// Decodes a run list: each run starts with a header byte whose low nibble
/// is the size of the length field and whose high nibble is the size of the
/// signed, relative cluster offset. A header of 0 ends the list.
pub fn parse_runs(mut data: &[u8]) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut lcn: i64 = 0;
    loop {
        let header = *data.first()?;
        if header == 0 {
            return Some(runs);
        }
        let (len_size, off_size) = (usize::from(header & 0x0F), usize::from(header >> 4));
        if len_size == 0 || len_size > 8 || off_size > 8 {
            return None;
        }
        let field = |bytes: &[u8], signed: bool| -> i64 {
            let mut value: i64 = 0;
            for (i, &b) in bytes.iter().enumerate() {
                value |= i64::from(b) << (8 * i);
            }
            let bits = 8 * bytes.len();
            if signed && bits > 0 && bits < 64 && bytes[bytes.len() - 1] & 0x80 != 0 {
                value -= 1 << bits;
            }
            value
        };
        let clusters = field(data.get(1..1 + len_size)?, false);
        if clusters <= 0 {
            return None;
        }
        let run_lcn = if off_size == 0 {
            None
        } else {
            lcn = lcn.checked_add(field(
                data.get(1 + len_size..1 + len_size + off_size)?,
                true,
            ))?;
            Some(u64::try_from(lcn).ok()?)
        };
        runs.push(Run {
            lcn: run_lcn,
            clusters: clusters as u64,
        });
        data = data.get(1 + len_size + off_size..)?;
    }
}

/// Restores the last two bytes of every 512-byte block from the update
/// sequence array. Returns false for a torn or damaged record.
pub fn apply_fixups(record: &mut [u8]) -> bool {
    let (Some(offset), Some(count)) = (u16_at(record, 4), u16_at(record, 6)) else {
        return false;
    };
    let (offset, count) = (usize::from(offset), usize::from(count));
    if count == 0 || (count - 1) * FIXUP_STRIDE > record.len() || offset + 2 * count > record.len()
    {
        return false;
    }
    let check = [record[offset], record[offset + 1]];
    for i in 1..count {
        let end = i * FIXUP_STRIDE - 2;
        if record[end..end + 2] != check {
            return false;
        }
        let saved = offset + 2 * i;
        record[end] = record[saved];
        record[end + 1] = record[saved + 1];
    }
    true
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileName<'a> {
    pub parent: u64,
    pub parent_seq: u16,
    /// UTF-16LE bytes.
    pub name: &'a [u8],
    pub namespace: u8,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct DataSize {
    pub apparent: u64,
    pub allocated: u64,
}

#[derive(Debug, Default)]
pub struct Record<'a> {
    pub in_use: bool,
    pub directory: bool,
    pub reparse: bool,
    /// The base record when this is an extension record, else 0.
    pub base: u64,
    pub seq: u16,
    pub names: Vec<FileName<'a>>,
    /// Sizes of the unnamed data stream, from its first extent.
    pub data: Option<DataSize>,
}

/// Parses a file record after `apply_fixups`. Returns `None` for a record
/// that is not a valid FILE record.
pub fn parse_record(record: &[u8]) -> Option<Record<'_>> {
    if record.get(0..4)? != b"FILE" {
        return None;
    }
    let flags = u16_at(record, 0x16)?;
    let used = usize::try_from(u32_at(record, 0x18)?)
        .ok()?
        .min(record.len());
    let mut parsed = Record {
        in_use: flags & RECORD_IN_USE != 0,
        directory: flags & RECORD_DIRECTORY != 0,
        base: split_reference(u64_at(record, 0x20)?).0,
        seq: u16_at(record, 0x10)?,
        ..Record::default()
    };
    let mut at = usize::from(u16_at(record, 0x14)?);
    while at + 8 <= used {
        let kind = u32_at(record, at)?;
        if kind == ATTR_END {
            break;
        }
        let len = usize::try_from(u32_at(record, at + 4)?).ok()?;
        if len < 16 || at + len > used {
            return None;
        }
        let attr = &record[at..at + len];
        let resident = attr[8] == 0;
        let unnamed = attr[9] == 0;
        match (kind, resident) {
            (ATTR_STANDARD_INFORMATION, true) => {
                let value = resident_value(attr)?;
                parsed.reparse = u32_at(value, 0x20)? & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            }
            (ATTR_FILE_NAME, true) => {
                let value = resident_value(attr)?;
                let (parent, parent_seq) = split_reference(u64_at(value, 0)?);
                let chars = usize::from(*value.get(0x40)?);
                parsed.names.push(FileName {
                    parent,
                    parent_seq,
                    name: value.get(0x42..0x42 + 2 * chars)?,
                    namespace: *value.get(0x41)?,
                });
            }
            (ATTR_DATA, true) if unnamed => {
                // Resident data lives inside the record and takes no clusters.
                // The Windows directory listing reports its size rounded up to
                // 8 bytes, so do the same: the result must not depend on
                // whether minimenta runs as an administrator.
                let size = u64::from(u32_at(attr, 0x10)?);
                parsed.data = Some(DataSize {
                    apparent: size,
                    allocated: size.next_multiple_of(8),
                });
            }
            (ATTR_DATA, false) if unnamed && u64_at(attr, 0x10)? == 0 => {
                let attr_flags = u16_at(attr, 0x0C)?;
                let allocated = if attr_flags & (ATTR_FLAG_COMPRESSED | ATTR_FLAG_SPARSE) != 0 {
                    u64_at(attr, 0x40)?
                } else {
                    u64_at(attr, 0x28)?
                };
                parsed.data = Some(DataSize {
                    apparent: u64_at(attr, 0x30)?,
                    allocated,
                });
            }
            _ => {}
        }
        at += len;
    }
    Some(parsed)
}

fn resident_value(attr: &[u8]) -> Option<&[u8]> {
    let len = usize::try_from(u32_at(attr, 0x10)?).ok()?;
    let offset = usize::from(u16_at(attr, 0x14)?);
    attr.get(offset..offset + len)
}

/// The run list and the size of the $MFT data stream, from record 0.
pub fn mft_extents(record: &[u8]) -> Option<(Vec<Run>, u64)> {
    let used = usize::try_from(u32_at(record, 0x18)?)
        .ok()?
        .min(record.len());
    let mut at = usize::from(u16_at(record, 0x14)?);
    while at + 8 <= used {
        let kind = u32_at(record, at)?;
        let len = usize::try_from(u32_at(record, at + 4)?).ok()?;
        if kind == ATTR_END || len < 16 || at + len > used {
            return None;
        }
        let attr = &record[at..at + len];
        if kind == ATTR_DATA && attr[8] != 0 && attr[9] == 0 {
            let runs = parse_runs(attr.get(usize::from(u16_at(attr, 0x20)?)..)?)?;
            return Some((runs, u64_at(attr, 0x30)?));
        }
        at += len;
    }
    None
}

/// Appends UTF-16LE text as WTF-8, the encoding of `OsStr` on Windows, so a
/// name with an unpaired surrogate still round-trips to the same file.
fn push_wtf8(utf16le: &[u8], out: &mut Vec<u8>) {
    let units = utf16le
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c));
    for unit in char::decode_utf16(units) {
        let code = match unit {
            Ok(c) => u32::from(c),
            Err(e) => u32::from(e.unpaired_surrogate()),
        };
        match code {
            0..0x80 => out.push(code as u8),
            0x80..0x800 => out.extend([0xC0 | (code >> 6) as u8, 0x80 | (code & 0x3F) as u8]),
            0x800..0x10000 => out.extend([
                0xE0 | (code >> 12) as u8,
                0x80 | ((code >> 6) & 0x3F) as u8,
                0x80 | (code & 0x3F) as u8,
            ]),
            _ => out.extend([
                0xF0 | (code >> 18) as u8,
                0x80 | ((code >> 12) & 0x3F) as u8,
                0x80 | ((code >> 6) & 0x3F) as u8,
                0x80 | (code & 0x3F) as u8,
            ]),
        }
    }
}

const IN_USE: u8 = 1;
const DIRECTORY: u8 = 2;
const REPARSE: u8 = 4;

#[derive(Clone, Copy, Default)]
struct Rec {
    seq: u16,
    flags: u8,
    names: u16,
    disk: u64,
    apparent: u64,
}

struct Link {
    parent: u32,
    parent_seq: u16,
    record: u32,
    name_start: u32,
    name_len: u32,
}

/// Everything the tree needs from the MFT, collected record by record.
#[derive(Default)]
pub struct Table {
    recs: Vec<Rec>,
    links: Vec<Link>,
    names: Vec<u8>,
}

impl Table {
    pub fn with_capacity(records: usize) -> Self {
        Table {
            recs: Vec::with_capacity(records),
            links: Vec::with_capacity(records),
            names: Vec::with_capacity(records * 16),
        }
    }

    /// Adds the record at `index`. Extension records add their names and
    /// data sizes to their base record.
    pub fn add(&mut self, index: u32, record: &Record) {
        let Ok(target) = u32::try_from(if record.base != 0 {
            record.base
        } else {
            u64::from(index)
        }) else {
            return;
        };
        if self.recs.len() <= target as usize {
            self.recs.resize(target as usize + 1, Rec::default());
        }
        let rec = &mut self.recs[target as usize];
        if record.base == 0 {
            rec.seq = record.seq;
            rec.flags = (u8::from(record.in_use) * IN_USE)
                | (u8::from(record.directory) * DIRECTORY)
                | (u8::from(record.reparse) * REPARSE);
        }
        if let Some(data) = record.data {
            (rec.disk, rec.apparent) = (data.allocated, data.apparent);
        }
        for name in record.names.iter().filter(|n| n.namespace != NAMESPACE_DOS) {
            let Ok(parent) = u32::try_from(name.parent) else {
                continue;
            };
            let name_start = self.names.len() as u32;
            push_wtf8(name.name, &mut self.names);
            self.links.push(Link {
                parent,
                parent_seq: name.parent_seq,
                record: target,
                name_start,
                name_len: self.names.len() as u32 - name_start,
            });
            self.recs[target as usize].names += 1;
        }
    }

    /// Builds the tree below the directory record `root`.
    pub fn build(&self, root: u32, progress: &Progress) -> Dir {
        // Children grouped by parent (compressed sparse rows): one counting
        // pass, one prefix sum, one fill.
        let n = self.recs.len();
        let valid = |l: &Link| {
            let (parent, child) = (l.parent as usize, l.record as usize);
            l.record >= FIRST_USER_RECORD
                && parent < n
                && self.recs[child].flags & IN_USE != 0
                && self.recs[parent].flags & IN_USE != 0
                && self.recs[parent].seq == l.parent_seq
        };
        let mut start = vec![0u32; n + 1];
        for l in self.links.iter().filter(|l| valid(l)) {
            start[l.parent as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut order = vec![0u32; start[n] as usize];
        for (i, l) in self.links.iter().enumerate().filter(|(_, l)| valid(l)) {
            order[fill[l.parent as usize] as usize] = i as u32;
            fill[l.parent as usize] += 1;
        }
        let mut counted = vec![false; n];
        self.walk(root, &start, &order, &mut counted, progress)
    }

    fn walk(
        &self,
        dir_rec: u32,
        start: &[u32],
        order: &[u32],
        counted: &mut [bool],
        progress: &Progress,
    ) -> Dir {
        let mut dir = Dir {
            id: u64::from(dir_rec),
            ..Dir::default()
        };
        let (from, to) = (
            start[dir_rec as usize] as usize,
            start[dir_rec as usize + 1] as usize,
        );
        for &link in &order[from..to] {
            let link = &self.links[link as usize];
            let rec = self.recs[link.record as usize];
            let name = &self.names[link.name_start as usize..][..link.name_len as usize];
            if rec.flags & DIRECTORY != 0 {
                if rec.flags & REPARSE != 0 {
                    // Junctions and directory symlinks are never followed.
                    dir.push(name, Kind::Symlink, 0, 0, 0);
                } else {
                    let index = dir.entries.len();
                    dir.push(name, Kind::Dir, 0, 0, 0);
                    let sub = self.walk(link.record, start, order, counted, progress);
                    dir.attach(index, sub, 0);
                }
                continue;
            }
            let mut flags = 0;
            if rec.names > 1 {
                flags |= flag::MULTI_LINK;
                if std::mem::replace(&mut counted[link.record as usize], true) {
                    flags |= flag::HARDLINK;
                }
            }
            dir.push(name, Kind::File, rec.disk, rec.apparent, flags);
        }
        progress.items.fetch_add(dir.entries.len() as u64, Relaxed);
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a valid 1024-byte FILE record with the given attributes and
    /// fixups applied as NTFS writes them.
    fn record(seq: u16, flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes());
        r[0x10..0x12].copy_from_slice(&seq.to_le_bytes());
        r[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
        r[0x20..0x28].copy_from_slice(&base.to_le_bytes());
        let mut at = 0x38;
        for a in attrs {
            r[at..at + a.len()].copy_from_slice(a);
            at += a.len();
        }
        r[at..at + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        r[0x18..0x1C].copy_from_slice(&((at + 8) as u32).to_le_bytes());
        // Update sequence: move the last two bytes of each 512-byte block
        // into the array and write the check value in their place.
        let check = [0xAB, 0xCD];
        r[0x30..0x32].copy_from_slice(&check);
        for i in 1..3 {
            let end = i * 512 - 2;
            let (a, b) = (r[end], r[end + 1]);
            r[0x30 + 2 * i] = a;
            r[0x31 + 2 * i] = b;
            r[end..end + 2].copy_from_slice(&check);
        }
        r
    }

    fn resident(kind: u32, value: &[u8]) -> Vec<u8> {
        let len = (0x18 + value.len()).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
        a[0x14..0x16].copy_from_slice(&0x18u16.to_le_bytes());
        a[0x18..0x18 + value.len()].copy_from_slice(value);
        a
    }

    fn file_name(parent: u64, parent_seq: u16, name: &str, namespace: u8) -> Vec<u8> {
        let utf16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut v = vec![0u8; 0x42 + utf16.len()];
        v[0..8].copy_from_slice(&(parent | u64::from(parent_seq) << 48).to_le_bytes());
        v[0x40] = (utf16.len() / 2) as u8;
        v[0x41] = namespace;
        v[0x42..].copy_from_slice(&utf16);
        resident(ATTR_FILE_NAME, &v)
    }

    fn std_info(attributes: u32) -> Vec<u8> {
        let mut v = vec![0u8; 0x30];
        v[0x20..0x24].copy_from_slice(&attributes.to_le_bytes());
        resident(ATTR_STANDARD_INFORMATION, &v)
    }

    fn non_resident_data(
        apparent: u64,
        allocated: u64,
        flags: u16,
        total: u64,
        runs: &[u8],
    ) -> Vec<u8> {
        let len = (0x48 + runs.len() + 1).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = 1;
        a[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
        a[0x20..0x22].copy_from_slice(&0x48u16.to_le_bytes());
        a[0x28..0x30].copy_from_slice(&allocated.to_le_bytes());
        a[0x30..0x38].copy_from_slice(&apparent.to_le_bytes());
        a[0x40..0x48].copy_from_slice(&total.to_le_bytes());
        a[0x48..0x48 + runs.len()].copy_from_slice(runs);
        a
    }

    fn parsed(raw: &mut [u8]) -> Record<'_> {
        assert!(apply_fixups(raw));
        parse_record(raw).unwrap()
    }

    #[test]
    fn boot_sector_gives_geometry() {
        let mut s = vec![0u8; 512];
        s[3..11].copy_from_slice(b"NTFS    ");
        s[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        s[0x0D] = 8;
        s[0x30..0x38].copy_from_slice(&786_432u64.to_le_bytes());
        s[0x40] = 0xF6; // -10: 2^10 = 1024 bytes per record
        let boot = parse_boot(&s).unwrap();
        assert_eq!(
            boot,
            Boot {
                bytes_per_sector: 512,
                cluster_size: 4096,
                mft_offset: 786_432 * 4096,
                record_size: 1024
            }
        );
        s[3] = b'X';
        assert!(parse_boot(&s).is_none());
    }

    #[test]
    fn run_lists_decode_relative_and_sparse_runs() {
        // 0x18 clusters at LCN 0x5634, then 0x10 sparse clusters, then 4
        // clusters 0x100 before the previous run (negative offset).
        let runs = parse_runs(&[
            0x21, 0x18, 0x34, 0x56, 0x01, 0x10, 0x21, 0x04, 0x00, 0xFF, 0x00,
        ])
        .unwrap();
        assert_eq!(
            runs,
            [
                Run {
                    lcn: Some(0x5634),
                    clusters: 0x18
                },
                Run {
                    lcn: None,
                    clusters: 0x10
                },
                Run {
                    lcn: Some(0x5534),
                    clusters: 4
                },
            ]
        );
        assert!(parse_runs(&[0x21, 0x18]).is_none(), "accepted a cut run");
    }

    #[test]
    fn fixups_restore_sector_ends_and_reject_torn_records() {
        let mut raw = record(1, RECORD_IN_USE, 0, &[]);
        let mut torn = raw.clone();
        assert!(apply_fixups(&mut raw));
        assert_eq!(&raw[510..512], &[0, 0]);
        torn[1022] = 0x11;
        assert!(!apply_fixups(&mut torn));
    }

    #[test]
    fn records_give_names_flags_and_sizes() {
        let mut raw = record(
            7,
            RECORD_IN_USE,
            0,
            &[
                std_info(0x20),
                file_name(5, 5, "LONGFI~1.TXT", NAMESPACE_DOS),
                file_name(5, 5, "long file name.txt", 1),
                non_resident_data(10_000, 12_288, 0, 0, &[0x11, 0x03, 0x40, 0x00]),
            ],
        );
        let r = parsed(&mut raw);
        assert!(r.in_use && !r.directory && !r.reparse);
        assert_eq!(r.seq, 7);
        assert_eq!(r.names.len(), 2);
        assert_eq!(
            r.data,
            Some(DataSize {
                apparent: 10_000,
                allocated: 12_288
            })
        );
    }

    #[test]
    fn compressed_files_count_their_compressed_size() {
        let mut raw = record(
            1,
            RECORD_IN_USE,
            0,
            &[non_resident_data(
                1 << 20,
                1 << 20,
                ATTR_FLAG_COMPRESSED,
                65_536,
                &[0x11, 0x10, 0x40, 0x00],
            )],
        );
        assert_eq!(
            parsed(&mut raw).data,
            Some(DataSize {
                apparent: 1 << 20,
                allocated: 65_536
            })
        );
    }

    #[test]
    fn small_files_live_inside_the_record() {
        let mut raw = record(1, RECORD_IN_USE, 0, &[resident(ATTR_DATA, b"hello")]);
        assert_eq!(
            parsed(&mut raw).data,
            Some(DataSize {
                apparent: 5,
                allocated: 8
            })
        );
    }

    #[test]
    fn utf16_names_become_wtf8() {
        let mut out = Vec::new();
        let name: Vec<u8> = "å 日本 🗑"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        push_wtf8(&name, &mut out);
        assert_eq!(out, "å 日本 🗑".as_bytes());
        out.clear();
        push_wtf8(&0xD800u16.to_le_bytes(), &mut out);
        assert_eq!(
            out,
            [0xED, 0xA0, 0x80],
            "an unpaired surrogate keeps its WTF-8 form"
        );
    }

    /// Root (5) holds `docs` (30) and a metafile; `docs` holds a file with
    /// two hard links, a junction, and a file with a stale parent reference.
    #[test]
    fn the_table_builds_the_tree_below_a_root() {
        let mut table = Table::default();
        let mut add = |index: u32, raw: &mut Vec<u8>| {
            assert!(apply_fixups(raw));
            table.add(index, &parse_record(raw).unwrap());
        };
        add(
            5,
            &mut record(
                5,
                RECORD_IN_USE | RECORD_DIRECTORY,
                0,
                &[file_name(5, 5, ".", 1)],
            ),
        );
        add(
            6,
            &mut record(6, RECORD_IN_USE, 0, &[file_name(5, 5, "$Bitmap", 1)]),
        );
        add(
            30,
            &mut record(
                2,
                RECORD_IN_USE | RECORD_DIRECTORY,
                0,
                &[file_name(5, 5, "docs", 1)],
            ),
        );
        add(
            31,
            &mut record(
                1,
                RECORD_IN_USE,
                0,
                &[
                    file_name(30, 2, "a.bin", 1),
                    file_name(30, 2, "b.bin", 1),
                    resident(ATTR_DATA, &[7; 100]),
                ],
            ),
        );
        // An extension record carries the data sizes of record 32.
        add(
            32,
            &mut record(1, RECORD_IN_USE, 0, &[file_name(30, 2, "big.iso", 1)]),
        );
        add(
            33,
            &mut record(
                1,
                RECORD_IN_USE,
                32,
                &[non_resident_data(
                    9000,
                    12_288,
                    0,
                    0,
                    &[0x11, 0x03, 0x40, 0x00],
                )],
            ),
        );
        add(
            34,
            &mut record(
                1,
                RECORD_IN_USE | RECORD_DIRECTORY,
                0,
                &[
                    std_info(FILE_ATTRIBUTE_REPARSE_POINT),
                    file_name(30, 2, "link", 1),
                ],
            ),
        );
        add(
            35,
            &mut record(1, RECORD_IN_USE, 0, &[file_name(30, 1, "orphan", 1)]),
        );
        add(36, &mut record(1, 0, 0, &[file_name(30, 2, "deleted", 1)]));

        let progress = Progress::default();
        let root = table.build(5, &progress);
        let names: Vec<&[u8]> = root.entries.iter().map(|e| root.name(e)).collect();
        assert_eq!(
            names,
            [b"docs"],
            "metafiles and the root's own name stay out"
        );
        let docs = root.entries[0].dir.as_ref().unwrap();
        let mut seen: Vec<(String, u64, u64, Kind, u8)> = docs
            .entries
            .iter()
            .map(|e| {
                (
                    String::from_utf8_lossy(docs.name(e)).into_owned(),
                    e.disk,
                    e.apparent,
                    e.kind,
                    e.flags,
                )
            })
            .collect();
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            seen,
            [
                ("a.bin".into(), 104, 100, Kind::File, flag::MULTI_LINK),
                (
                    "b.bin".into(),
                    104,
                    100,
                    Kind::File,
                    flag::MULTI_LINK | flag::HARDLINK
                ),
                ("big.iso".into(), 12_288, 9000, Kind::File, 0),
                ("link".into(), 0, 0, Kind::Symlink, 0),
            ]
        );
        assert_eq!(
            root.totals().apparent,
            9100,
            "the second hard link is not counted"
        );
        assert_eq!(progress.items.load(Relaxed), 5);
    }
}
