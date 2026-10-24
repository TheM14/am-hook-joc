#!/usr/bin/env node
// End-to-end check of the JOC playback modes in the real am-hook UI.
//
//   1. preload the settings (mode / gain) through localStorage,
//   2. resolve an E-AC-3 (Atmos) variant of a track through the app's own
//      wrapper client and start it on the player,
//   3. watch playback, seek (including several rapid seeks), switch tracks,
//   4. collect every scheduled AudioBuffer's peak/RMS and the console log.
//
// Usage:
//   node tests/e2e_joc.mjs --base http://127.0.0.1:8888 --track <adamId> \
//       --mode speaker|binaural [--gain -4] [--seconds 8] [--json report.json]
//
// Requires Playwright (tests/package.json); the browser must allow autoplay,
// which the launch flags below do.

import { chromium } from 'playwright';
import { writeFile } from 'node:fs/promises';

function parseArgs(argv) {
  const options = { base: 'http://127.0.0.1:8888', mode: 'speaker', gain: -4, seconds: 8, json: null, track: null, expect: null };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--base') options.base = argv[++i];
    else if (argv[i] === '--track') options.track = argv[++i];
    else if (argv[i] === '--mode') options.mode = argv[++i];
    else if (argv[i] === '--gain') options.gain = Number(argv[++i]);
    else if (argv[i] === '--seconds') options.seconds = Number(argv[++i]);
    else if (argv[i] === '--json') options.json = argv[++i];
    else if (argv[i] === '--expect-mode') options.expect = argv[++i];
    else throw new Error(`unknown option ${argv[i]}`);
  }
  if (!options.track) throw new Error('missing --track');
  return options;
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const browser = await chromium.launch({
    headless: true,
    args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'],
  });
  const page = await browser.newPage();
  const logs = [];
  page.on('console', (message) => logs.push(`${message.type()}: ${message.text()}`));
  page.on('pageerror', (error) => logs.push(`pageerror: ${error.message}`));

  await page.addInitScript(({ mode, gain }) => {
    localStorage.setItem('am-hook:settings', JSON.stringify({ joc: { mode, gain } }));
    localStorage.setItem('am-hook:volume', '1');
    // Record what the player actually schedules, without keeping the samples.
    window.__jocBuffers = [];
    const start = AudioBufferSourceNode.prototype.start;
    AudioBufferSourceNode.prototype.start = function startWrapped(...args) {
      try {
        if (this.buffer && window.__jocBuffers.length < 400) {
          const channels = this.buffer.numberOfChannels;
          let peak = 0;
          let energy = 0;
          for (let c = 0; c < channels; c++) {
            const data = this.buffer.getChannelData(c);
            for (let i = 0; i < data.length; i++) {
              const value = data[i];
              if (Math.abs(value) > peak) peak = Math.abs(value);
              energy += value * value;
            }
          }
          window.__jocBuffers.push({
            channels,
            length: this.buffer.length,
            rate: this.buffer.sampleRate,
            peak,
            rms: Math.sqrt(energy / (this.buffer.length * channels)),
            at: this.context ? this.context.currentTime : 0,
          });
        }
      } catch (error) {
        window.__jocBuffers.push({ error: String(error) });
      }
      return start.apply(this, args);
    };
  }, { mode: options.mode, gain: options.gain });

  await page.goto(options.base, { waitUntil: 'domcontentloaded' });
  await page.waitForFunction(() => !!window.AmApp && !!window.AmApp.player, null, { timeout: 30000 });

  const started = await page.evaluate(async (track) => {
    const { variants } = await window.AmWrapper.songMaster(track);
    const ec3 = variants.filter((v) => /^(ec-3|ec3)$/i.test(String(v.codecs)));
    if (!ec3.length) return { error: 'track has no E-AC-3 variant', variants: variants.map((v) => v.group_id) };
    const best = ec3.find((v) => /atmos/i.test(v.group_id)) || ec3[0];
    const item = {
      id: `${track}:${best.group_id}`,
      track,
      codecs: best.codecs,
      m3u8Url: best.url,
      label: best.group_id,
      badge: 'Dolby Atmos',
      title: 'E2E',
    };
    window.AmApp.player.play(item);
    return { group: best.group_id, url: best.url, modes: window.AmHook.detectModes(best.codecs) };
  }, options.track);
  if (started.error) throw new Error(started.error);

  // Wait until the JOC engine is running.
  await page.waitForFunction(() => {
    const player = window.AmApp.player;
    return player.current && /^joc/.test(player.current.mode) && player.transport().currentTime > 0.5;
  }, null, { timeout: 60000 });

  const first = await page.evaluate(() => {
    const player = window.AmApp.player;
    const transport = player.transport();
    return {
      mode: player.current.mode,
      duration: player.current.duration,
      playing: !transport.paused,
      time: transport.currentTime,
      rate: transport.rate,
      gainDb: transport.gainDb,
      channels: transport.channels,
      qualityGroup: player.current.label,
    };
  });

  await sleep(options.seconds * 1000);
  const afterPlay = await page.evaluate(() => {
    const player = window.AmApp.player;
    const transport = player.transport();
    return { time: transport.currentTime, playing: !transport.paused, loadedUntil: transport.loadedUntil };
  });

  // Seek backward, then forward, then several rapid seeks, like dragging the bar.
  const seeks = [];
  for (const target of [20, 5, 40, 12, 33, 8]) {
    const result = await page.evaluate(async (time) => {
      const player = window.AmApp.player;
      const transport = player.transport();
      await transport.seek(time);
      return { requested: time, current: transport.currentTime, playing: !transport.paused };
    }, target);
    await sleep(600);
    result.after = await page.evaluate(() => {
      const transport = window.AmApp.player.transport();
      return { current: transport.currentTime, playing: !transport.paused };
    });
    seeks.push(result);
  }

  const tail = await page.evaluate(() => {
    const player = window.AmApp.player;
    const buffers = window.__jocBuffers || [];
    const peaks = buffers.filter((b) => typeof b.peak === 'number').map((b) => b.peak);
    const rms = buffers.filter((b) => typeof b.rms === 'number').map((b) => b.rms);
    return {
      mode: player.current.mode,
      current: player.transport().currentTime,
      paused: player.transport().paused,
      buffers: buffers.length,
      maxPeak: peaks.length ? Math.max(...peaks) : 0,
      meanRms: rms.length ? rms.reduce((a, b) => a + b, 0) / rms.length : 0,
      silent: rms.filter((value) => value < 1e-6).length,
      sampleRates: [...new Set(buffers.map((b) => b.rate))],
      channels: [...new Set(buffers.map((b) => b.channels))],
    };
  });

  const report = {
    base: options.base,
    requestedMode: options.mode,
    expectedMode: options.expect,
    gainDb: options.gain,
    variant: started.group,
    detectModes: started.modes,
    first,
    afterPlay,
    seeks,
    tail,
    errors: logs.filter((line) => /pageerror|error:/i.test(line)),
    logs: logs.slice(-40),
  };
  console.log(JSON.stringify(report, null, 2));
  if (options.json) await writeFile(options.json, JSON.stringify(report, null, 2) + '\n');
  await browser.close();

  // Acceptance checks that do not depend on the audio device.
  const problems = [];
  if (options.expect && first.mode !== options.expect) problems.push(`mode is ${first.mode}, expected ${options.expect}`);
  else if (!/^joc/.test(first.mode)) problems.push(`mode is ${first.mode}, expected a JOC mode`);
  if (!first.playing) problems.push('playback did not start');
  if (afterPlay.time - first.time < options.seconds * 0.5) problems.push('playback clock did not advance');
  for (const seek of seeks) {
    if (Math.abs(seek.after.current - seek.requested) > 3) {
      problems.push(`seek to ${seek.requested} landed at ${seek.after.current.toFixed(2)}`);
    }
    if (!seek.after.playing) problems.push(`playback stopped after seeking to ${seek.requested}`);
  }
  if (tail.silent) problems.push(`${tail.silent} scheduled buffers are silent`);
  if (tail.maxPeak <= 0) problems.push('no audio was produced');
  if (options.mode === 'binaural' && tail.channels.some((c) => c !== 2)) problems.push('binaural output is not stereo');
  if (problems.length) {
    console.error('FAIL:\n - ' + problems.join('\n - '));
    process.exitCode = 1;
  } else {
    console.error('PASS: JOC playback, seeks and buffer scheduling behaved as expected');
  }
}

main().catch((error) => {
  console.error(error.stack || error.message || String(error));
  process.exit(1);
});
