// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Header sniff behind the binary scan: is an exec-bit file a program, or a shared library?
//!
//! The ELF order is elfutils' `eu-elfclassify --executable`; the Mach-O fat window is
//! libmagic's `Magdir/cafebabe`. A file that is neither, or cannot be parsed, is a program:
//! a wrongly claimed file costs a stray launcher, a wrongly dropped one a missing tool.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use elf::abi::{
    DF_1_PIE, DT_DEBUG, DT_FLAGS_1, DT_NULL, DT_SONAME, ELFMAGIC, ET_DYN, ET_EXEC, PT_DYNAMIC, PT_INTERP, PT_LOAD,
};
use elf::dynamic::DynamicTable;
use elf::endian::AnyEndian;
use elf::{ElfStream, ParseError};

/// A real dynamic table is a few hundred bytes; a segment past this is malformed, so the file is kept.
const DYNAMIC_SEGMENT_MAX_BYTES: u64 = 1 << 20;
const MACH_O_EXECUTE: u32 = 2;
/// A `0xcafebabe` file with more slices than this is a Java class, whose major version reads as `nfat_arch`.
const FAT_SLICE_MAX: u32 = 19;
/// Bytes of a thin Mach-O header, magic included; `mach_header` is 28, `mach_header_64` adds a reserved word.
const MACH_O_HEADER_BYTES_32: usize = 28;
const MACH_O_HEADER_BYTES_64: usize = 32;
/// Offset of `filetype` in a thin Mach-O header, after magic, cputype and cpusubtype.
const MACH_O_FILETYPE_OFFSET: usize = 12;

/// Whether the file at `path` is a program rather than a shared library, object or other non-executable.
///
/// Blocking: wrap in `spawn_blocking` from async code.
///
/// # Errors
///
/// Any I/O failure but a file too short to hold a header, which reads as a program.
pub(super) fn is_program(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    let verdict = match file.read_exact(&mut magic) {
        Ok(()) if magic == ELFMAGIC => elf_is_program(&mut file),
        Ok(()) => mach_o_is_program(&mut file, magic),
        Err(error) => Err(error),
    };
    match verdict {
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(true),
        verdict => verdict,
    }
}

fn elf_is_program(file: &mut File) -> std::io::Result<bool> {
    let fail_open = |error: ParseError| match error {
        ParseError::IOError(error) => Err(error),
        _ => Ok(true),
    };
    let object = match ElfStream::<AnyEndian, _>::open_stream(&mut *file) {
        Ok(object) => object,
        Err(error) => return fail_open(error),
    };

    let file_type = object.ehdr.e_type;
    let segments = object.segments();
    if !matches!(file_type, ET_EXEC | ET_DYN) || !segments.iter().any(|header| header.p_type == PT_LOAD) {
        return Ok(false);
    }
    if file_type == ET_EXEC {
        return Ok(true);
    }
    let has_interpreter = segments.iter().any(|header| header.p_type == PT_INTERP);
    let dynamic_segment = segments
        .iter()
        .find(|header| header.p_type == PT_DYNAMIC)
        .map(|header| (header.p_offset, header.p_filesz));
    let (endianness, class) = (object.ehdr.endianness, object.ehdr.class);
    drop(object);

    // `ElfStream::dynamic` looks only at section headers once any exist, so a loadable file
    // whose section table lacks SHT_DYNAMIC would lose DF_1_PIE and DT_DEBUG; the loader
    // reads PT_DYNAMIC, and so does this.
    let Some((offset, size)) = dynamic_segment else {
        return Ok(has_interpreter);
    };
    if size > DYNAMIC_SEGMENT_MAX_BYTES {
        return Ok(true);
    }
    let mut bytes = vec![0u8; size as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes)?;

    let table = DynamicTable::<AnyEndian>::new(endianness, class, &bytes);
    let (mut position_independent, mut has_soname, mut has_debug, mut terminated) = (false, false, false, false);
    for index in 0..table.len() {
        let Ok(entry) = table.get(index) else {
            return Ok(true);
        };
        match entry.d_tag {
            DT_NULL => {
                terminated = true;
                break;
            }
            DT_FLAGS_1 => position_independent |= entry.d_val() & DF_1_PIE as u64 != 0,
            DT_SONAME => has_soname = true,
            DT_DEBUG => has_debug = true,
            _ => {}
        }
    }
    // A table cut mid-entry or before its DT_NULL is malformed, not evidence of a library.
    if !terminated {
        return Ok(true);
    }

    Ok(position_independent || (!has_soname && (has_interpreter || has_debug)))
}

