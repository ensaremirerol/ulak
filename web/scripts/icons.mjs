// Renders public/icon.svg to the PNG icons the manifest references. Run: npm run icons
import { Resvg } from '@resvg/resvg-js';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';

const svg = readFileSync(new URL('../public/icon.svg', import.meta.url), 'utf8');
// Maskable: full-bleed background, glyph inside the 80% safe zone.
const maskable = svg.replace(/<rect[^>]*\/>/, '<rect width="512" height="512" fill="#12151a"/>').replace('<path', '<g transform="translate(256 256) scale(0.7) translate(-256 -256)"><path').replace('/></svg>', '/></g></svg>');
const out = new URL('../public/icons/', import.meta.url);
mkdirSync(out, { recursive: true });
for (const [name, source, size] of [['icon-192.png', svg, 192], ['icon-512.png', svg, 512], ['maskable-512.png', maskable, 512]]) {
  writeFileSync(new URL(name, out), new Resvg(source, { fitTo: { mode: 'width', value: size } }).render().asPng());
}
