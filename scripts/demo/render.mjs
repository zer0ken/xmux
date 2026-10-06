// Renders the recordings in <dir> into frame sequences, one per README GIF.
//
// The stage page replays each recording in a terminal emulator; this script steps
// its clock frame by frame and screenshots it with a transparent background, so the
// video timing never depends on how fast the machine renders. It writes
// <dir>/manifest.json, which encode.py turns into the GIFs under <dir>/gifs, and
// saves a PNG still of the xmux window there.
//
// usage: node render.mjs <dir>
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { chromium } from "playwright";

const here = path.dirname(fileURLToPath(import.meta.url));
const inDir = path.resolve(process.argv[2]);
const FPS = 20;
const BURST = 0.03;   // the PTY splits one paint into chunks a few ms apart; draw a burst whole
const TAIL = 2.5;     // seconds kept after the last pane finishes
const HOLD_END = 1;   // seconds the last frame of a feature GIF holds before it loops

function load(name) {
  const lines = fs.readFileSync(path.join(inDir, name + ".cast"), "utf8").split(/\r?\n/).filter(Boolean);
  const head = JSON.parse(lines[0]);
  const events = [];
  for (const [t, kind, data] of lines.slice(1).map(l => JSON.parse(l))) {
    if (kind !== "o") continue;
    const last = events[events.length - 1];
    if (last && t - last[0] < BURST) { last[0] = t; last[1] += data; } else events.push([t, data]);
  }
  const meta = JSON.parse(fs.readFileSync(path.join(inDir, name + ".json"), "utf8"));
  return { cols: head.width, rows: head.height, events, ...meta };
}

async function capture(browser, spec, from, to, dir, label = null) {
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  const page = await browser.newPage({ viewport: { width: 2400, height: 1200 }, deviceScaleFactor: 1 });
  await page.goto(pathToFileURL(path.join(here, "stage.html")).href);
  await page.evaluate(() => document.fonts.ready);
  const done = await page.evaluate(s => setup(s), spec);
  await page.evaluate(([f, l]) => setWindow(f, l), [from ?? -1e9, label]);
  const stage = await page.$("#stage");
  const start = from ?? 0, end = to ?? Math.max(...done) + TAIL;
  const n = Math.ceil((end - start) * FPS);
  const layouts = new Set();
  for (let i = 0; i <= n; i++) {
    await page.evaluate(t => frame(t), start + i / FPS);
    layouts.add(await page.evaluate(() => layout()));
    await stage.screenshot({ path: path.join(dir, String(i).padStart(4, "0") + ".png"), omitBackground: true });
  }
  if (layouts.size !== 1) throw new Error(`${dir}: a terminal moved between frames (${[...layouts].join(" / ")})`);
  await page.close();
  const sizes = new Set(fs.readdirSync(dir).map(f => {
    const b = fs.readFileSync(path.join(dir, f));
    return b.readUInt32BE(16) + "x" + b.readUInt32BE(20);
  }));
  if (sizes.size !== 1) throw new Error(`${dir}: frames differ in size (${[...sizes]})`);
}

const jobs = [];
function addGif(dir, gif, holdEnd) {
  jobs.push({ frames: path.relative(inDir, dir), gif: path.join("gifs", gif), fps: FPS, hold: holdEnd });
}

// One frame of a single pane, without the key strip, as a full-colour PNG.
async function still(browser, rec, t, file) {
  const page = await browser.newPage({ viewport: { width: 2400, height: 1200 }, deviceScaleFactor: 1 });
  await page.goto(pathToFileURL(path.join(here, "stage.html")).href);
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(s => setup(s), [{ title: "xmux", timer: false, pulse: null, rec }]);
  await page.evaluate(() => { setWindow(1e9, ""); document.querySelectorAll(".foot").forEach(f => f.remove()); });
  await page.evaluate(v => frame(v), t);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  await (await page.$("#stage")).screenshot({ path: file, omitBackground: true });
  await page.close();
  console.log(`${path.basename(file)}: one frame at ${t.toFixed(2)} s`);
}

const browser = await chromium.launch();
const work = path.join(inDir, "frames");

// Side by side: the same session reached by hand and through xmux.
const manual = load("compare-manual"), viaXmux = load("compare-xmux");
await capture(browser, [
  { title: "ssh + tmux", timer: true, pulse: "243,139,168", rec: manual },
  { title: "xmux", timer: true, pulse: "166,227,161", rec: viaXmux },
], null, null, path.join(work, "compare"));
addGif(path.join(work, "compare"), "xmux-demo.gif", 0);
console.log(`  ssh + tmux ${(manual.done - manual.keys[0][0]).toFixed(1)}s, xmux ${(viaXmux.done - viaXmux.keys[0][0]).toFixed(1)}s`);

// One GIF per captioned feature, cut from a single recording.
const tour = load("features");
const lead = t => t - tour.keys[0][0] + 0.8;
const caps = tour.captions.map(([t, c]) => [lead(t), c]);
const FEATURES = {
  landing: "Open a session from the landing screen", "nav-switch": "Switch sessions",
  hierarchy: "Walk up to the source and the host", "nav-resize": "Resize the nav",
  "nav-place": "Place the nav", "nav-autohide": "Auto-hide the nav",
};
for (const [name, label] of Object.entries(FEATURES)) {
  const i = caps.findIndex(([, c]) => c === label);
  if (i < 0 || i + 1 >= caps.length) throw new Error(`features recording has no "${label}" segment`);
  const dir = path.join(work, name);
  await capture(browser, [{ title: "xmux", timer: false, pulse: null, rec: tour }],
    caps[i][0] - 0.4, caps[i + 1][0] - 0.06, dir, label);
  addGif(dir, `xmux-${name}.gif`, HOLD_END);
}

// The login has a recording of its own, because its server is in no other scenario.
const login = load("login");
await capture(browser, [{ title: "xmux", timer: false, pulse: null, rec: login }],
  null, null, path.join(work, "login"), login.captions[0][1]);
addGif(path.join(work, "login"), "xmux-login.gif", HOLD_END);

// The xmux window alone, after the session switch settles and before the resize starts.
const resize = caps.find(([, c]) => c === "Resize the nav");
await still(browser, tour, resize[0] - 0.1, path.join(inDir, "gifs", "xmux.png"));
await browser.close();
fs.mkdirSync(path.join(inDir, "gifs"), { recursive: true });
fs.writeFileSync(path.join(inDir, "manifest.json"), JSON.stringify(jobs.map(j => ({ ...j,
  frames: j.frames.split(path.sep).join("/"), gif: j.gif.split(path.sep).join("/") })), null, 1));
