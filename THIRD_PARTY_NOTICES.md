# Third-party notices

## am-hook

The patch applies to [am-hook](https://github.com/itouakirai/am-hook) releases
and is distributed under the same AGPL-3.0-only license; each release has its
own patch in `patches/`. am-hook's EC-3 decoder (`src/ui/ec3.wasm`,
`src/ui/ec3-runtime.mjs`) is `@mediabunny/ac3` 1.59.1 (MPL-2.0, see
`src/ui/EC3-LICENSE.txt`), built on FFmpeg's AC-3/E-AC-3 decoder.

## Rust crates

`joc-core` depends on `flate2` (MIT/Apache-2.0, `miniz_oxide` backend) for the
ZIP/deflate members of `.jochrtf`, and `sha2` (MIT/Apache-2.0) for the payload
digest. `joc-wasm` and `joc-cli` have no other dependencies.

## SADIE II HRTF

Binaural builds embed compiled `.jochrtf` fields derived from the SADIE II
database of the AudioLab, University of York
(<https://www.york.ac.uk/sadie-project/database.html>): the D1, D2 and
H3-H20 sets, licensed under the Apache License 2.0
(<https://www.apache.org/licenses/LICENSE-2.0>). The compiled fields remain
under the dataset's licence.
