# every-git-commit-shorthash

A git repository with a commit for *every* seven-character git commit shorthash (all 2<sup>28</sup> of them).

If you have a commit shorthash, or any seven-character hex string, you can find a commit for it here! It's like a dictionary, but much less useful. Also see [lucky-commit](https://github.com/not-an-aardvark/lucky-commit) if you'd like to generate commits with arbitrary shorthashes on the fly instead.

## FAQ

### Where can I see all the commits?

This repository contains code for generating a repository with every shorthash locally.

The repository has so many commits that `git push` hangs and runs out of memory, presumably because it tries to regenerate a packfile on the fly. As a result, there isn't a GitHub-hosted interactive demo. Sorry.

This problem might be tractable in theory. `git` uses the same packfile format for network transport as it does for storage, so it might be possible to convince it to use the packfile from the filesystem directly rather than generating a new one. However, I have some doubts that GitHub would accept the push without timing out or hitting a memory limit somewhere.

If you want to get the commits without running the tool yourself, you can download a pregenerated packfile and index by following the instructions on the [releases page](https://github.com/not-an-aardvark/every-git-commit-shorthash/releases).

### How much space does this take up?

The commits are stored in a 14.7 GB [git packfile](https://git-scm.com/docs/pack-format), as well as an associated 9.35 GB pack index file. This is the result of significant optimization to reduce the file sizes.

Specifically, two major strategies are used:

#### Almost all of the commits are stored as deltas

Git packfiles support a delta format, where a git object is stored as a diff from another git object, then reassembled at runtime. Normally, this delta format is used for file contents (i.e. blobs), so that git doesn't need to store two copies of a file for a one-line change. However, it's supported for all types of git objects, including commits themselves. Git commits contain a lot of metadata, so storing a commit as e.g. "the same as this other commit, plus one space at the end of the commit message" saves a lot of space over inlining the entire commit.

#### The commit graph is arranged to take advantage of compression

It's worth noting a design goal of the tool here: it should be possible to place all 2<sup>28</sup> commits in a single git branch, such that they won't be immediately garbage-collected by git.

This design goal requires the commit layout to use more space. Without this requirement, it would be possible to simply create 2<sup>28</sup> root commits (i.e. commits without parents). However, with this requirement, for each non-branch-tip commit there needs to be at least one child commit whose body contains `parent <40-character hex commit hash>` in the commit metadata. Since all of the 40-character commit hashes are different, this requires a minimum storage cost of of 40 uncompressed bytes per commit, regardless of how the commits are arranged. There's also a flat cost of 20 bytes per commit to use deltas in the first place (since they require a reference to a 20-byte OID as the delta base), along with a few bytes of overhead to specify the delta itself.

With that aside, there is still a significant amount of flexibility in how the commits can be arranged. For example, it would be possible to create a linear commit history of 2<sup>28</sup> commits, or to create 2<sup>28</sup> root commits and one massive merge commit with 2<sup>28</sup> parents, or anything in between.

Emperically, using a linear commit history resulted in an amortized size of 73 compressed bytes per commit, whereas using big merge commits resulted an amortized size of only 46 compressed bytes per commit. This is because giant merge commits are much more easily compressible, since they consist almost entirely of hex characters from the parent hashes.

I suspected git wouldn't really like having a single merge commit with 2<sup>28</sup> parents, so I compromised and created 2<sup>14</sup> merge commits, each with 2<sup>14</sup> root-commit parents, and one top-level merge commit with each of the 2<sup>14</sup> merge commits as parents. It's worth noting that while the merge commits account for 0.0061% of the total number of commits, they account for about 46% of the total storage space. I think the total size is close to optimal given the requirement that all the commits be reachable, but removing that requirement would result in almost a 50% packfile size reduction.

### How long does it take to generate the commits?

About 5 hours on my laptop (a 2015 MacBook Pro).

The tool was designed for one-time use, so I haven't spent a lot of time optimizing its performance. There is some significant low-hanging fruit:

* Currently, it's single-threaded for simplicity. The performance could be sped up by a significant factor by using multiple cores or running on a GPU.
* Generating the index file currently involves a lot of cache thrashing, which could be fixed with only a bit of added complexity.

When the tool starts running, the main bottleneck is zlib compression (which is run on each commit, using the maximum compression ratio). This continues to be the main bottleneck until the tool reaches the last million commits or so, at which point SHA1 throughput becomes the main bottleneck. (For each of the last few commits, the tool has to try a large number of commit possibilities in order to find a shorthash that hasn't already been used.)

Note that the tool is currently very memory-constrained; in order to generate the packfile index, the tool needs to keep track of a sorted index of all of the commit hashes generated so far. As a result of this and a few other pieces of metadata, it uses 11GB of memory, which is just small enough to run on my laptop. Some plausible-seeming performance improvements would result in OOM, and some memory usage improvements (e.g. saving state to the filesystem) could result in slower performance.

### How many commits does the tool need to go through to find 2<sup>28</sup> unique shorthashes?

Due to the [coupon collector's problem](https://en.wikipedia.org/wiki/Coupon_collector%27s_problem), the expected number of commit attempts is 2<sup>28</sup>(ln(2<sup>28</sup>) + 0.577...), or about 5.4 billion.

### Does git actually work in such a big repository?

Sort of. You can view any particular commit and check out files at that commit. (You can run `git checkout` and keyboard-mash seven random hex characters, and it will go to that commit, which is neat.) Anything that requires stepping through history in order, such as `git log`, seems to stall and run out of memory.

### Why would someone want to use this?

¯\\\_(ツ)\_/¯

### No, really, is it useful for anything?

Probably not, but maybe.

I originally created [lucky-commit](https://github.com/not-an-aardvark/lucky-commit), the companion project to this one, as a practical joke. However, it turned out to be [unexpectedly useful for security research](https://blog.teddykatz.com/2019/11/12/github-actions-dos.html) as a way to generate targeted commit hash collisions.

In theory, this project could also be used to [generate commit hash collisions in bulk](https://blog.teddykatz.com/2019/11/12/github-actions-dos.html#:~:text=Making%20every%20shorthash%20collide). However, it seems like using `lucky-commit` to generate a targeted collision would be more useful in almost all circumstances, especially since `lucky-commit` allows you to amend your own commits rather than creating useless commits from scratch. This is particularly true because the branch with bulk commits from this project can't really be pushed anywhere.

### How do I run it?

First, [ensure you have `rustc` and `cargo` installed](https://www.rust-lang.org/tools/install).

Then run:

```bash
$ git clone https://github.com/not-an-aardvark/every-git-commit-shorthash.git
$ cd every-git-commit-shorthash
```

Optionally, update the hardcoded commit templates in `src/main.rs`, e.g. to update the author to yourself or change the commit message.

Then run:

```bash
$ cargo run --release
```
