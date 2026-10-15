// The homepage's only script: the header border on scroll, the hero scene
// starting when it is seen, the copy button and the theme switch. Everything
// works without it; it only adds behaviour.

// Header: a hairline once the page scrolls under it.
const header = document.querySelector("[data-header]");
if (header) {
  // Read scrollY in a frame, never during load: reading it there forces a
  // layout of the whole page before its first paint.
  let queued = false;
  const update = () => {
    queued = false;
    header.classList.toggle("is-scrolled", window.scrollY > 4);
  };
  const queue = () => {
    if (!queued) {
      queued = true;
      requestAnimationFrame(update);
    }
  };
  queue();
  window.addEventListener("scroll", queue, { passive: true });
}

// The product scene holds its first frame until at least a third of it is on
// screen, so a visitor who scrolls to it on a phone still sees it play.
const product = document.querySelector(".product");
if (product) {
  const play = () => product.classList.add("is-playing");
  if (!("IntersectionObserver" in window)) {
    play();
  } else {
    const seen = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          play();
          seen.disconnect();
        }
      },
      { threshold: 0.35 },
    );
    seen.observe(product);
  }
}

// Copy buttons: the text of the element named by data-copy.
for (const button of document.querySelectorAll("[data-copy]")) {
  const label = button.querySelector("[data-copy-label]");
  let reset;
  button.addEventListener("click", async () => {
    const source = document.querySelector(button.dataset.copy);
    if (!source) return;
    try {
      await navigator.clipboard.writeText(source.textContent.trim() + "\n");
      button.setAttribute("data-copied", "");
      if (label) label.textContent = "Copied";
    } catch {
      if (label) label.textContent = "Press ⌘C";
      const range = document.createRange();
      range.selectNodeContents(source);
      getSelection()?.removeAllRanges();
      getSelection()?.addRange(range);
    }
    clearTimeout(reset);
    reset = setTimeout(() => {
      button.removeAttribute("data-copied");
      if (label) label.textContent = "Copy";
    }, 2000);
  });
}

// Theme: the same key and values as the app (web/src/lib/theme.ts), so a
// choice made here holds there too. "system" removes the key.
const KEY = "romulus-theme";
const root = document.documentElement;
const apply = (theme) => {
  root.classList.toggle("light", theme === "light");
  root.classList.toggle("dark", theme === "dark");
};
const read = () => {
  try {
    const value = localStorage.getItem(KEY);
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
};
const switcher = document.querySelector("[data-theme-switch]");
if (switcher) {
  const current = switcher.querySelector(`input[value="${read()}"]`);
  if (current) current.checked = true;
  switcher.addEventListener("change", (event) => {
    const theme = event.target.value;
    try {
      if (theme === "system") localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, theme);
    } catch {
      // Private mode: the choice lasts for this page only.
    }
    apply(theme);
  });
}
