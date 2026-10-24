#!/usr/bin/env node
// Regression check with JOC off: the stock playback paths must be unchanged.
// Plays the Atmos and the lossless variant of a track through the normal UI
// entry point and checks the chosen mode.

import { chromium } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8888';
const track = process.argv[3];
if (!track) { console.error('usage: node regression_check.mjs <base> <track>'); process.exit(2); }

const browser = await chromium.launch({ headless: true, args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'] });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (error) => errors.push(error.message));
page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
await page.addInitScript(() => {
  localStorage.setItem('am-hook:settings', JSON.stringify({ joc: { mode: 'off', gain: -4 } }));
});
await page.goto(base, { waitUntil: 'domcontentloaded' });
await page.waitForFunction(() => !!window.AmApp && !!window.AmApp.player, null, { timeout: 30000 });

async function playVariant(kind) {
  return page.evaluate(async ({ track, kind }) => {
    const { variants } = await window.AmWrapper.songMaster(track);
    const match = variants.find((v) => kind === 'atmos'
      ? /^(ec-3|ec3)$/i.test(String(v.codecs)) && /atmos/i.test(v.group_id)
      : /alac/i.test(v.group_id));
    if (!match) return { error: `no ${kind} variant` };
    window.AmApp.player.play({
      id: `${track}:${match.group_id}`, track, codecs: match.codecs, m3u8Url: match.url,
      label: match.group_id, badge: kind, title: kind,
    });
    return { group: match.group_id, modes: window.AmHook.detectModes(match.codecs) };
  }, { track, kind });
}

const atmos = await playVariant('atmos');
await page.waitForFunction(() => {
  const p = window.AmApp.player;
  return p.current && p.transport().currentTime > 0.3;
}, null, { timeout: 60000 });
const atmosMode = await page.evaluate(() => ({
  mode: window.AmApp.player.current.mode,
  time: window.AmApp.player.transport().currentTime,
}));

const alac = await playVariant('alac');
await page.waitForFunction(() => {
  const p = window.AmApp.player;
  return p.current && p.transport().currentTime > 0.3;
}, null, { timeout: 60000 });
const alacMode = await page.evaluate(() => ({
  mode: window.AmApp.player.current.mode,
  time: window.AmApp.player.transport().currentTime,
}));

const report = { atmos, atmosMode, alac, alacMode, errors };
console.log(JSON.stringify(report, null, 2));
await browser.close();

const problems = [];
if (atmos.error) problems.push(atmos.error);
else if (!['mse', 'ec3'].includes(atmosMode.mode)) problems.push(`Atmos played as ${atmosMode.mode}`);
if (alac.error) problems.push(alac.error);
else if (!['mse', 'flac'].includes(alacMode.mode)) problems.push(`ALAC played as ${alacMode.mode}`);
if (errors.length) problems.push(`console errors: ${errors.join(' | ')}`);
if (problems.length) {
  console.error('FAIL: ' + problems.join('; '));
  process.exit(1);
}
console.error('PASS: stock playback paths are unchanged with JOC off');