/// Judges a thin Mach-O file or the first slice of a fat one; `true` for anything unrecognised.
fn mach_o_is_program(file: &mut File, magic: [u8; 4]) -> std::io::Result<bool> {
    let (slice_magic, slice_offset) = if magic == [0xca, 0xfe, 0xba, 0xbe] {
        file.seek(SeekFrom::Start(4))?;
        let slice_count = read_u32(file, true)?;
        if !(1..=FAT_SLICE_MAX).contains(&slice_count) {
            return Ok(true);
        }
        // The first `fat_arch` entry follows cputype and cpusubtype with its slice offset.
        file.seek(SeekFrom::Start(16))?;
        let slice_offset = u64::from(read_u32(file, true)?);
        file.seek(SeekFrom::Start(slice_offset))?;
        let mut slice_magic = [0u8; 4];
        file.read_exact(&mut slice_magic)?;
        (slice_magic, slice_offset)
    } else {
        (magic, 0)
    };
    let file_type = thin_mach_o_file_type(file, slice_magic, slice_offset)?;
    Ok(file_type.is_none_or(|file_type| file_type == MACH_O_EXECUTE))
}

/// The `filetype` of a thin Mach-O header starting at `base`, or `None` when `magic` is not one.
///
/// A header cut short surfaces as `UnexpectedEof`, which `is_program` keeps.
fn thin_mach_o_file_type(file: &mut File, magic: [u8; 4], base: u64) -> std::io::Result<Option<u32>> {
    let (big_endian, is_64_bit) = match magic {
        [0xce, 0xfa, 0xed, 0xfe] => (false, false),
        [0xcf, 0xfa, 0xed, 0xfe] => (false, true),
        [0xfe, 0xed, 0xfa, 0xce] => (true, false),
        [0xfe, 0xed, 0xfa, 0xcf] => (true, true),
        _ => return Ok(None),
    };
    let header_bytes = if is_64_bit {
        MACH_O_HEADER_BYTES_64
    } else {
        MACH_O_HEADER_BYTES_32
    };
    let mut header = vec![0u8; header_bytes];
    file.seek(SeekFrom::Start(base))?;
    file.read_exact(&mut header)?;
    let filetype = [
        header[MACH_O_FILETYPE_OFFSET],
        header[MACH_O_FILETYPE_OFFSET + 1],
        header[MACH_O_FILETYPE_OFFSET + 2],
        header[MACH_O_FILETYPE_OFFSET + 3],
    ];
    Ok(Some(if big_endian {
        u32::from_be_bytes(filetype)
    } else {
        u32::from_le_bytes(filetype)
    }))
}

