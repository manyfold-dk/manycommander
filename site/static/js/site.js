// The theme picker and the copy button. The site works without this file: the stylesheet
// follows prefers-color-scheme. A picked theme is kept in localStorage and applied here,
// before the first paint, by setting the palette's CSS properties on <html>.
(() => {
  "use strict";
  const KEY = "manycommander-theme";
  const root = document.documentElement;

  const apply = (theme) => {
    root.removeAttribute("style");
    if (!theme) return;
    for (const [key, value] of Object.entries(theme.colors)) {
      root.style.setProperty(`--${key.replaceAll("_", "-")}`, value);
    }
    root.style.setProperty("color-scheme", theme.mode);
  };

  const load = () => {
    try {
      const theme = JSON.parse(localStorage.getItem(KEY));
      return theme && theme.name && theme.colors ? theme : null;
    } catch {
      return null;
    }
  };

  const save = (theme) => {
    try {
      if (theme) localStorage.setItem(KEY, JSON.stringify(theme));
      else localStorage.removeItem(KEY);
    } catch {
      // Storage can be off (private windows); the choice then lasts for this page only.
    }
  };

  let current = load();
  apply(current);

  document.addEventListener("DOMContentLoaded", () => {
    const shot = document.getElementById("shot");
    const light = document.getElementById("shot-light");
    const defaults = shot && { src: shot.getAttribute("src"), light: light.getAttribute("srcset"), alt: shot.alt };
    const buttons = [...document.querySelectorAll(".picker button[data-theme]")];

    const themeOf = (button) =>
      button.dataset.theme
        ? { name: button.dataset.theme, title: button.dataset.title, mode: button.dataset.mode, colors: JSON.parse(button.dataset.colors) }
        : null;

    const show = () => {
      for (const b of buttons) b.setAttribute("aria-pressed", String(b.dataset.theme === (current ? current.name : "")));
      for (const label of document.querySelectorAll("[data-theme-label]")) {
        label.textContent = current ? current.name : "system";
        label.parentElement.hidden = !current;
      }
      if (!shot) return;
      if (current) {
        const src = shot.getAttribute("src").replace(/[^/]+\.svg$/, `${current.name}.svg`);
        shot.src = src;
        light.srcset = src;
        shot.alt = `${defaults.alt}, in the ${current.title} theme`;
      } else {
        shot.src = defaults.src;
        light.srcset = defaults.light;
        shot.alt = defaults.alt;
      }
    };

    for (const b of buttons) {
      if (b.dataset.colors) {
        const colors = JSON.parse(b.dataset.colors);
        b.style.setProperty("--sw-bg", colors.background);
        b.style.setProperty("--sw-fg", colors.accent);
      }
      b.addEventListener("click", () => {
        current = themeOf(b);
        apply(current);
        save(current);
        show();
      });
    }
    for (const picker of document.querySelectorAll(".picker")) picker.hidden = false;
    show();

    for (const button of document.querySelectorAll("button[data-copy]")) {
      button.hidden = false;
      button.addEventListener("click", async () => {
        const text = document.getElementById(button.dataset.copy).textContent.trim();
        try {
          await navigator.clipboard.writeText(text);
          button.textContent = "copied";
        } catch {
          button.textContent = "select it";
        }
        setTimeout(() => (button.textContent = "copy"), 1600);
      });
    }
  });
})();
