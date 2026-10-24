// Offline JOC regression: feeds the committed fixture (real EMDF containers,
// synthetic core PCM) through the WASM renderer and compares against reference
// outputs rendered by the native CLI from the same sources.
//
//   node offline_joc.mjs [--wasm path] [--fixtures dir] [--hrtf-dir dir]
//
// Covers: real ID14/OAMD parsing, object reconstruction, speaker and binaural
// rendering, .jochrtf loading for every shipped field, channel/level sanity.
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const args = { wasm: null, fixtures: path.join(here, 'fixtures'), hrtfDir: path.join(here, '..', 'hrtf') };
for (let i = 2; i < process.argv.length; i++) {
  if (process.argv[i] === '--wasm') args.wasm = process.argv[++i];
  else if (process.argv[i] === '--fixtures') args.fixtures = process.argv[++i];
  else if (process.argv[i] === '--hrtf-dir') args.hrtfDir = process.argv[++i];
}
if (!args.wasm) {
  const candidate = path.join(here, '..', 'joc-wasm', 'target', 'wasm32-unknown-unknown', 'wasm', 'joc.wasm');
  if (fs.existsSync(candidate)) args.wasm = candidate;
  else {
    console.error('error: pass --wasm <joc.wasm>');
    process.exit(2);
  }
}

const spec = JSON.parse(fs.readFileSync(path.join(args.fixtures, 'joc_fixture.json'), 'utf8'));
const reference = JSON.parse(fs.readFileSync(path.join(args.fixtures, 'test.json'), 'utf8'));
const eac3 = fs.readFileSync(path.join(args.fixtures, 'joc_fixture.eac3'));
const frames = [];
for (let offset = 0; offset < eac3.length;) {
  const size = ((((eac3[offset + 2] & 0x07) << 8) | eac3[offset + 3]) + 1) * 2;
  frames.push(eac3.subarray(offset, offset + size));
  offset += size;
}
if (frames.length !== spec.frames) throw new Error(`fixture has ${frames.length} frames, expected ${spec.frames}`);

const channels = spec.bed.channels;
const bedPerFrame = 1536;
const bed = new Float32Array(frames.length * bedPerFrame * channels);
for (let frame = 0; frame < frames.length; frame++) {
  for (let sample = 0; sample < bedPerFrame; sample++) {
    const t = (frame * bedPerFrame + sample) / 48000;
    for (let channel = 0; channel < channels; channel++) {
      bed[(frame * bedPerFrame + sample) * channels + channel] =
        spec.bed.amplitude * Math.sin(2 * Math.PI * spec.bed.tones_hz[channel] * t + channel * 0.3);
    }
  }
}

function readF32(file) {
  const buffer = fs.readFileSync(file);
  return new Float32Array(buffer.buffer, buffer.byteOffset, buffer.byteLength / 4);
}

const bytes = fs.readFileSync(args.wasm);
const { instance } = await WebAssembly.instantiate(bytes, {});
const ex = instance.exports;

