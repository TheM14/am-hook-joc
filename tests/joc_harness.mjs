#!/usr/bin/env node
// Browser-decoder harness: E-AC-3 syncframes are decoded by am-hook's own
// `ec3.wasm` (the `@mediabunny/ac3` build the player ships) and rendered by the
// `joc.wasm` built from this repository.  It is the same pair of modules the
// browser worker uses, driven from Node so the comparison is reproducible.
//
//   node tests/joc_harness.mjs --ec3-runtime <ec3-runtime.mjs> --ec3-wasm <ec3.wasm> \
//       --joc-wasm <joc.wasm> --input <file.eac3> --mode speaker|binaural \
//       [--hrtf file.jochrtf] [--gain-db -4] [--frames N] [--skip-frames N] \
//       [--out out.f32] [--bed-out bed.f32] [--stats]
//
// `--bed-out` writes the EC-3 decoder's own interleaved float32 core PCM, which is
// what the C++ reference is fed (`joc_cli --bed`).

import { readFile, writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

function parseArgs(argv) {
  const options = { mode: 'speaker', gainDb: 0, frames: 0, skipFrames: 0, stats: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    switch (arg) {
      case '--ec3-runtime': options.ec3Runtime = argv[++i]; break;
      case '--ec3-wasm': options.ec3Wasm = argv[++i]; break;
      case '--joc-wasm': options.jocWasm = argv[++i]; break;
      case '--input': options.input = argv[++i]; break;
      case '--mode': options.mode = argv[++i]; break;
      case '--hrtf': options.hrtf = argv[++i]; break;
      case '--gain-db': options.gainDb = Number(argv[++i]); break;
      case '--frames': options.frames = Number(argv[++i]); break;
      case '--skip-frames': options.skipFrames = Number(argv[++i]); break;
      case '--drop-frames': options.dropFrames = Number(argv[++i]); break;
      case '--out': options.out = argv[++i]; break;
      case '--fragment': options.fragment = argv[++i]; break;
      case '--eac3-out': options.eac3Out = argv[++i]; break;
      case '--bed-out': options.bedOut = argv[++i]; break;
      case '--stats': options.stats = true; break;
      default: throw new Error(`unknown option ${arg}`);
    }
  }
  for (const required of ['ec3Runtime', 'ec3Wasm', 'jocWasm']) {
    if (!options[required]) throw new Error(`missing --${required.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`);
  }
  if (!options.input && !options.fragment) throw new Error('either --input or --fragment is required');
  return options;
}

/** Minimal ISO-BMFF walk, the same one the decode worker uses. */
function boxes(bytes, start = 0, end = bytes.length) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out = [];
  for (let p = start; p + 8 <= end;) {
    let size = view.getUint32(p);
    const type = String.fromCharCode(...bytes.subarray(p + 4, p + 8));
    let header = 8;
    if (size === 1) { size = Number(view.getBigUint64(p + 8)); header = 16; }
    else if (size === 0) size = end - p;
    if (size < header || p + size > end) throw new Error(`Invalid MP4 ${type} box`);
    out.push({ type, start: p, body: p + header, end: p + size });
    p += size;
  }
  return out;
}

function child(bytes, parent, type) {
  const box = boxes(bytes, parent.body, parent.end).find((entry) => entry.type === type);
  if (!box) throw new Error(`Missing MP4 ${type} box`);
  return box;
}

function top(bytes, type) { return child(bytes, { body: 0, end: bytes.length }, type); }

/** The EC-3 samples of one fMP4 fragment, in decode order. */
function samplesFromFragment(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const moof = top(bytes, 'moof');
  const traf = child(bytes, moof, 'traf');
  const tfhd = child(bytes, traf, 'tfhd');
  const flags = view.getUint32(tfhd.body) & 0xffffff;
  let cursor = tfhd.body + 8;
  if (flags & 0x01) cursor += 8;
  if (flags & 0x02) cursor += 4;
  const defaultSize = flags & 0x10 ? view.getUint32(cursor + (flags & 0x08 ? 4 : 0)) : 0;
  const samples = [];
  let nextData = top(bytes, 'mdat').body;
  for (const trun of boxes(bytes, traf.body, traf.end).filter((box) => box.type === 'trun')) {
    const runFlags = view.getUint32(trun.body) & 0xffffff;
    const count = view.getUint32(trun.body + 4);
    if (count > 100000) throw new Error('Too many EC-3 samples');
    let pos = trun.body + 8;
    let data = nextData;
    if (runFlags & 0x01) { data = moof.start + view.getInt32(pos); pos += 4; }
    if (runFlags & 0x04) pos += 4;
    for (let i = 0; i < count; i++) {
      if (runFlags & 0x100) pos += 4;
      const size = runFlags & 0x200 ? view.getUint32(pos) : defaultSize;
      if (runFlags & 0x200) pos += 4;
      if (runFlags & 0x400) pos += 4;
      if (runFlags & 0x800) pos += 4;
      if (!size || pos > trun.end || data < 0 || data + size > bytes.length) throw new Error('Invalid EC-3 sample range');
      samples.push(bytes.subarray(data, data + size));
      data += size;
    }
    nextData = data;
  }
  if (!samples.length) throw new Error('No EC-3 samples in fragment');
  return samples;
}

