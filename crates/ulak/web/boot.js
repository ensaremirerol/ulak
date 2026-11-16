// Loads the wasm app. A separate file because the hub CSP forbids inline scripts.
import init from "/ulak.js";
const status = document.querySelector("#status");
try {
  if (!navigator.gpu) throw new Error("WebGPU is required (a recent Chrome, Edge or Safari, over https or localhost).");
  await init();
  status.remove();
} catch (error) {
  status.textContent = String(error);
  console.error(error);
}
