import { chromium } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8898';
const track = process.argv[3];
if (!track) { console.error('usage: node fallback_check.mjs <base> <track>'); process.exit(2); }
const browser = await chromium.launch({ headless: true, args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'] });
const page = await browser.newPage();
await page.addInitScript(() => {
  localStorage.setItem('am-hook:settings', JSON.stringify({ joc: { mode: 'binaural', gain: -4 } }));
});
await page.goto(base, { waitUntil: 'domcontentloaded' });
await page.waitForFunction(() => !!window.AmApp && !!window.AmApp.player, null, { timeout: 30000 });
await page.evaluate(async (track) => {
  const { variants } = await window.AmWrapper.songMaster(track);
  const ec3 = variants.filter((v) => /^(ec-3|ec3)$/i.test(String(v.codecs)));
  const best = ec3.find((v) => /atmos/i.test(v.group_id)) || ec3[0];
  window.AmApp.player.play({
    id: `${track}:${best.group_id}`, track, codecs: best.codecs, m3u8Url: best.url,
    label: best.group_id, badge: 'Dolby Atmos', title: 'fallback',
  });
}, track);
await page.waitForFunction(() => window.AmApp.player.transport().currentTime > 0.5, null, { timeout: 60000 });
const state = await page.evaluate(() => {
  const badge = document.querySelector('.player-mode');
  const notice = document.querySelector('#player-notice');
  return {
    mode: window.AmApp.player.current.mode,
    badge: badge ? badge.textContent.trim() : null,
    notice: notice ? notice.textContent.trim() : null,
    failed: !!window.AmApp.player.jocHrtfFailed,
    playing: !window.AmApp.player.transport().paused,
  };
});
await page.goto(`${base}/settings`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('#joc .set-group');
await page.waitForFunction(() => {
  const text = document.querySelector('#joc .set-group')?.textContent || '';
  return !text.includes('正在检查') && !text.includes('Checking');
}, null, { timeout: 30000 });
const settings = await page.evaluate(() => {
  const buttons = [...document.querySelectorAll('#joc .set-segment')].map((b) => ({
    label: b.textContent.trim(), disabled: b.disabled,
  }));
  const binaural = buttons.find((b) => /Binaural/.test(b.label));
  return { buttons, binauralTitle: binaural ? (binaural.title || '') : '' };
});
console.log(JSON.stringify({ state, settings }, null, 2));
await browser.close();

const problems = [];
if (state.mode !== 'joc2') problems.push(`did not fall back to joc2 (${state.mode})`);
if (!state.failed) problems.push('jocHrtfFailed flag not set');
if (!/HRTF|jochrtf/i.test(state.notice || '')) problems.push('notice does not explain the missing HRTF');
if (!state.playing) problems.push('playback did not continue after the fallback');
if (!settings.buttons.find((b) => /Binaural/.test(b.label))?.disabled) problems.push('binaural option not disabled in settings');
if (!/jochrtf|HRTF/i.test(settings.binauralTitle)) problems.push('binaural option does not explain the missing HRTF');
if (problems.length) { console.error('FAIL: ' + problems.join('; ')); process.exit(1); }
console.error('PASS: missing HRTF disables binaural, explains it and falls back to Speaker 2.0');
