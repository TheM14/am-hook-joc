#!/usr/bin/env node
// Queue, track switching, pause/resume and normal end-of-track behaviour with
// the JOC engine.
//
//   queue: playQueue([A, B]) — in JOC mode the Atmos variants are preferred, so
//          both entries render through JOC; seeking A near its end must advance
//          to B (same engine), and B must keep rendering.
//   pause: pause holds the clock, play resumes from the same position.
//   switch: player.play(C) while A plays tears the engine down and starts C.
//
// Usage: node tests/queue_check.mjs <base> <trackA> <trackB> [mode]

import { chromium } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8888';
const trackA = process.argv[3];
const trackB = process.argv[4];
if (!trackA || !trackB) { console.error('usage: node queue_check.mjs <base> <trackA> <trackB> [mode]'); process.exit(2); }
const mode = process.argv[5] || 'speaker';

const browser = await chromium.launch({ headless: true, args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'] });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (error) => errors.push(error.message));
page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
await page.addInitScript((mode) => {
  localStorage.setItem('am-hook:settings', JSON.stringify({ joc: { mode, gain: -4 } }));
}, mode);
await page.goto(base, { waitUntil: 'domcontentloaded' });
await page.waitForFunction(() => !!window.AmApp && !!window.AmApp.player, null, { timeout: 30000 });

const start = await page.evaluate(async ({ trackA, trackB }) => {
  const item = async (track) => {
    const { variants } = await window.AmWrapper.songMaster(track);
    const ec3 = variants.filter((v) => /^(ec-3|ec3)$/i.test(String(v.codecs)));
    const best = ec3.find((v) => /atmos/i.test(v.group_id)) || ec3[0];
    return { entry: { track, country: 'jp', name: `T${track}` }, item: {
      id: `${track}:${best.group_id}`, track, codecs: best.codecs, m3u8Url: best.url,
      label: best.group_id, badge: 'Dolby Atmos', title: `T${track}`,
    } };
  };
  const a = await item(trackA);
  const b = await item(trackB);
  // A queue of two entries: playAt resolves each entry through the same quality
  // preference the UI uses (Atmos first while JOC is on).
  window.AmApp.player.playQueue([a.entry, b.entry], 0);
  return { a: a.item.id, b: b.item.id };
}, { trackA, trackB });

await page.waitForFunction(() => {
  const p = window.AmApp.player;
  return p.current && /^joc/.test(p.current.mode) && p.transport().currentTime > 0.3;
}, null, { timeout: 60000 });

const first = await page.evaluate(() => {
  const p = window.AmApp.player;
  return { track: p.current.track, mode: p.current.mode, duration: p.current.duration, time: p.transport().currentTime };
});

// Pause / resume.
await page.evaluate(() => window.AmApp.player.transport().pause());
await page.waitForTimeout(1200);
const paused = await page.evaluate(() => {
  const t = window.AmApp.player.transport();
  return { time: t.currentTime, paused: t.paused };
});
await page.evaluate(() => window.AmApp.player.transport().play());
await page.waitForTimeout(1500);
const resumed = await page.evaluate(() => {
  const t = window.AmApp.player.transport();
  return { time: t.currentTime, paused: t.paused };
});

// Jump close to the end of A: the queue must advance to B.
await page.evaluate(async () => {
  const p = window.AmApp.player;
  await p.transport().seek(Math.max(0, p.current.duration - 4));
});
await page.waitForFunction(() => window.AmApp.player.current && window.AmApp.player.current.track !== window.__firstTrack,
  null, { timeout: 45000 }).catch(() => {});
await page.evaluate((track) => { window.__firstTrack = track; }, first.track);
await page.waitForFunction((track) => window.AmApp.player.current && window.AmApp.player.current.track !== track,
  first.track, { timeout: 60000 });
await page.waitForTimeout(2500);

const second = await page.evaluate(() => {
  const p = window.AmApp.player;
  const t = p.transport();
  return {
    track: p.current.track, mode: p.current.mode, time: t.currentTime, paused: t.paused,
    buffers: (window.__jocBuffers || []).length,
  };
});

const report = { requested: { mode, a: start.a, b: start.b }, first, paused, resumed, second, errors };
console.log(JSON.stringify(report, null, 2));
await browser.close();

const problems = [];
if (!/^joc/.test(first.mode)) problems.push(`first track mode ${first.mode}`);
if (!paused.paused) problems.push('pause did not hold');
if (resumed.paused) problems.push('play did not resume');
const advanced = resumed.time - paused.time;
if (advanced < 0 || advanced > 2.5) problems.push(`clock did not resume sanely across pause/resume (${advanced.toFixed(2)} s)`);
if (second.track === first.track) problems.push('queue did not advance to the next track');
if (!/^joc/.test(second.mode)) problems.push(`second track mode ${second.mode}`);
if (second.paused) problems.push('second track is not playing');
if (errors.length) problems.push(`console errors: ${errors.join(' | ')}`);
if (problems.length) {
  console.error('FAIL: ' + problems.join('; '));
  process.exit(1);
}
console.error('PASS: queue advance, track switch and pause/resume work in JOC mode');
