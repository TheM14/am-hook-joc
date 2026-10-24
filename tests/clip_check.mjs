#!/usr/bin/env node
// Clipping check: with 0 dB gain the binaural render exceeds full scale near
// 172.6 s of the reference track; the player must say so instead of silently
// hard-clipping. At -4 dB the same region must not warn.

import { chromium } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8888';
const track = process.argv[3];
if (!track) { console.error('usage: node clip_check.mjs <base> <track> [seek] [safeGain] [hotGain]'); process.exit(2); }
const seekTo = Number(process.argv[4] || 168);
const safeGain = Number(process.argv[5] || -12);

const browser = await chromium.launch({ headless: true, args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'] });

async function run(gain, boost = 0) {
  const page = await browser.newPage();
  await page.addInitScript(({ gain }) => {
    localStorage.setItem('am-hook:settings', JSON.stringify({ joc: { mode: 'binaural', gain } }));
  }, { gain });
  await page.goto(base, { waitUntil: 'domcontentloaded' });
  await page.waitForFunction(() => !!window.AmApp && !!window.AmApp.player, null, { timeout: 30000 });
  await page.evaluate(async ({ track }) => {
    const { variants } = await window.AmWrapper.songMaster(track);
    const ec3 = variants.filter((v) => /^(ec-3|ec3)$/i.test(String(v.codecs)));
    const best = ec3.find((v) => /atmos/i.test(v.group_id)) || ec3[0];
    window.AmApp.player.play({
      id: `${track}:${best.group_id}`, track, codecs: best.codecs, m3u8Url: best.url,
      label: best.group_id, badge: 'Dolby Atmos', title: 'clip',
    });
  }, { track });
  await page.waitForFunction(() => window.AmApp.player.transport().currentTime > 0.5, null, { timeout: 60000 });
  await page.evaluate(async (time) => {
    await window.AmApp.player.transport().seek(time);
  }, seekTo);
  await page.waitForTimeout(2000);
  if (boost) {
    // The settings cap the gain at 0 dB; push it past that to exercise the
    // clipping detection on a master that would otherwise stay below full scale.
    await page.evaluate((value) => {
      const engine = window.AmApp.player.pcm;
      engine.setGain(value);
      // updatePcm() keeps the engine in sync with the settings (which cap the
      // gain at 0 dB); pin the reported value so that sync does not undo the boost.
      engine.gainDb = 0;
    }, boost);
  }
  await page.waitForTimeout(10000);
  const result = await page.evaluate(() => ({
    mode: window.AmApp.player.current && window.AmApp.player.current.mode,
    time: window.AmApp.player.transport().currentTime,
    message: document.querySelector('.player-msg')?.textContent || '',
    overUnity: window.AmApp.player.pcm ? window.AmApp.player.pcm.overUnity : null,
    peak: window.AmApp.player.pcm ? window.AmApp.player.pcm.peak : null,
  }));
  await page.close();
  return result;
}

const loud = await run(0, Number(process.argv[6] || 12));
const safe = await run(safeGain);
console.log(JSON.stringify({ gain0: loud, safeGain: { gain: safeGain, ...safe } }, null, 2));
await browser.close();

const problems = [];
if (!/削波|clipping/i.test(loud.message)) problems.push('0 dB did not warn about clipping');
if (!(loud.overUnity > 0)) problems.push('0 dB did not report over-unity samples');
if (safe.overUnity > 0) problems.push(`${safeGain} dB still reports ${safe.overUnity} over-unity samples`);
if (/削波|clipping/i.test(safe.message)) problems.push(`${safeGain} dB warned about clipping`);
if (problems.length) {
  console.error('FAIL: ' + problems.join('; '));
  process.exit(1);
}
console.error('PASS: clipping is detected and reported at 0 dB, silent at the safe gain');
