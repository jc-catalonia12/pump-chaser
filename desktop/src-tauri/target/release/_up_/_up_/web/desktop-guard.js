/** Disable context menu and devtools shortcuts in the Tauri desktop shell. */
(function () {
  const inTauri =
    typeof window.__TAURI__ !== "undefined" ||
    typeof window.__TAURI_INTERNALS__ !== "undefined" ||
    typeof window.__TAURI_METADATA__ !== "undefined";
  if (!inTauri) return;

  document.addEventListener(
    "contextmenu",
    (e) => {
      e.preventDefault();
    },
    { capture: true },
  );

  document.addEventListener(
    "keydown",
    (e) => {
      const key = e.key || "";
      if (key === "F12") {
        e.preventDefault();
        return;
      }
      // macOS: Cmd+Option+I/J/C
      if (e.metaKey && e.altKey && /^(i|j|c)$/i.test(key)) {
        e.preventDefault();
        return;
      }
      // Windows/Linux: Ctrl+Shift+I/J/C
      if (e.ctrlKey && e.shiftKey && /^(i|j|c)$/i.test(key)) {
        e.preventDefault();
      }
    },
    { capture: true },
  );
})();
