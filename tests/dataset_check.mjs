// Checks the HRTF dataset selector: the SADIE II subjects are listed, D2 is the
// default, the choice persists, and a running engine reloads the chosen field.
import { chromium } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8888';
const track = process.argv[3];
if (!track) {
  console.error('usage: node dataset_check.mjs <base> <track>');
  process.exit(2);
}

const browser = await chromium.launch({
  headless: true,
  args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'],
});
const page = await browser.newPage();
const errors = [];
const fields = new Set();
page.on('pageerror', (error) => errors.push(String(error)));
page.on('request', (request) => {
  const match = /joc-hrtf-([A-Z0-9]+)(?:\.[0-9a-f]{16})?\.jochrtf/.exec(request.url());
  if (match) fields.add(match[1]);
});

await page.goto(`${base}/settings`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('#joc select');
const settings = await page.evaluate(() => {
  const select = document.querySelector('#joc select');
  return {
    options: [...select.options].map((option) => option.value),
    value: select.value,
  };
});
await page.selectOption('#joc select', 'H3');
const stored = await page.evaluate(
  () => JSON.parse(localStorage.getItem('am-hook:settings') || '{}').joc || null);

await page.evaluate(() => localStorage.setItem('am-hook:settings', JSON.stringify({
  joc: { mode: 'binaural', gain: 0, dataset: 'D2' },
})));
await page.goto(base, { waitUntil: 'domcontentloaded' });
await page.waitForFunction(() => !!window.AmApp?.player, null, { timeout: 30000 });
await page.evaluate(async (id) => {
  const { variants } = await window.AmWrapper.songMaster(id);
  const ec3 = variants.filter((v) => /^(ec-3|ec3)$/i.test(String(v.codecs)));
  const best = ec3.find((v) => /atmos/i.test(v.group_id)) || ec3[0];
  await window.AmApp.player.play({
    id: `${id}:${best.group_id}`, track: id, codecs: best.codecs, m3u8Url: best.url,
    label: best.group_id, badge: 'Dolby Atmos', title: 'dataset',
  });
}, track);
await page.waitForFunction(() => window.AmApp.player.transport().currentTime > 0.5,
  null, { timeout: 60000 });
const before = await page.evaluate(() => ({
  mode: window.AmApp.player.current.mode,
  dataset: window.AmApp.player.pcm.dataset,
}));
for (const dataset of ['H3', 'D1']) {
  await page.evaluate((value) => window.AmPrefs.set('joc', { dataset: value }), dataset);
  await page.waitForTimeout(2500);
}
const after = await page.evaluate(() => ({
  dataset: window.AmApp.player.pcm.dataset,
  playing: !window.AmApp.player.transport().paused,
  time: window.AmApp.player.transport().currentTime,
}));

console.log(JSON.stringify({ settings, stored, before, after, fields: [...fields], errors }, null, 2));
await browser.close();

const problems = [];
const expected = ['D1', 'D2', ...Array.from({ length: 18 }, (_, i) => `H${i + 3}`)];
if (settings.options.join(',') !== expected.join(',')) problems.push(`options: ${settings.options}`);
if (settings.value !== 'D2') problems.push(`default is ${settings.value}`);
if (stored?.dataset !== 'H3') problems.push('selection was not persisted');
if (before.mode !== 'jocbin' || before.dataset !== 'D2') problems.push('engine did not start on D2');
if (after.dataset !== 'D1' || !after.playing) problems.push('live dataset switch failed');
if (!fields.has('H3') || !fields.has('D1')) problems.push(`fields requested: ${[...fields]}`);
if (errors.length) problems.push(...errors);
if (problems.length) {
  console.error('FAIL: ' + problems.join('; '));
  process.exit(1);
}
console.error('PASS: HRTF datasets listed, default D2, live switching loads the new field');
