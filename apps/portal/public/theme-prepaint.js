(function () {
  var stored;
  try {
    stored = globalThis.localStorage.getItem("ffdb.portal.theme");
  } catch (_) {
    stored = null;
  }
  var theme = stored === "light" || stored === "dark"
    ? stored
    : globalThis.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  var root = document.documentElement;
  root.dataset.theme = theme;
  root.classList.toggle("dark", theme === "dark");
  root.style.colorScheme = theme;
  var meta = document.querySelector('meta[name="theme-color"]');
  if (meta !== null) meta.setAttribute("content", theme === "dark" ? "#121214" : "#f6f6f7");
})();
