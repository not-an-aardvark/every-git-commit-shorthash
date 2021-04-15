use crc::crc32;
use flate2::{write::ZlibEncoder, Compression};
use sha1::{Digest, Sha1};
use std::{
    collections::BTreeMap,
    io,
    io::{copy, Seek, SeekFrom, Write},
    num::NonZeroU8,
};
use std::{
    fs::OpenOptions,
    io::BufWriter,
};

pub type Sha1Oid = [u8; 20];

#[derive(Clone, Debug)]
pub enum GitObjectType {
    Commit,
    Tree,
    #[allow(dead_code)]
    Blob,
}

#[derive(Clone, Debug)]
pub struct GitObject {
    pub data: Vec<u8>,
    pub object_type: GitObjectType,
}

impl GitObjectType {
    fn type_name(&self) -> &'static str {
        match self {
            GitObjectType::Commit { .. } => "commit",
            GitObjectType::Tree { .. } => "tree",
            GitObjectType::Blob { .. } => "blob",
        }
    }
}

impl GitObject {
    pub fn oid(&self) -> Sha1Oid {
        Sha1::new()
            .chain(format!("{} {}\0", self.object_type.type_name(), self.data.len()).as_bytes())
            .chain(&self.data)
            .finalize()
            .into()
    }
}

#[derive(Debug)]
pub enum PackFileDeltaInstruction {
    CopyFromBaseObject { offset: usize, size: usize },
    AddNewData(Vec<u8>),
}

#[derive(Debug)]
pub enum PackFileObject {
    Raw(GitObject),
    Deltified {
        base_oid: Sha1Oid,
        base_size: usize,
        delta: Vec<PackFileDeltaInstruction>,
        new_oid: Sha1Oid,
        new_size: usize,
    },
}

impl PackFileObject {
    pub fn oid(&self) -> Sha1Oid {
        match self {
            Self::Raw(git_object) => git_object.oid(),
            Self::Deltified { new_oid, .. } => *new_oid,
        }
    }
}

#[derive(Debug)]
pub struct PackFile {
    object_positions: BTreeMap<Sha1Oid, (usize, u32)>,
}

