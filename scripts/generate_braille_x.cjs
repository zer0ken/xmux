// Regenerate the fixed Braille frame atlas from the final, user-approved prototype.
// Usage: node scripts/generate_braille_x.cjs [prototype-directory]
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || './braille_x_prototype/node_modules/playwright');

const prototype = process.argv[2] || path.join(__dirname, 'braille_x_prototype');
const sequence = 'X⚔️χ✂️Ж⚒️✘🛠️ж🦋✗🤞x☒';
const output = path.resolve(__dirname, '../src/ui/braille_x');
fs.mkdirSync(output, { recursive: true });

(async () => {
  const browser = await chromium.launch(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {});
  try {
    for (const width of [32, 64]) {
      const page = await browser.newPage({ viewport: { width: 1280, height: 950 } });
      const source = path.join(prototype, `x-${width}-20261004_1756.html`);
      await page.goto(pathToFileURL(source).href);
      await page.waitForFunction(() => window.brailleAnimation?.columns === 32 || window.brailleAnimation?.columns === 64);
      const result = await page.evaluate(sequence => {
        brailleAnimation.setText(sequence);
        const columns = brailleAnimation.columns;
        const rows = brailleAnimation.rows;
        const glyphs = [...new Intl.Segmenter(undefined, { granularity: 'grapheme' }).segment(sequence)].length;
        const bytes = [];
        const bit = [[1, 8], [2, 16], [4, 32], [64, 128]];
        for (let glyph = 0; glyph < glyphs; glyph++) {
          for (let step = 0; step < 14; step++) {
            // Stay just inside each phase so binary floating-point rounding cannot
            // select the preceding glyph at an exact 1.4-second boundary.
            const seconds = glyph * 1.4 + .001 + (step === 0 ? 0 : 1 + (step - 1) * .033);
            brailleAnimation.renderAt(seconds);
            const characters = [...document.querySelector('#art').textContent.replaceAll('\n', '')];
            if (characters.length !== columns * rows) throw new Error(`bad frame ${glyph}/${step}`);
            const codes = [];
            for (const character of characters) {
              const dot = character.codePointAt(0) - 0x2800;
              if (dot < 0 || dot > 255) throw new Error('non-Braille character');
              codes.push(dot);
            }
            // The final outline is solid. Raster-to-Braille rounding can still
            // leave a lone unlit pixel surrounded on all four sides; close it.
            const pixels = Array.from({ length: rows * 4 }, (_, y) =>
              Array.from({ length: columns * 2 }, (_, x) =>
                !!(codes[Math.floor(y / 4) * columns + Math.floor(x / 2)] & bit[y % 4][x % 2])));
            const repaired = pixels.map(row => row.slice());
            for (let y = 1; y < rows * 4 - 1; y++) for (let x = 1; x < columns * 2 - 1; x++) {
              if (!pixels[y][x] && pixels[y - 1][x] && pixels[y + 1][x] && pixels[y][x - 1] && pixels[y][x + 1]) repaired[y][x] = true;
            }
            for (let y = 0; y < rows; y++) for (let x = 0; x < columns; x++) {
              let dot = 0;
              for (let dy = 0; dy < 4; dy++) for (let dx = 0; dx < 2; dx++) {
                if (repaired[y * 4 + dy][x * 2 + dx]) dot |= bit[dy][dx];
              }
              bytes.push(dot);
            }
          }
        }
        return { columns, rows, glyphs, bytes };
      }, sequence);
      if (result.columns !== width || result.glyphs !== 14) throw new Error(`unexpected geometry ${JSON.stringify(result)}`);
      fs.writeFileSync(path.join(output, `frames_${width}.bin`), Buffer.from(result.bytes));
      console.log(`${width}x${result.rows}: ${result.bytes.length} bytes`);
      await page.close();
    }
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