class Joc {
  constructor(mode, hrtfBytes) {
    let hrtfPtr = 0;
    let hrtfLength = 0;
    if (hrtfBytes) {
      hrtfPtr = ex.joc_alloc(hrtfBytes.length);
      new Uint8Array(ex.memory.buffer, hrtfPtr, hrtfBytes.length).set(hrtfBytes);
      hrtfLength = hrtfBytes.length;
    }
    this.handle = ex.joc_open(mode, 0, 1, 1473, 5.0, hrtfPtr, hrtfLength);
    if (hrtfPtr) ex.joc_free(hrtfPtr, hrtfLength);
    if (!this.handle) throw new Error('joc_open failed');
    this.pcmCapacity = 1536 * channels;
    this.pcmPtr = ex.joc_alloc(this.pcmCapacity * 4);
    this.outPtr = ex.joc_alloc(8192 * 2 * 4);
  }
  push(frame, pcm) {
    const ptr = ex.joc_alloc(frame.length);
    new Uint8Array(ex.memory.buffer, ptr, frame.length).set(frame);
    let status = ex.joc_push_eac3(this.handle, ptr, frame.length);
    ex.joc_free(ptr, frame.length);
    if (status !== 0) throw new Error(`push_eac3 failed: ${status}`);
    new Float32Array(ex.memory.buffer, this.pcmPtr, pcm.length).set(pcm);
    status = ex.joc_push_pcm(this.handle, this.pcmPtr, pcm.length / channels, channels);
    if (status !== 0) throw new Error(`push_pcm failed: ${status}`);
  }
  flushPending() {
    const status = ex.joc_flush_pending(this.handle);
    if (status !== 0) throw new Error(`flush_pending failed: ${status}`);
  }
  pull() {
    const chunks = [];
    for (;;) {
      const count = ex.joc_pull(this.handle, this.outPtr, 8192);
      if (count <= 0) break;
      chunks.push(new Float32Array(ex.memory.buffer, this.outPtr, count * 2).slice());
    }
    const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
    const out = new Float32Array(total);
    let at = 0;
    for (const chunk of chunks) { out.set(chunk, at); at += chunk.length; }
    return out;
  }
  close() {
    if (this.handle) ex.joc_close(this.handle);
    ex.joc_free(this.pcmPtr, this.pcmCapacity * 4);
    ex.joc_free(this.outPtr, 8192 * 2 * 4);
    this.handle = 0;
  }
}

function compare(name, actual, expected) {
  if (actual.length !== expected.length) {
    throw new Error(`${name}: ${actual.length} samples, expected ${expected.length}`);
  }
  let max = 0;
  let peak = 0;
  for (let i = 0; i < actual.length; i++) {
    if (!Number.isFinite(actual[i])) throw new Error(`${name}: non-finite sample at ${i}`);
    max = Math.max(max, Math.abs(actual[i] - expected[i]));
    peak = Math.max(peak, Math.abs(actual[i]));
  }
  if (max > 1e-6) throw new Error(`${name}: max abs diff ${max}`);
  return { peak, max };
}

function render(mode, hrtf) {
  const engine = new Joc(mode, hrtf);
  try {
    for (let frame = 0; frame < frames.length; frame++) {
      engine.push(frames[frame], bed.subarray(frame * bedPerFrame * channels, (frame + 1) * bedPerFrame * channels));
    }
    engine.flushPending();
    return engine.pull();
  } finally {
    engine.close();
  }
}

const results = {};
const speaker = render(0, null);
results.speaker = compare('speaker', speaker,
  readF32(path.join(args.fixtures, 'test_speaker.f32')));
if (results.speaker.peak <= 0) throw new Error('speaker output is silent');

const d2 = fs.readFileSync(path.join(args.hrtfDir, 'D2.jochrtf'));
const binaural = render(1, d2);
const expectedBinaural = readF32(path.join(args.fixtures, 'test_binaural.f32'));
results.binaural = compare('binaural', binaural, expectedBinaural);
if (results.binaural.peak <= 0) throw new Error('binaural output is silent');

// Every shipped field must load and render.
const fields = fs.readdirSync(args.hrtfDir).filter((name) => name.endsWith('.jochrtf')).sort();
if (fields.length !== 20) throw new Error(`expected 20 HRTF fields, found ${fields.length}`);
for (const name of fields) {
  const engine = new Joc(1, fs.readFileSync(path.join(args.hrtfDir, name)));
  try {
    for (let frame = 0; frame < 4; frame++) {
      engine.push(frames[frame], bed.subarray(frame * bedPerFrame * channels, (frame + 1) * bedPerFrame * channels));
    }
    const out = engine.pull();
    if (out.length === 0) throw new Error(`${name}: no output`);
    let peak = 0;
    for (const value of out) {
      if (!Number.isFinite(value)) throw new Error(`${name}: non-finite sample`);
      peak = Math.max(peak, Math.abs(value));
    }
    if (peak <= 0) throw new Error(`${name}: silent output`);
  } finally {
    engine.close();
  }
}

console.log(JSON.stringify({
  frames: frames.length,
  speaker: results.speaker,
  binaural: results.binaural,
  expected: reference,
  fields: fields.length,
}, null, 2));
console.error('PASS: offline JOC fixture matches the native reference outputs and all HRTF fields render');