/// Generates a git packfile and index file containing the given git objects.
/// The git packfile format is mostly specified [here](https://git-scm.com/docs/pack-format). In a few places
/// noted below, the format documentation is underspecified; this generator is implemented based on a combination
/// of the documented behavior, testing with git itself, and reading the git source code to figure out what it
/// actually accepts.
pub fn stream_to_pack_file<T: IntoIterator<Item = PackFileObject>>(
    iter: T,
) -> io::Result<PackFile> {
    let mut pack = BufWriter::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(".git/objects/pack/pack-every-shorthash.pack")?,
    );

    // --- Start of packfile header ---
    // 4-byte signature
    pack.write_all("PACK".as_bytes())?;

    // 4-byte version number
    pack.write_all(&2u32.to_be_bytes())?;

    // 4-byte number of objects (currently initialized to 0; will be filled in afterwards)
    pack.write_all(&[0, 0, 0, 0])?;

    // --- End of packfile header ---

    let mut current_position = 12;
    let mut object_positions = BTreeMap::new();
    let mut object_counts_by_first_byte = [0u32; 256];
    let mut current_object = Vec::new();

    for object in iter {
        let oid = object.oid();
        let current_object_position = current_position;

        // Object type, using the ID values [here](https://git-scm.com/docs/pack-format#:~:text=Object%20types)
        let object_type: u8 = match &object {
            PackFileObject::Raw(GitObject { object_type, .. }) => match object_type {
                GitObjectType::Commit => 1,
                GitObjectType::Tree => 2,
                GitObjectType::Blob => 3,
            },
            PackFileObject::Deltified { base_oid, .. } => {
                if object_positions.contains_key(base_oid) {
                    6
                } else {
                    7
                }
            }
        };

        let encoded_object = match &object {
            // Non-deltified objects have no packfile-specific encoding.
            PackFileObject::Raw(git_object) => git_object.data.clone(),
            PackFileObject::Deltified {
                base_size,
                delta,
                new_size,
                ..
            } => {
                // Deltified objects use the packfile-specific encoding described
                // [here](https://git-scm.com/docs/pack-format#_deltified_representation).
                let mut deltified_representation = Vec::new();
                append_variable_length_size(&mut deltified_representation, *base_size)?;
                append_variable_length_size(&mut deltified_representation, *new_size)?;
                for delta_instruction in delta {
                    match delta_instruction {
                        PackFileDeltaInstruction::CopyFromBaseObject { offset, size } => {
                            // The "copy from base object" instruction encoding, documented
                            // [here](https://git-scm.com/docs/pack-format#_instruction_to_copy_from_base_object)
                            let offset1 = NonZeroU8::new(*offset as u8);
                            let offset2 = NonZeroU8::new((*offset >> 8) as u8);
                            let offset3 = NonZeroU8::new((*offset >> 16) as u8);
                            let offset4 = NonZeroU8::new((*offset >> 24) as u8);
                            let size1 = NonZeroU8::new(*size as u8);
                            let size2 = NonZeroU8::new((*size >> 8) as u8);
                            let size3 = NonZeroU8::new((*size >> 16) as u8);
                            deltified_representation.push(
                                0b1000_0000
                                    | if size3.is_some() { 0b0100_0000 } else { 0 }
                                    | if size2.is_some() { 0b0010_0000 } else { 0 }
                                    | if size1.is_some() { 0b0001_0000 } else { 0 }
                                    | if offset4.is_some() { 0b0000_1000 } else { 0 }
                                    | if offset3.is_some() { 0b0000_0100 } else { 0 }
                                    | if offset2.is_some() { 0b0000_0010 } else { 0 }
                                    | if offset1.is_some() { 0b0000_0001 } else { 0 },
                            );
                            deltified_representation.extend(
                                vec![offset1, offset2, offset3, offset4, size1, size2, size3]
                                    .into_iter()
                                    .filter_map(|v| v)
                                    .map(NonZeroU8::get),
                            );
                        }
                        PackFileDeltaInstruction::AddNewData(new_data) => {
                            // The "add new data" instruction encoding, documented
                            // [here](https://git-scm.com/docs/pack-format#_instruction_to_add_new_data).
                            // FIXME: is the length limit for this instruction actually 127?
                            // It seems like it would be impossible to encode a length more than 127 with
                            // the documented format, but that seems surprising. Maybe it's supposed to use the
                            // variable-length encoding described in other places?
                            // In any case, this tool only uses the instruction with sizes less than 127 anyway.
                            debug_assert!((1..=127).contains(&new_data.len()));
                            deltified_representation.push(new_data.len() as u8);
                            deltified_representation.extend(new_data);
                        }
                    }
                }
                deltified_representation
            }
        };

        // Append the object type and object size. The git pack format documentation specifies that this should
        // be "3-bit type, (n-1)*7+4-bit length", but it underspecifies how exactly these bits need to be arranged.
        // From viewing the git source code: the first byte always has a 1 as the most significant bit, followed
        // by the 3 bits of the object type, followed by the four least significant bits of the encoded object size
        // (measured before any compression is applied). Then the remaining bits of the encoded object size are appended
        // in the documented format for variable-length sizes.
        current_object.push(0x80 | (object_type << 4) | (encoded_object.len() & 0xf) as u8);
        append_variable_length_size(&mut current_object, encoded_object.len() >> 4)?;

        if let PackFileObject::Deltified { base_oid, .. } = object {
            if let Some((previous_position, _)) = object_positions.get(&base_oid) {
                // For "offset delta" objects, append the relative offset of the delta base.
                let offset = current_object_position - *previous_position;
                append_variable_length_size_with_continuation_increment(
                    &mut current_object,
                    offset,
                );
            } else {
                // For "ref delta" objects, append the OID of the delta base.
                current_object.extend(&base_oid);
            }
        }
        // Append the encoded object data, with maximum compression.
        let mut encoder = ZlibEncoder::new(&mut current_object, Compression::best());
        encoder.write_all(&encoded_object)?;
        encoder.finish()?;

        object_positions.insert(
            oid,
            (
                current_object_position,
                // The git pack format documentation specifies that the index file needs to include the CRC32
                // of each object, but doesn't specify which CRC32 table to use. Emperically, it seems like git
                // uses the IEEE CRC32 table.
                crc32::checksum_ieee(&current_object),
            ),
        );
        current_position += current_object.len();
        pack.write_all(&current_object)?;
        current_object.clear();

        object_counts_by_first_byte[oid[0] as usize] += 1;
    }

    let mut pack_file = pack.into_inner()?;

    // Now that all the objects have been added to the packfile, insert the correct object count into the header
    pack_file.seek(SeekFrom::Start(8))?;
    pack_file.write_all(&(object_positions.len() as u32).to_be_bytes())?;

    // Add the sha1 pack checksum to the end of the packfile
    pack_file.seek(SeekFrom::Start(0))?;
    let mut pack_hasher = Sha1::new();
    copy(&mut pack_file, &mut pack_hasher)?;
    let pack_checksum = pack_hasher.finalize();
    pack_file.write_all(&pack_checksum)?;

    pack_file.sync_all()?;
    drop(pack_file);

    // At this point, the packfile is complete and we're finished processing commits, but we still need to
    // generate an index file. Version-2 index files are needed because the packfile is generally bigger than
    // 2**32 bytes.
    let mut index = BufWriter::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(".git/objects/pack/pack-every-shorthash.idx")?,
    );

    // --- Start of index file header ---

    // 4-byte "magic number"
    index.write_all(b"\xfftOc")?;

    // 4-byte version number
    index.write_all(&2u32.to_be_bytes())?;

    // --- End of index file header ---

    // 256-entry "fanout table", encoding the number of objects in the packfile that start with each of
    // 0, 1, 2, ..., 255.
    let mut num_objects: u32 = 0;
    for &count_with_first_byte_equal in object_counts_by_first_byte.iter() {
        num_objects += count_with_first_byte_equal;
        index.write_all(&num_objects.to_be_bytes()[..])?;
    }

    // At this point, we need to iterate over the objects, in order of their OID, several times. Using a B-tree
    // is asymtotically optimal for this, but it results in pretty severe cache thrashing, which greatly slows down
    // generating the index file. There is a lot of room for improvement here.
    //
    // The reason we use a B-tree in the first place, rather than just accumulating a list of
    // (oid, position, checksum) tuples and sorting it afterwards, is that the current API allows an object
    // to be specified as a delta from any other object by OID, and we need to be able to fetch the position
    // of the delta base before we've obtained or sorted the whole list. This API is also intended to be generic
    // (in that it can generate packfiles of arbitrary objects, not just the objects generated in main.rs). But in
    // reality, all deltified commits that are passed to this API have the same delta base, so this issue could be
    // avoided by exposing a more specialized API.
    //
    // Another way to avoid the issue would be to only iterate over the B-tree once, and write to several different
    // parts of the file simultaneously using multiple file descriptors.

    // All of the OIDs, in lexicographic order
    for oid in object_positions.keys() {
        index.write_all(oid)?;
    }

    // CRC32 checksums of the packed object data
    for (_, checksum) in object_positions.values() {
        index.write_all(&checksum.to_be_bytes())?;
    }

    let mut num_big_offsets = 0u32;
    // Table of 4-byte object offsets
    for (position, _) in object_positions.values() {
        if *position < 0x80_00_00_00 {
            index.write_all(&(*position as u32).to_be_bytes())?;
        } else {
            index.write_all(&(0x80_00_00_00 | num_big_offsets).to_be_bytes())?;
            num_big_offsets += 1;
        }
    }

    // Table of 8-byte object offsets
    for (position, _) in object_positions.values() {
        // FIXME: might faster to have two separate cursors writing to the file rather than iterating over
        // the B-tree twice
        if *position >= 0x80_00_00_00 {
            index.write_all(&(*position as u64).to_be_bytes())?;
        }
    }

    // Add a copy of the pack file checksum
    index.write_all(&pack_checksum)?;

    let mut index_file = index.into_inner()?;

    // Add the sha1 index checksum to the index of the index file
    index_file.seek(SeekFrom::Start(0))?;
    let mut index_hasher = Sha1::new();
    copy(&mut index_file, &mut index_hasher)?;
    index_file.write_all(&index_hasher.finalize())?;

    index_file.sync_all()?;

    // Deallocating the B-tree of object positions is very, very slow. It's a really big B-tree that has lots of
    // individual allocations. Deallocating the B-tree is also completely unnecessary if the process is about to
    // exit, serving only to add hours to the runtime for no reason. So the B-tree is included as a private
    // returned struct field, and the caller can explicitly leak the struct rather than dropping it if needed.
    Ok(PackFile {
        object_positions,
    })
}

/// Appends a "size-encoded" non-negative integer to packfile data, using the
/// encoding format specified [here](https://git-scm.com/docs/pack-format#:~:text=Size%20encoding).
fn append_variable_length_size<T: Write>(mut data: T, mut size: usize) -> io::Result<()> {
    loop {
        let next_seven_bits = (size & 0x7f) as u8;
        size >>= 7;
        if size == 0 {
            data.write_all(&[next_seven_bits])?;
            break;
        } else {
            data.write_all(&[next_seven_bits | 0x80])?;
        }
    }
    Ok(())
}

/// Packfiles use a slightly different variable-length size encoding for delta offsets
/// than they do for other values. This modified encoding is entirely undocumented and also necessary
/// to generate a packfile that git will understand.
/// [This blogpost](https://medium.com/@concertdaw/sneaky-git-number-encoding-ddcc5db5329f) contains
/// some more information.
fn append_variable_length_size_with_continuation_increment(data: &mut Vec<u8>, mut size: usize) {
    let initial_index = data.len();
    data.push((size & 0x7f) as u8);
    size >>= 7;
    while size > 0 {
        size -= 1;
        data.insert(initial_index, 0x80 | (size as u8 & 0x7f));
        size >>= 7;
    }
}
