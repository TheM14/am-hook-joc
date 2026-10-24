#!/usr/bin/env node
// Dumps one real fragment's PCM from the browser worker, so it can be compared
// with the native renderers.
//
// It loads the app (for AmDecrypt/AmWrapper), resolves an Atmos track, fetches
// one media segment (an fMP4 fragment), then runs the real decode worker twice
// (speaker and binaural) on that fragment.  The fragment, the EC-3 samples the
// worker decoded, the bed PCM and both stereo outputs are written to --out-dir:
//
//   fragment.mp4  the fMP4 fragment
//   samples.eac3  its E-AC-3 syncframes, in order
//   speaker.f32   worker output, interleaved stereo float32
//   binaural.f32  worker output, interleaved stereo float32
//
// The native harness then re-decodes fragment.mp4 (or samples.eac3) with the same
// modules, and joc_cli.exe renders the same PCM, so all three can be diffed.

import { chromium } from 'playwright';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';

function parseArgs(argv) {
  const options = {
    base: 'http://127.0.0.1:8888',
    track: null,
    time: 30,
    gain: -4,
    outDir: 'out',
  };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--base') options.base = argv[++i];
    else if (argv[i] === '--track') options.track = argv[++i];
    else if (argv[i] === '--time') options.time = Number(argv[++i]);
    else if (argv[i] === '--gain') options.gain = Number(argv[++i]);
    else if (argv[i] === '--out-dir') options.outDir = argv[++i];
    else throw new Error(`unknown option ${argv[i]}`);
  }
  if (!options.track) throw new Error('missing --track');
  return options;
}

const BASE64_CHUNK = 0x8000;

function toBase64(bytes) {
  let text = '';
  for (let at = 0; at < bytes.length; at += BASE64_CHUNK) {
    text += String.fromCharCode(...bytes.subarray(at, at + BASE64_CHUNK));
  }
  return btoa(text);
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const browser = await chromium.launch({ headless: true, args: ['--autoplay-policy=no-user-gesture-required'] });
  const page = await browser.newPage();
  const logs = [];
  page.on('console', (message) => logs.push(`${message.type()}: ${message.text()}`));
  page.on('pageerror', (error) => logs.push(`pageerror: ${error.message}`));
  await page.goto(options.base, { waitUntil: 'domcontentloaded' });
  await page.waitForFunction(() => !!window.AmApp && !!window.AmDecrypt, null, { timeout: 30000 });

  const fragment = await page.evaluate(async ({ track, time }) => {
    const { variants } = await window.AmWrapper.songMaster(track);
    const ec3 = variants.filter((v) => /^(ec-3|ec3)$/i.test(String(v.codecs)));
    if (!ec3.length) throw new Error('no E-AC-3 variant');
    const best = ec3.find((v) => /atmos/i.test(v.group_id)) || ec3[0];
    const playlist = await window.AmDecrypt.openTrack(best.url);
    const segments = playlist.segments;
    let index = segments.findIndex((segment) => segment.time > time);
    if (index < 0) index = segments.length - 1;
    if (index > 0) index -= 1;
    const segment = segments[index];
    const buffer = await playlist.load(segment);
    const bytes = new Uint8Array(buffer.slice(0));
    let text = '';
    for (let at = 0; at < bytes.length; at += 32768) {
      text += String.fromCharCode(...bytes.subarray(at, at + 32768));
    }
    return {
      group: best.group_id,
      url: best.url,
      segmentIndex: index,
      segmentTime: segment.time,
      segmentDuration: segments[index + 1] ? segments[index + 1].time - segment.time : 0,
      totalSegments: segments.length,
      base64: btoa(text),
      init: playlist.init ? true : false,
    };
  }, { track: options.track, time: options.time });

  const fragmentBytes = Buffer.from(fragment.base64, 'base64');
  await mkdir(options.outDir, { recursive: true });
  await writeFile(path.join(options.outDir, 'fragment.mp4'), fragmentBytes);

  // Run the real worker on that fragment, once per mode.
  const outputs = await page.evaluate(async ({ base64, gain }) => {
    const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
    async function run(mode) {
      const worker = new Worker('/assets/joc-decode-worker.js', { type: 'module' });
      const request = (id, message, transfer = []) => new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`${mode} timeout`)), 120000);
        const listener = ({ data }) => {
          if (data.id !== id) return;
          clearTimeout(timer);
          worker.removeEventListener('message', listener);
          if (data.ok) resolve(data.result);
          else reject(new Error(data.error));
        };
        worker.addEventListener('message', listener);
        worker.postMessage({ id, ...message }, transfer);
      });
      await request(1, { op: 'open', mode, gainDb: gain });
      const copy = bytes.slice();
      const result = await request(2, { op: 'decode', buf: copy.buffer }, [copy.buffer]);
      await request(3, { op: 'close' });
      worker.terminate();
      const frames = new Uint8Array(result.frames.buffer.slice(0));
      let text = '';
      for (let at = 0; at < frames.length; at += 32768) text += String.fromCharCode(...frames.subarray(at, at + 32768));
      return { rate: result.rate, samples: result.samples, skipped: result.skipped, base64: btoa(text) };
    }
    const speaker = await run('speaker');
    const binaural = await run('binaural');
    return { speaker, binaural };
  }, { base64: fragment.base64, gain: options.gain });

  for (const [name, output] of Object.entries(outputs)) {
    await writeFile(path.join(options.outDir, `${name}.f32`), Buffer.from(output.base64, 'base64'));
    delete output.base64;
  }
  const report = {
    track: options.track,
    variant: fragment.group,
    segmentIndex: fragment.segmentIndex,
    segmentTime: fragment.segmentTime,
    segmentDuration: fragment.segmentDuration,
    fragmentBytes: fragmentBytes.length,
    speaker: outputs.speaker,
    binaural: outputs.binaural,
    errors: logs.filter((line) => /pageerror|error:/i.test(line)),
  };
  await writeFile(path.join(options.outDir, 'browser-dump.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report, null, 2));
  await browser.close();
}

main().catch((error) => {
  console.error(error.stack || error.message || String(error));
  process.exit(1);
});
