# am-hook-joc

English | [简体中文](README.zh-CN.md)

E-AC-3 JOC (Dolby Atmos) object decoding and stereo rendering for
[am-hook](https://github.com/itouakirai/am-hook), written in Rust and compiled to
WebAssembly.

The player keeps using am-hook's own EC-3 WASM decoder for the core PCM, reads the
JOC metadata from the same syncframes, rebuilds 15 objects plus LFE and renders
them to stereo in one worker.

## Features

* JOC / EMDF / ID14 / ID11 (OAMD) parsing, including the bed plan and object
  positions.
* 15-object plus LFE reconstruction with 64-band QMF analysis and synthesis, matrix
  interpolation, LFE delay and the Ls/Rs phase shift.
* **Speaker 2.0** — equal-power panning to two loudspeakers with floor/height
  attenuation.
* **Binaural** — 77-band hybrid filterbank, order-5 spherical-harmonic field, six
  image sources and a shared FDN room, driven by a compiled `.jochrtf` HRTF field;
  the SADIE II subjects D1, D2 and H3-H20 are selectable in the settings.
* Output attenuation (default **0 dB**); when clipping occurs, -4 dB or lower
  is recommended and the player bar shows a warning.
* Seek with a 64-syncframe pre-roll, album/queue playback that prefers Atmos while
  the mode is on, and a settings section (Chinese and English).
* Everything runs in the browser: audio is never uploaded, decoding and rendering
  stay in a Web Worker; the main thread only schedules AudioBuffers.

## Requirements

* Rust with the `wasm32-unknown-unknown` target
* Node.js 20+ (for `wasm-opt` via `npx`, and for the browser tests)
* A clean am-hook checkout of the upstream release that matches the patch;
  playback also needs its usual back end (wrapper-lite)

## Build

```sh
git clone https://github.com/itouakirai/am-hook.git
cd am-hook && git checkout <upstream-tag>

# from this repository:
bash scripts/apply.sh ../am-hook --tag <upstream-tag>   # apply the matching patch
bash scripts/build.sh ../am-hook                        # build joc.wasm + am-hook
```

## HRTF

Binaural rendering uses fields compiled from the public
[SADIE II database](https://www.york.ac.uk/sadie-project/database.html) of the
University of York (Apache-2.0). The build ships the subjects **D1, D2 and
H3-H20** as `.jochrtf` files in `hrtf/`; **Settings → JOC object rendering**
switches between them (D2 is the default). The SADIE site describes each subject,
so users can pick the one that matches their own head and ears.

`tools/compile_hrtf.py` produces the fields from the SOFA files (minimum-phase
conversion, delay kept in `Data.Delay`, SOFA metadata preserved), and
`scripts/build.sh` installs them into the patched checkout.

## Run

```sh
../am-hook/target/release/am-hook --listen 127.0.0.1:8888 \
    --wrapper-url http://127.0.0.1:12340
```

Open <http://127.0.0.1:8888>, go to **Settings → JOC object rendering**, choose
*Speaker 2.0* or *Binaural*, then play a Dolby Atmos track. The settings page has a
shortcut to this section in its top bar.

## Repository layout

```
patches/                 one patch per upstream release
hrtf/                    compiled SADIE II HRTF fields
joc-wasm/                Rust workspace
  crates/joc-core/       bitstream, DSP and streaming state (host and wasm)
  crates/joc-wasm/       C ABI used by the browser worker
  crates/joc-cli/        native driver
scripts/                 apply / build and helper scripts
tests/                   test tools
tools/                   table generator, C++ comparison dump
```

## License

AGPL-3.0-only, matching am-hook: the patch and the browser integration are a
derivative work of am-hook. The Rust renderer in `joc-wasm/` is additionally
available under the MIT license (`joc-wasm/LICENSE`). See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
