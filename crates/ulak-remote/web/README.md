Static PWA shell served by ulak-remote when `ULAK_WEB_DIR` points at a directory built from this one.

Files the web build must provide next to these (copy this directory, then add):
- `index.html`: links `/manifest.json`, registers `/sw.js`, loads `/ulak.js` as a module (CSP: no inline
  script or `<style>`; use external files; `el.style.x = ...` from JS is fine).
- `ulak.js` and `ulak_bg.wasm` (wasm-bindgen output; served as `application/wasm`).
Shipped here: `manifest.json`, `sw.js`, `icon.svg` (placeholder; replace with PNGs if a store needs them).
