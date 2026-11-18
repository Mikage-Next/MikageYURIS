# Enigma Virtual Box packages

Code: `crates/yu-vfs/src/evb.rs` (from ethornell-rs by way of MikageYURIS-rs,
MPL-2.0).
Translation patches often ship the original exe wrapped with Enigma Virtual
Box (EVB). When the exe runs, the packed files appear in its folder ahead of
the real ones. Erewhon's Chinese patch is such an exe; its packed files are
the Chinese scripts (`pac\bn.ypf`, YPF v490), translated UI images
(`update9.ypf`), a first-run `save\config.sd`, VSFilter and subtitles.

## Where the files are

The packed files are either inside the exe, in its `.enigma1` PE section, or
in an external package that the exe names. Erewhon's exe names
`%DEFAULT FOLDER%\erewhon_chs.mys` as a UTF-16 string just before its own
table; the string is not NUL-terminated and runs into the `EVB\0` magic.
Both forms use the same table, which starts with `EVB\0`.

## Table

All integers are little-endian; names are UTF-16LE.

```text
u8  magic[4] = "EVB\0"
u8  header[60]
main node                 // at 64: u32 size, 8 unknown bytes, u32 top-level object count
node ...                  // pre-order, from offset 79
```

Each node:

```text
u8  head[12]
u32 objects               // children of a folder
u16 name[]                // NUL-terminated
u8  kind                  // 2 file, 3 folder
file:   53 bytes: u32 size at +2, u32 stored size at +49
folder: 25 bytes
```

A top-level folder named `%DEFAULT FOLDER%` is the exe's own folder. Files'
data follows the table in table order, starting at `table + 80 + main.size
- 12`. A file whose stored size is smaller than its size is compressed: a
chunk table (u32 table size, 4 unknown bytes, then 12 bytes per chunk whose
first u32 is the chunk's packed size), then the aPLib-packed chunks. Back
references in a chunk may reach into earlier chunks.

## How the runtime uses it

`Project::set_launcher` (`crates/yu-engine/src/project.rs`) mounts the packed
files of the exe and of the external package it names. A packed `.ypf` stored
as is replaces the real pack at the same path, or joins the packs
(`update1..9.ypf` as update layers); it is read in place with
`YpfSource::open_range`. Other packed files form one source just above the
loose files (`EvbSource`). Packed `save/` files are left out: they are the
translator's own saves and settings, and Erewhon's `save/config.sd` would
skip the first-run setup that places the message window.

Erewhon's patch also stores its translated UI pictures as YCG
(`crates/yu-format/src/ycg.rs`) under `.png` names; pictures are recognised by
their bytes.

Hosts: the iOS interface takes the exe as `YurisConfig.launcher`; the
`drive` example (`crates/yu-engine/examples/drive.rs`) has `--launcher`.
