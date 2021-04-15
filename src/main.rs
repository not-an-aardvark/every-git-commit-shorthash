mod git_utils;

use git_utils::{
    stream_to_pack_file, GitObject, GitObjectType, PackFileDeltaInstruction, PackFileObject,
    Sha1Oid,
};
use sha1::{Digest, Sha1};
use std::{convert::TryInto, iter::FusedIterator, mem::forget};

#[repr(transparent)]
struct PackedBoolArray {
    data: Vec<u8>,
}

impl PackedBoolArray {
    const MASKS: [u8; 8] = [1, 2, 4, 8, 16, 32, 64, 128];

    #[inline(always)]
    fn offset(index: usize) -> usize {
        index / 8
    }

    #[inline(always)]
    fn mask(index: usize) -> u8 {
        Self::MASKS[index % 8]
    }

    fn get(&self, index: usize) -> bool {
        self.data[Self::offset(index)] & Self::mask(index) != 0
    }

    fn set(&mut self, index: usize, value: bool) {
        if value {
            self.data[Self::offset(index)] |= Self::mask(index);
        } else {
            self.data[Self::offset(index)] &= !Self::mask(index);
        }
    }

    #[inline(always)]
    fn hash_to_shorthash_index(hash: &Sha1Oid) -> usize {
        (u32::from_be_bytes(hash[0..4].try_into().unwrap()) >> 4) as usize
    }
}

impl Default for PackedBoolArray {
    fn default() -> Self {
        Self { data: vec![0; 1 << 25] }
    }
}

fn main() -> std::io::Result<()> {
    let empty_tree = GitObject {
        object_type: GitObjectType::Tree,
        data: vec![],
    };
    let delta_base_commit = GitObject {
        object_type: GitObjectType::Commit,
        data: b"\
            tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
            author Teddy Katz <teddy.katz@gmail.com> 1616279625 -0400\n\
            committer Teddy Katz <teddy.katz@gmail.com> 1616279625 -0400\n\
            \n\
            Entropy value for this commit: "
            .to_vec(),
    };
    let last_block_length = (delta_base_commit.data.len()
        + format!("commit {}\0", delta_base_commit.data.len()).len())
        % 64;
    assert!(
        (0..=47).contains(&last_block_length),
        "suboptimal commit length {}; hashing would be twice as slow",
        last_block_length
    );
    let mut found_shorthashes = PackedBoolArray::default();
    let delta_base_commit_oid = delta_base_commit.oid();
    found_shorthashes.set(
        PackedBoolArray::hash_to_shorthash_index(&delta_base_commit_oid),
        true,
    );

    let deltified_generator = DeltifiedCommitGenerator {
        delta_base_commit: delta_base_commit.clone(),
        delta_base_commit_oid,
        found_shorthashes,
        root_commit_oid_buffer: vec![delta_base_commit_oid],
        merge_commit_oid_buffer: vec![],
        delta_base_commit_extension_length: 8,
        delta_base_commit_intermediate_sha1_state: Sha1::new()
            .chain(format!("commit {}\0", delta_base_commit.data.len() + 8).as_bytes())
            .chain(&delta_base_commit.data),
        entropy_specifier: 0,
        commit_count_cap: usize::MAX,
        is_finished: false,
    };

    let pack_file = stream_to_pack_file(
        vec![
            PackFileObject::Raw(empty_tree),
            PackFileObject::Raw(delta_base_commit),
        ]
        .into_iter()
        .chain(deltified_generator),
    )?;

    // Avoid running the destructor for the metadata, since it takes a very long time to clean up and
    // we're about to exit the process anyway.
    forget(pack_file);

    Ok(())
}

struct DeltifiedCommitGenerator {
    delta_base_commit: GitObject,
    delta_base_commit_oid: Sha1Oid,
    found_shorthashes: PackedBoolArray,
    root_commit_oid_buffer: Vec<Sha1Oid>,
    merge_commit_oid_buffer: Vec<Sha1Oid>,
    delta_base_commit_extension_length: usize,
    delta_base_commit_intermediate_sha1_state: Sha1,

    // Due to https://en.wikipedia.org/wiki/Coupon_collector%27s_problem, we expect to need
    // 2**28 * (ln(2**28) + 0.577) = 2**32.3 total commits to find all 2**28 unique shorthashes,
    // which is over the threshold of 2**32 32-bit ints.
    entropy_specifier: u64,
    commit_count_cap: usize,
    is_finished: bool,
}

