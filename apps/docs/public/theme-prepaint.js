(() => {
  try {
    const stored = window.localStorage.getItem("ffdb-docs-theme");
    document.documentElement.dataset.theme = stored === "light" ? "light" : "dark";
  } catch {
    document.documentElement.dataset.theme = "dark";
  }
})();