/** Splits a bare E-AC-3 stream into syncframes. */
function splitFrames(data) {
  const frames = [];
  let offset = 0;
  while (offset + 6 <= data.length) {
    if (data[offset] !== 0x0b || data[offset + 1] !== 0x77) throw new Error(`lost sync at ${offset}`);
    const bsid = (data[offset + 5] >> 3) & 0x1f;
    if (bsid <= 8) throw new Error(`AC-3 frame at ${offset} is not supported`);
    const frmsiz = ((data[offset + 2] & 0x07) << 8) | data[offset + 3];
    const size = (frmsiz + 1) * 2;
    if (offset + size > data.length) break;
    frames.push(data.subarray(offset, offset + size));
    offset += size;
  }
  return frames;
}

async function openEc3(runtimePath, wasmPath) {
  const { default: createModule } = await import(pathToFileURL(runtimePath).href);
  const wasmBinary = new Uint8Array(await readFile(wasmPath));
  const module = await createModule({ wasmBinary });
  const wrap = (name, args) => module.cwrap(name, 'number', args);
  return {
    module,
    open: wrap('init_decoder', ['number']),
    packet: wrap('configure_decode_packet', ['number', 'number']),
    decode: wrap('decode_packet', ['number', 'bigint']),
    format: wrap('get_decoded_format', ['number']),
    plane: wrap('get_decoded_plane_ptr', ['number', 'number']),
    channels: wrap('get_decoded_channels', ['number']),
    rate: wrap('get_decoded_sample_rate', ['number']),
    count: wrap('get_decoded_sample_count', ['number']),
    flush: wrap('flush_decoder', ['number']),
    close: wrap('close_decoder', ['number']),
  };
}

