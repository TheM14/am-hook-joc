#!/usr/bin/env node
// Settings page check: the JOC section renders and its
// controls persist through prefs.mjs. Also takes a screenshot for review.

import { chromium } from 'playwright';
import { writeFile } from 'node:fs/promises';
import path from 'node:path';

const base = process.argv[2] || 'http://127.0.0.1:8888';
const outDir = process.argv[3] || '.';

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1280, height: 1000 }, locale: 'zh-CN' });
const logs = [];
page.on('console', (message) => logs.push(`${message.type()}: ${message.text()}`));
page.on('pageerror', (error) => logs.push(`pageerror: ${error.message}`));

await page.goto(`${base}/settings`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('#joc .set-group', { timeout: 30000 });
// The probe runs in a worker; wait for its verdict.
await page.waitForFunction(() => {
  const text = document.querySelector('#joc .set-group')?.textContent || '';
  return !text.includes('正在检查') && !text.includes('Checking');
}, null, { timeout: 30000 });

const section = await page.evaluate(() => {
  const root = document.querySelector('#joc .set-group');
  const buttons = [...root.querySelectorAll('.set-segment')].map((b) => ({
    label: b.textContent.trim(),
    checked: b.getAttribute('aria-checked'),
    disabled: b.disabled,
  }));
  const gain = root.querySelector('input[type=number]');
  const status = [...root.querySelectorAll('.set-desc')].map((n) => n.textContent.trim()).filter(Boolean);
  return {
    title: root.querySelector('.set-group-title')?.textContent.trim(),
    buttons,
    gain: gain ? { value: gain.value, min: gain.min, max: gain.max } : null,
    status,
    stored: JSON.parse(localStorage.getItem('am-hook:settings') || '{}').joc || null,
  };
});

// Change the mode and the attenuation, then read the stored settings back.
await page.click('#joc .set-segment:nth-child(2)'); // Speaker 2.0
const gain = page.locator('#joc input[type=number]');
await gain.fill('-6');
await gain.dispatchEvent('change');
const after = await page.evaluate(() => ({
  stored: JSON.parse(localStorage.getItem('am-hook:settings') || '{}').joc,
  checked: [...document.querySelectorAll('#joc .set-segment')].map((b) => b.getAttribute('aria-checked')),
}));

const shot = path.join(outDir, 'settings-joc.png');
await page.screenshot({ path: shot, fullPage: true });
await writeFile(path.join(outDir, 'settings-report.json'), JSON.stringify({ section, after, logs, shot }, null, 2) + '\n');

const problems = [];
if (!section.title || !/JOC/.test(section.title)) problems.push('JOC section title missing');
if (section.buttons.length !== 3) problems.push(`expected 3 mode buttons, got ${section.buttons.length}`);
if (!section.gain || section.gain.min !== '-60' || section.gain.max !== '0') problems.push('gain range is not -60..0');
if (after.stored?.mode !== 'speaker') problems.push(`mode did not persist (${after.stored?.mode})`);
if (after.stored?.gain !== -6) problems.push(`gain did not persist (${after.stored?.gain})`);
console.log(JSON.stringify({ section, after, problems }, null, 2));
await browser.close();
if (problems.length) {
  console.error('FAIL: ' + problems.join('; '));
  process.exit(1);
}
console.error('PASS: settings section renders and persists its controls');
