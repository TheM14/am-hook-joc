#!/usr/bin/env node
// Performance measurement for the JOC engine in the browser: render cost per
// second of audio (from the worker's own timings), main-thread task time and JS
// heap while playing, and the configured look-ahead latency.

import { chromium } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8888';
const track = process.argv[3];
if (!track) { console.error('usage: node perf_check.mjs <base> <track> [mode] [seconds]'); process.exit(2); }
const mode = process.argv[4] || 'binaural';
const seconds = Number(process.argv[5] || 30);

const browser = await chromium.launch({ headless: true, args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'] });
const page = await browser.newPage();
await page.addInitScript((mode) => {
  localStorage.setItem('am-hook:settings', JSON.stringify({ joc: { mode, gain: -4 } }));
}, mode);
await page.goto(base, { waitUntil: 'domcontentloaded' });
await page.waitForFunction(() => !!window.AmApp && !!window.AmApp.player, null, { timeout: 30000 });

const engineStart = await page.evaluate(async (track) => {
  const { variants } = await window.AmWrapper.songMaster(track);
  const ec3 = variants.filter((v) => /^(ec-3|ec3)$/i.test(String(v.codecs)));
  const best = ec3.find((v) => /atmos/i.test(v.group_id)) || ec3[0];
  const t0 = performance.now();
  await window.AmApp.player.play({
    id: `${track}:${best.group_id}`, track, codecs: best.codecs, m3u8Url: best.url,
    label: best.group_id, badge: 'Dolby Atmos', title: 'perf',
  });
  // play() resolves once the first fragment is scheduled; wait for the clock.
  while (window.AmApp.player.transport().currentTime <= 0.01) {
    if (performance.now() - t0 > 30000) throw new Error('playback did not start');
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return performance.now() - t0;
}, track);
const startupMs = Math.round(engineStart);
await page.waitForFunction(() => window.AmApp.player.transport().currentTime > 0.5, null, { timeout: 60000 });

const client = await page.context().newCDPSession(page);
await client.send('Performance.enable');
const before = Object.fromEntries((await client.send('Performance.getMetrics')).metrics.map((m) => [m.name, m.value]));
const heapBefore = await page.evaluate(() => performance.memory ? performance.memory.usedJSHeapSize : 0);
await page.waitForTimeout(seconds * 1000);
const after = Object.fromEntries((await client.send('Performance.getMetrics')).metrics.map((m) => [m.name, m.value]));
const heapAfter = await page.evaluate(() => performance.memory ? performance.memory.usedJSHeapSize : 0);

const engine = await page.evaluate(() => {
  const player = window.AmApp.player;
  const pcm = player.pcm;
  const transport = player.transport();
  return {
    mode: player.current && player.current.mode,
    time: transport.currentTime,
    decodedMs: pcm ? pcm.decodedMs : 0,
    decodedAudio: pcm ? pcm.decodedAudio : 0,
    overUnity: pcm ? pcm.overUnity : 0,
    nonFinite: pcm ? pcm.nonFinite : 0,
    peak: pcm ? pcm.peak : 0,
    loadedUntil: transport.loadedUntil,
    duration: player.current && player.current.duration,
    wasmMegabytes: player.pcm && player.pcm.wasmBytes ? player.pcm.wasmBytes / 1048576 : null,
  };
});

const taskSeconds = (after.TaskDuration || 0) - (before.TaskDuration || 0);
const report = {
  mode,
  seconds,
  engine,
  render: {
    decodedAudioSeconds: engine.decodedAudio,
    workerMilliseconds: engine.decodedMs,
    realtimeFactor: engine.decodedMs > 0 ? engine.decodedAudio / (engine.decodedMs / 1000) : null,
  },
  mainThread: {
    taskSeconds,
    taskPercentOfWall: taskSeconds / seconds * 100,
    heapBeforeMB: heapBefore / 1048576,
    heapAfterMB: heapAfter / 1048576,
    nodes: after.Nodes,
    layoutCount: after.LayoutCount,
  },
  latency: {
    startupMs,
    lookAheadSeconds: engine.loadedUntil - engine.time,
  },
};
console.log(JSON.stringify(report, null, 2));
await browser.close();