impl DeltifiedCommitGenerator {
    fn get_entropy(&self) -> String {
        if self.delta_base_commit_extension_length == 8 {
            format!("{:08x}", self.entropy_specifier)
        } else {
            format!("{:016x}", self.entropy_specifier)
        }
    }
}

fn create_merge_commit(parent_oids: &[Sha1Oid]) -> GitObject {
    GitObject {
        object_type: GitObjectType::Commit,
        data: format!(
            "\
                tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
                {}\
                author Teddy Katz <teddy.katz@gmail.com> 1616279625 -0400\n\
                committer Teddy Katz <teddy.katz@gmail.com> 1616279625 -0400\n\
                \n\
                Merge of {} commits\n",
            parent_oids
                .iter()
                .map(|oid| {
                    format!(
                        "parent {}\n",
                        oid.iter()
                            .map(|&byte| format!("{:02x}", byte))
                            .collect::<String>()
                    )
                })
                .collect::<String>(),
            parent_oids.len()
        )
        .as_bytes()
        .to_vec(),
    }
}

impl Iterator for DeltifiedCommitGenerator {
    type Item = PackFileObject;
    fn next(&mut self) -> Option<Self::Item> {
        if self.is_finished
            || self.merge_commit_oid_buffer.len() * (1 << 14) + self.root_commit_oid_buffer.len()
                > self.commit_count_cap
        {
            return None;
        }

        if self.merge_commit_oid_buffer.len() >= 1 << 14 {
            self.is_finished = true;
            let final_merge = create_merge_commit(&self.merge_commit_oid_buffer);
            println!(
                "Top-level merge commit: {}",
                final_merge
                    .oid()
                    .iter()
                    .map(|&byte| format!("{:02x}", byte))
                    .collect::<String>()
            );
            println!("Your call is important to us.");
            println!("Please hold while an index file is generated. This will take a while");
            return Some(PackFileObject::Raw(final_merge));
        }

        if self.root_commit_oid_buffer.len() >= 1 << 14 {
            let merge = create_merge_commit(&self.root_commit_oid_buffer);
            self.root_commit_oid_buffer.clear();
            self.merge_commit_oid_buffer.push(merge.oid());
            println!(
                "created first-level merge commit {}/{}",
                self.merge_commit_oid_buffer.len(),
                1 << 14
            );
            return Some(PackFileObject::Raw(merge));
        }

        let new_oid = loop {
            if self.entropy_specifier == (u32::MAX as u64) + 1 {
                self.delta_base_commit_extension_length = 16;
                self.delta_base_commit_intermediate_sha1_state = Sha1::new()
                    .chain(
                        format!(
                            "commit {}\0",
                            self.delta_base_commit.data.len()
                                + self.delta_base_commit_extension_length
                        )
                        .as_bytes(),
                    )
                    .chain(&self.delta_base_commit.data);
            }

            let oid = self
                .delta_base_commit_intermediate_sha1_state
                .clone()
                .chain(self.get_entropy().as_bytes())
                .finalize()
                .into();

            if !self
                .found_shorthashes
                .get(PackedBoolArray::hash_to_shorthash_index(&oid))
            {
                break oid;
            }

            self.entropy_specifier += 1;
            if self.entropy_specifier & 0xfffff == 0 {
                println!("number of commits attempted so far: {}", self.entropy_specifier);
            }
        };

        let delta_instructions = vec![
            PackFileDeltaInstruction::CopyFromBaseObject {
                offset: 0,
                size: self.delta_base_commit.data.len(),
            },
            PackFileDeltaInstruction::AddNewData(self.get_entropy().as_bytes().to_vec()),
        ];

        self.found_shorthashes
            .set(PackedBoolArray::hash_to_shorthash_index(&new_oid), true);
        self.entropy_specifier += 1;
        if self.entropy_specifier & 0xfffff == 0 {
            println!("number of commits attempted so far: {}", self.entropy_specifier);
        }
        self.root_commit_oid_buffer.push(new_oid);

        Some(PackFileObject::Deltified {
            base_oid: self.delta_base_commit_oid,
            base_size: self.delta_base_commit.data.len(),
            delta: delta_instructions,
            new_oid,
            new_size: self.delta_base_commit.data.len() + self.delta_base_commit_extension_length,
        })
    }
}

impl FusedIterator for DeltifiedCommitGenerator {}
