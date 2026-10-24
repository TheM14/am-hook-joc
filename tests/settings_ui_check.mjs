import { chromium } from 'playwright';

const base = process.argv[2] || 'http://127.0.0.1:8888';
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
const errors = [];
page.on('pageerror', (e) => errors.push(String(e)));
await page.goto(`${base}/settings`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('#joc .set-card');

const info = await page.evaluate(() => {
  const toc = [...document.querySelectorAll('#toc .set-toc-link')].map((a) => ({
    text: a.textContent.trim(), href: a.getAttribute('href'),
  }));
  const title = document.querySelector('#joc .set-group-title')?.textContent || '';
  const rows = [...document.querySelectorAll('#joc .set-row .set-label > span:first-child')].map((s) => s.textContent.trim());
  const cardText = document.querySelector('#joc')?.textContent || '';
  const binaural = [...document.querySelectorAll('#joc .set-segment')].find((b) => /Binaural/.test(b.textContent));
  return { toc, title, rows, hasHrtfRow: /HRTF 资源|HRTF asset/.test(cardText), binauralDisabled: binaural ? binaural.disabled : null };
});

// Jump: click the JOC chip in the sticky TOC and check the section lands near the top.
const before = await page.evaluate(() => window.scrollY);
await page.click('#toc .set-toc-link[href="#joc"]');
await page.waitForTimeout(700);
const after = await page.evaluate(() => window.scrollY);
const top = await page.evaluate(() => {
  const rect = document.querySelector('#joc .set-group').getBoundingClientRect();
  return Math.round(rect.top);
});
await page.screenshot({ path: '/tmp/settings-joc.png', fullPage: false });

console.log(JSON.stringify({ ...info, jump: { before, after, sectionTop: top }, errors }, null, 2));
await browser.close();

const problems = [];
if (info.title !== 'JOC 对象渲染 (Dolby Atmos)') problems.push(`title is ${JSON.stringify(info.title)}`);
if (info.hasHrtfRow) problems.push('HRTF row still present');
if (!info.toc.some((e) => e.href === '#joc')) problems.push('TOC has no JOC entry');
if (after <= before) problems.push(`TOC click did not scroll (${before} -> ${after})`);
// The JOC section is the last one, so the jump can only reach the maximum scroll
// position; require it to be visible and clearly scrolled.
if (top < 0 || top > 90) problems.push(`section top is ${top} after the jump`);
if (errors.length) problems.push(...errors);
if (problems.length) { console.error('FAIL: ' + problems.join('; ')); process.exit(1); }
console.error('PASS: settings title, removed HRTF row and TOC jump behave as expected');
