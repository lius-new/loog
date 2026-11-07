# Application icons

Loog's canonical icon assets now live in `../icons`.

`../icons/app.ico` is the standard Windows application icon used by the
executable, taskbar, Start menu, window title bar, and Alt+Tab. It contains
32-bit transparent images at 16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72,
80, 96, 128, and 256 pixels.

Notification-area variants are provided separately because Tray icons are
displayed at much smaller sizes:

- `../icons/tray-color.ico` — simplified color icon.
- `../icons/tray-light.ico` — light icon for dark taskbars.
- `../icons/tray-dark.ico` — dark icon for light taskbars.

The canonical artwork is the set of SVG masters in `../icons/source`:

- `app-icon.svg` — full tile icon, used from 48 px up.
- `app-icon-medium.svg` — heavier stroke, used from 25 to 47 px.
- `app-icon-small.svg` — one simplified wave and a heavier stroke, used up to 24 px.
- `tray-mark.svg` — single-color mark without a tile, used for the light and
  dark tray icons.
- `logo-mark.svg` — the mark without a tile, for use inside the application UI.

All PNG and ICO files are generated from those masters, picking the artwork
that suits each size so the mark keeps its shape at 16 px:

```console
npm install --prefix /tmp/icon-render @resvg/resvg-js
NODE_PATH=/tmp/icon-render/node_modules node ./scripts/render-app-icons.js
```

`scripts/generate-app-icon.js` remains available to build an ICO by resampling
a single PNG.