async function openJoc(wasmBytes) {
  const instance = await WebAssembly.instantiate(new WebAssembly.Module(wasmBytes), {});
  const e = instance.exports;
  const memory = e.memory;
  const error = () => {
    const ptr = e.joc_error_ptr();
    const len = e.joc_error_len();
    return Buffer.from(memory.buffer, ptr, len).toString('utf8') || 'unknown JOC error';
  };
  return { e, memory, error };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  let packets;
  if (options.fragment) {
    packets = samplesFromFragment(new Uint8Array(await readFile(options.fragment)));
  } else {
    packets = splitFrames(await readFile(options.input));
  }
  if (options.eac3Out) {
    const total = packets.reduce((sum, packet) => sum + packet.length, 0);
    const merged = new Uint8Array(total);
    let at = 0;
    for (const packet of packets) { merged.set(packet, at); at += packet.length; }
    await writeFile(options.eac3Out, Buffer.from(merged.buffer));
  }
  const count = options.frames ? Math.min(options.frames, packets.length) : packets.length;
  const start = Math.min(options.skipFrames, count);

  const dec = await openEc3(options.ec3Runtime, options.ec3Wasm);
  const ctx = dec.open(1);
  if (!ctx) throw new Error('cannot open the EC-3 decoder');

  const jocBytes = new Uint8Array(await readFile(options.jocWasm));
  const hrtf = options.hrtf ? new Uint8Array(await readFile(options.hrtf)) : null;
  const joc = await openJoc(jocBytes);
  const { e, memory } = joc;

  let hrtfPtr = 0;
  if (hrtf) {
    hrtfPtr = e.joc_alloc(hrtf.length);
    new Uint8Array(memory.buffer, hrtfPtr, hrtf.length).set(hrtf);
  }
  const handle = e.joc_open(
    options.mode === 'binaural' ? 1 : 0,
    options.gainDb,
    1, // mid profile
    1473,
    5.0,
    hrtfPtr,
    hrtf ? hrtf.length : 0,
  );
  if (!handle) throw new Error(joc.error());

  const pcmPtr = e.joc_alloc(1536 * 8 * 4);
  const outPtr = e.joc_alloc(4096 * 2 * 4);
  const statusPtr = e.joc_alloc(128);

  const output = [];
  const bedFrames = [];
  let channels = 0;
  let rate = 0;
  let skip = (options.dropFrames || 0) * 1536;
  const started = Date.now();

  const pullAll = (into) => {
    for (;;) {
      const produced = e.joc_pull(handle, outPtr, 4096);
      if (produced < 0) throw new Error(joc.error());
      if (produced === 0) break;
      const view = new Float32Array(memory.buffer, outPtr, produced * 2);
      let from = 0;
      if (skip > 0) {
        from = Math.min(skip, produced);
        skip -= from;
        if (from === produced) continue;
      }
      into.push(view.subarray(from * 2).slice());
    }
  };

  for (let index = start; index < count; index++) {
    const packet = packets[index];
    const ptr = dec.packet(ctx, packet.length);
    if (!ptr) throw new Error('EC-3 packet allocation failed');
    dec.module.HEAPU8.set(packet, ptr);
    const result = dec.decode(ctx, 0n);
    if (result < 0) throw new Error(`EC-3 decode failed (${result})`);
    if (dec.format(ctx) !== 8) throw new Error('EC-3 decoder returned non-planar PCM');
    const n = dec.count(ctx);
    const ch = dec.channels(ctx);
    const hz = dec.rate(ctx);
    if (!channels) { channels = ch; rate = hz; }
    if (ch !== channels || hz !== rate) throw new Error('EC-3 layout changed mid-stream');

    // Pack the decoder's planes into the interleaved layout the JOC stream reads.
    const interleaved = new Float32Array(n * ch);
    for (let c = 0; c < ch; c++) {
      const plane = dec.plane(ctx, c);
      if (!plane) throw new Error('missing EC-3 PCM plane');
      const source = new Float32Array(dec.module.HEAPU8.buffer, plane, n);
      for (let s = 0; s < n; s++) interleaved[s * ch + c] = source[s];
    }
    if (options.bedOut) bedFrames.push(interleaved.slice());

    const eac3Ptr = e.joc_alloc(packet.length);
    new Uint8Array(memory.buffer, eac3Ptr, packet.length).set(packet);
    if (e.joc_push_eac3(handle, eac3Ptr, packet.length) !== 0) throw new Error(joc.error());
    e.joc_free(eac3Ptr, packet.length);

    new Float32Array(memory.buffer, pcmPtr, interleaved.length).set(interleaved);
    if (e.joc_push_pcm(handle, pcmPtr, n, ch) !== 0) throw new Error(joc.error());
    // The frame is complete at the packet boundary; render it now.
    if (e.joc_flush_pending(handle) !== 0) throw new Error(joc.error());
    pullAll(output);
  }
  if (e.joc_flush(handle) !== 0) throw new Error(joc.error());
  pullAll(output);

  const statusSize = 128;
  e.joc_status(handle, statusPtr);
  const status = new DataView(memory.buffer, statusPtr, statusSize);
  const stats = {
    frames: count - start,
    seconds: ((count - start) * 1536) / (rate || 48000),
    bedChannels: channels,
    rate,
    outputFrames: output.reduce((sum, chunk) => sum + chunk.length, 0) / 2,
    renderMs: Date.now() - started,
    peak: status.getFloat64(56, true),
    overUnity: Number(status.getBigUint64(64, true)),
    nonFinite: Number(status.getBigUint64(72, true)),
    missingJoc: status.getUint32(36, true),
  };

  if (options.out) {
    const total = output.reduce((sum, chunk) => sum + chunk.length, 0);
    const merged = new Float32Array(total);
    let at = 0;
    for (const chunk of output) { merged.set(chunk, at); at += chunk.length; }
    await writeFile(options.out, Buffer.from(merged.buffer));
  }
  if (options.bedOut) {
    const total = bedFrames.reduce((sum, frame) => sum + frame.length, 0);
    const merged = new Float32Array(total);
    let at = 0;
    for (const frame of bedFrames) { merged.set(frame, at); at += frame.length; }
    await writeFile(options.bedOut, Buffer.from(merged.buffer));
  }
  if (options.stats) console.error(JSON.stringify(stats));
  e.joc_close(handle);
  dec.close(ctx);
}

main().catch((error) => {
  console.error(error.stack || error.message || String(error));
  process.exit(1);
});