fn read_u32(file: &mut File, big_endian: bool) -> std::io::Result<u32> {
    let mut bytes = [0u8; 4];
    file.read_exact(&mut bytes)?;
    Ok(if big_endian {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MH_OBJECT: u32 = 1;
    const MH_EXECUTE: u32 = 2;
    const MH_DYLIB: u32 = 6;
    const MH_DYLINKER: u32 = 7;
    const MH_BUNDLE: u32 = 8;

    /// A thin Mach-O header: `filetype` sits at offset 12 in the file's own byte order.
    fn thin_macho(magic_le: [u8; 4], big_endian: bool, filetype: u32) -> Vec<u8> {
        let mut bytes = if big_endian {
            let mut reversed = magic_le;
            reversed.reverse();
            reversed.to_vec()
        } else {
            magic_le.to_vec()
        };
        bytes.extend_from_slice(&[0u8; 8]); // cputype + cpusubtype
        let filetype_bytes = if big_endian {
            filetype.to_be_bytes()
        } else {
            filetype.to_le_bytes()
        };
        bytes.extend_from_slice(&filetype_bytes);
        bytes.resize(32, 0);
        bytes
    }

    const MAGIC_64: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];
    const MAGIC_32: [u8; 4] = [0xce, 0xfa, 0xed, 0xfe];

    /// A fat header with `nfat_arch` entries; the first slice is `slice`, placed right after the table.
    fn fat_macho(magic: [u8; 4], nfat_arch: u32, slice: &[u8]) -> Vec<u8> {
        let table_end = 8 + 20 * nfat_arch.max(1);
        let mut bytes = magic.to_vec();
        bytes.extend_from_slice(&nfat_arch.to_be_bytes());
        for index in 0..nfat_arch.max(1) {
            bytes.extend_from_slice(&[0u8; 8]); // cputype + cpusubtype
            let offset = if index == 0 { table_end } else { 0 };
            bytes.extend_from_slice(&offset.to_be_bytes());
            bytes.extend_from_slice(&(slice.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&[0u8; 4]); // align
        }
        bytes.extend_from_slice(slice);
        bytes
    }

    fn verdict(bytes: &[u8]) -> bool {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file");
        std::fs::write(&path, bytes).expect("write fixture");
        is_program(&path).expect("fixture reads")
    }

    #[test]
    fn thin_64_bit_execute_is_a_program() {
        assert!(verdict(&thin_macho(MAGIC_64, false, MH_EXECUTE)));
    }

    #[test]
    fn thin_32_bit_and_byte_swapped_execute_are_programs() {
        assert!(verdict(&thin_macho(MAGIC_32, false, MH_EXECUTE)));
        assert!(verdict(&thin_macho(MAGIC_64, true, MH_EXECUTE)));
    }

    #[test]
    fn dylib_bundle_dylinker_and_object_are_not_programs() {
        for filetype in [MH_DYLIB, MH_BUNDLE, MH_DYLINKER, MH_OBJECT] {
            assert!(
                !verdict(&thin_macho(MAGIC_64, false, filetype)),
                "filetype {filetype} must be dropped"
            );
        }
    }

    #[test]
    fn thin_32_bit_and_byte_swapped_dylibs_are_not_programs() {
        for (magic, big_endian) in [(MAGIC_32, false), (MAGIC_64, true), (MAGIC_32, true)] {
            assert!(
                !verdict(&thin_macho(magic, big_endian, MH_DYLIB)),
                "dylib with magic {magic:02x?}, big_endian {big_endian} must be dropped"
            );
        }
    }

    #[test]
    fn fat_binary_is_judged_by_its_first_slice() {
        let fat_magic = [0xca, 0xfe, 0xba, 0xbe];
        assert!(
            !verdict(&fat_macho(fat_magic, 2, &thin_macho(MAGIC_64, false, MH_DYLIB))),
            "a fat dylib is a library"
        );
        assert!(
            verdict(&fat_macho(fat_magic, 2, &thin_macho(MAGIC_64, false, MH_EXECUTE))),
            "control: a fat executable is a program"
        );
    }

    #[test]
    fn fat_slice_count_outside_the_window_is_not_a_fat_binary() {
        let fat_magic = [0xca, 0xfe, 0xba, 0xbe];
        let dylib = thin_macho(MAGIC_64, false, MH_DYLIB);
        assert!(
            !verdict(&fat_macho(fat_magic, FAT_SLICE_MAX, &dylib)),
            "control: the top of the window is still judged as fat"
        );
        // Java class files share the magic and carry their major version (52) in this field.
        for slice_count in [0, FAT_SLICE_MAX + 1, 52] {
            assert!(
                verdict(&fat_macho(fat_magic, slice_count, &dylib)),
                "slice count {slice_count} is outside the fat window and must be kept"
            );
        }
    }

    #[test]
    fn fat64_magic_is_unknown_and_kept() {
        let fat64 = fat_macho([0xca, 0xfe, 0xba, 0xbf], 1, &thin_macho(MAGIC_64, false, MH_DYLIB));
        assert!(verdict(&fat64));
    }

    #[test]
    fn scripts_truncated_and_empty_files_are_kept() {
        assert!(verdict(b"#!/bin/sh\necho hi\n"));
        assert!(verdict(&[0xcf, 0xfa]));
        assert!(verdict(&[]));
    }

    #[test]
    fn a_thin_mach_o_header_cut_short_is_kept() {
        let dylib_64 = thin_macho(MAGIC_64, false, MH_DYLIB);
        assert!(
            !verdict(&dylib_64),
            "control: the complete header is judged by its filetype"
        );
        assert!(verdict(&dylib_64[..16]), "cut right after filetype");
        assert!(verdict(&dylib_64[..31]), "cut one byte before the 64-bit header ends");
        let dylib_32 = thin_macho(MAGIC_32, false, MH_DYLIB);
        assert!(!verdict(&dylib_32[..28]), "a 32-bit header is complete at 28 bytes");
        assert!(verdict(&dylib_32[..27]), "cut one byte before the 32-bit header ends");
    }

    #[test]
    fn a_truncated_elf_is_kept() {
        assert!(verdict(b"\x7fELF\x02\x01\x01"));
    }

    #[test]
    fn an_unreadable_path_propagates_the_io_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = is_program(&dir.path().join("missing")).expect_err("a missing file is an I/O error");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
