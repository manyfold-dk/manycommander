// The screenshot views, the theme picker and the copy button. The site works without this
// file: the stylesheet follows prefers-color-scheme, and the hero shows the panels view. A
// picked theme is kept in localStorage and applied here, before the first paint, by setting
// the palette's CSS properties on <html>.
(() => {
  "use strict";
  const KEY = "manycommander-theme";
  const root = document.documentElement;

  // --accent-ink (see css/site.css): the accent, mixed toward black on a light theme or white
  // on a dark one in steps of 1/40, until it reaches 4.5:1 against both backgrounds.
  const rgb = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  const luminance = (c) => {
    const [r, g, b] = c.map((v) => ((v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const contrast = (a, b) => {
    const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
    return (hi + 0.05) / (lo + 0.05);
  };
  const ink = (theme) => {
    const accent = rgb(theme.colors.accent);
    const toward = theme.mode === "light" ? [0, 0, 0] : [255, 255, 255];
    const grounds = [theme.colors.background, theme.colors.dark_background].map(rgb);
    for (let step = 0; step <= 40; step++) {
      const mix = accent.map((v, i) => Math.round(v + ((toward[i] - v) * step) / 40));
      if (grounds.every((g) => contrast(mix, g) >= 4.5)) {
        return `#${mix.map((v) => v.toString(16).padStart(2, "0")).join("")}`;
      }
    }
    return theme.colors.bright_foreground;
  };

  const apply = (theme) => {
    root.removeAttribute("style");
    if (!theme) return;
    for (const [key, value] of Object.entries(theme.colors)) {
      root.style.setProperty(`--${key.replaceAll("_", "-")}`, value);
    }
    root.style.setProperty("--accent-ink", ink(theme));
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
    // The first view is the theme screenshot; the others carry their own src and alt.
    const views = [...document.querySelectorAll(".views button[data-view]")];
    const captions = [...document.querySelectorAll("[data-caption]")];
    let view = views[0] || null;

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
      for (const v of views) v.setAttribute("aria-pressed", String(v === view));
      for (const c of captions) c.hidden = view !== null && c.dataset.caption !== view.dataset.view;
      if (!shot) return;
      if (view && view.dataset.src) {
        shot.src = view.dataset.src;
        light.srcset = view.dataset.src;
        shot.alt = view.dataset.alt;
      } else if (current) {
        const src = defaults.src.replace(/[^/]+\.svg$/, `${current.name}.svg`);
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
        view = views[0] || null; // a picked theme shows on the panels view
        show();
      });
    }
    for (const v of views) {
      v.addEventListener("click", () => {
        view = v;
        show();
      });
    }
    for (const more of document.querySelectorAll(".picker .more")) {
      const picker = more.closest(".picker");
      const label = more.textContent;
      more.addEventListener("click", () => {
        const open = picker.classList.toggle("open");
        more.setAttribute("aria-expanded", String(open));
        more.textContent = open ? "fewer themes" : label;
      });
    }
    for (const el of document.querySelectorAll(".picker, .views")) el.hidden = false;
    show();

    // The docs page list: open beside the article on a wide screen, folded above it on a
    // narrow one (the stylesheet's 50rem breakpoint).
    const wide = matchMedia("(min-width: 50.01rem)");
    const lists = document.querySelectorAll("details.panel-list");
    const fold = () => {
      for (const list of lists) list.open = wide.matches;
    };
    fold();
    wide.addEventListener("change", fold);

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
