#!/usr/bin/env node

'use strict';

// Renders every raster icon from the SVG masters in icons/source.
//
// The renderer is @resvg/resvg-js. It is a development-time tool and is not a
// dependency of the repository: install it anywhere and point NODE_PATH at the
// resulting node_modules directory, for example:
//
//   npm install --prefix /tmp/icon-render @resvg/resvg-js
//   NODE_PATH=/tmp/icon-render/node_modules node scripts/render-app-icons.js
//
// Small sizes use simplified artwork so the mark keeps its shape: the full
// master is used from 48 px up, the medium master from 25 px up, and the
// small master below that. Tray icons use the single-color mark.

const fs = require('node:fs');
const path = require('node:path');
const { Resvg } = require('@resvg/resvg-js');
const { encodeIco } = require('./generate-app-icon.js');

const root = path.resolve(__dirname, '..');
const iconsDir = path.join(root, 'icons');
const sourceDir = path.join(iconsDir, 'source');
const manifest = JSON.parse(fs.readFileSync(path.join(iconsDir, 'icon-manifest.json'), 'utf8'));

const FULL_FROM = 48;
const MEDIUM_FROM = 25;
const TRAY_LIGHT = '#F5F5F4';
const TRAY_DARK = '#1C1917';

const readSvg = (name) => fs.readFileSync(path.join(sourceDir, name), 'utf8');
const masters = {
  full: readSvg('app-icon.svg'),
  medium: readSvg('app-icon-medium.svg'),
  small: readSvg('app-icon-small.svg'),
  tray: readSvg('tray-mark.svg'),
};

function tierFor(size) {
  if (size >= FULL_FROM) return masters.full;
  if (size >= MEDIUM_FROM) return masters.medium;
  return masters.small;
}

function render(svg, size, background) {
  const options = { fitTo: { mode: 'width', value: size } };
  if (background) options.background = background;
  return new Resvg(svg, options).render().asPng();
}

function svgBody(svg) {
  return svg.replace(/^[\s\S]*?<svg[^>]*>/, '').replace(/<\/svg>\s*$/, '');
}

// The tile on an opaque white square, as used for stores and web manifests.
function whiteSquare(size) {
  const inner = svgBody(tierFor(size));
  const svg = [
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120" fill="none">',
    '<rect width="120" height="120" fill="#fff"/>',
    `<g transform="translate(7.2 7.2) scale(0.88)">${inner}</g>`,
    '</svg>',
  ].join('');
  return render(svg, size, '#ffffff');
}

function trayMark(size, color) {
  return render(masters.tray.replaceAll('#000', color), size);
}

function write(directory, name, buffer) {
  fs.writeFileSync(path.join(directory, name), buffer);
  console.log(`wrote ${path.relative(root, path.join(directory, name))}`);
}

function writeIco(name, frameFor) {
  const frames = manifest[name].map((size) => ({ size, png: frameFor(size) }));
  fs.writeFileSync(path.join(iconsDir, name), encodeIco(frames));
  console.log(`wrote icons/${name} (${manifest[name].join(', ')})`);
}

for (const size of [16, 32, 48, 64, 128, 256, 512, 1024]) {
  write(sourceDir, `app-icon-transparent-${size}.png`, render(tierFor(size), size));
}
for (const size of [16, 32, 48, 64, 128, 150, 180, 192, 256, 512, 1024]) {
  write(sourceDir, `app-icon-white-${size}.png`, whiteSquare(size));
}
write(sourceDir, 'android-chrome-192x192.png', whiteSquare(192));
write(sourceDir, 'android-chrome-512x512.png', whiteSquare(512));
write(sourceDir, 'apple-touch-icon.png', whiteSquare(180));
write(sourceDir, 'mstile-150x150.png', whiteSquare(150));
write(sourceDir, 'favicon-16x16.png', render(tierFor(16), 16));
write(sourceDir, 'favicon-32x32.png', render(tierFor(32), 32));
write(sourceDir, 'app-icon.png', render(masters.full, 512));

const faviconFrames = [16, 32, 48].map((size) => ({ size, png: render(tierFor(size), size) }));
fs.writeFileSync(path.join(sourceDir, 'favicon.ico'), encodeIco(faviconFrames));
console.log('wrote icons/source/favicon.ico (16, 32, 48)');

writeIco('app.ico', (size) => render(tierFor(size), size));
writeIco('tray-color.ico', (size) => render(tierFor(size), size));
writeIco('tray-light.ico', (size) => trayMark(size, TRAY_LIGHT));
writeIco('tray-dark.ico', (size) => trayMark(size, TRAY_DARK));
