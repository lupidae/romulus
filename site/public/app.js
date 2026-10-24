// The site's only script (the homepage, the comparisons and the guides): the
// header border on scroll, a launch post's welcome, the hero scene starting
// when it is seen, the live hero, the copy buttons and the theme switch.
// Everything works without it; it only adds behaviour.

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

// A welcome under the header for visitors from a launch post (?ref=hn,
// ?utm_source=producthunt…): one line, until they close it. Nothing is
// recorded about the visit; only the closing is remembered, in this browser.
const SOURCES = { hn: "Hacker News", producthunt: "Product Hunt" };
const WELCOME_KEY = "romulus-welcome-closed";
const params = new URLSearchParams(location.search);
const ref = [params.get("ref"), params.get("utm_source")]
  .map((value) => value?.trim().toLowerCase())
  .find((value) => value && Object.hasOwn(SOURCES, value));
const siteHeader = document.querySelector(".site-header");
let closed = false;
try {
  closed = localStorage.getItem(WELCOME_KEY) === "1";
} catch {}
if (ref && siteHeader && !closed) {
  const line = document.createElement("aside");
  line.className = "welcome-line";
  line.setAttribute("aria-label", "Welcome");
  line.innerHTML = `<div class="container welcome-inner">
  <p>Welcome, <span data-welcome-source></span>: <a href="/#self-host">self-host free</a>, or try the <a href="/#pricing">founding-team offer</a>.</p>
  <button class="welcome-close" type="button" aria-label="Close the welcome"><svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M6.758 17.243L12.001 12m5.243-5.243L12 12m0 0L6.758 6.757M12.001 12l5.243 5.243"/></svg></button>
</div>`;
  line.querySelector("[data-welcome-source]").textContent = SOURCES[ref];
  line.querySelector("button").addEventListener("click", () => {
    try {
      localStorage.setItem(WELCOME_KEY, "1");
    } catch {}
    line.remove();
  });
  siteHeader.after(line);
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

// The live hero: romulus itself, running in the browser (/hero/, built from
// web/src/hero). On a desktop it loads once the page is idle; on a phone, or
// with data saving on, it waits for a tap (a megabyte nobody asked for). It
// takes the picture's place only once it is drawn, in the same box. If it
// fails to load, or stops later (it then forgets what it kept), the picture
// comes back with a word about it and a way to try again.
const stage = document.querySelector("[data-hero] .hero-stage");
if (stage) {
  const live = stage.querySelector("[data-hero-live]");
  const start = stage.querySelector("[data-hero-start]");
  const label = start?.querySelector("[data-hero-start-label]");
  const status = document.querySelector("[data-hero-status]");
  const picture = stage.querySelector(".product");
  let loading = null;
  let layers = null;

  const stylesheet = () =>
    new Promise((resolve, reject) => {
      if (document.querySelector('link[href="/hero/hero.css"]')) return resolve();
      const link = document.createElement("link");
      link.rel = "stylesheet";
      link.href = "/hero/hero.css";
      link.onload = resolve;
      link.onerror = () => {
        link.remove();
        reject(new Error("hero.css did not load"));
      };
      document.head.append(link);
    });

  const say = (text) => {
    if (status) status.textContent = text;
  };

  /** The picture again, and the button to try once more. */
  const fallBack = (message) => {
    stage.classList.remove("is-live");
    picture.removeAttribute("aria-hidden");
    live.hidden = true;
    layers?.remove();
    layers = null;
    loading = null;
    say(message);
    if (start) {
      start.hidden = false;
      start.removeAttribute("aria-busy");
      start.disabled = false;
      if (label) label.textContent = "Try again";
    }
  };

  const load = () => {
    if (loading) return loading;
    say("");
    // Menus, popovers and dialogs render here, outside the window that clips.
    layers = document.createElement("div");
    layers.className = "romulus-hero hero-layers";
    document.body.append(layers);
    live.hidden = false;
    const onCrash = () =>
      fallBack("The live editor ran into a problem and stopped. It starts over if you try again.");
    loading = Promise.all([import("/hero/hero.js"), stylesheet()])
      .then(([hero]) => hero.mount(live, layers, { onCrash }))
      .then(() => {
        stage.classList.add("is-live");
        picture.setAttribute("aria-hidden", "true");
        if (start) start.hidden = true;
      })
      .catch((error) => {
        console.warn("romulus: the live editor did not load, the picture stays", error);
        fallBack("The live editor did not load.");
      });
    return loading;
  };

  if (start && live) {
    start.addEventListener("click", () => {
      start.disabled = true;
      start.setAttribute("aria-busy", "true");
      if (label) label.textContent = "Loading the editor…";
      load();
    });
  }
  const saveData = navigator.connection?.saveData === true;
  const auto = !saveData && matchMedia("(min-width: 64rem) and (pointer: fine)").matches;
  if (auto) {
    const idle = () =>
      "requestIdleCallback" in window ? requestIdleCallback(load, { timeout: 3000 }) : setTimeout(load, 1500);
    if (document.readyState === "complete") idle();
    else addEventListener("load", idle, { once: true });
  } else if (start) {
    start.hidden = false;
  }
}

// Copy buttons: the text of the element named by data-copy, line by line as
// rendered (innerText: each block is a line, a wrap on a phone is not).
const isMac = /mac|iphone|ipad/i.test(navigator.userAgentData?.platform ?? navigator.platform ?? "");
for (const button of document.querySelectorAll("[data-copy]")) {
  const label = button.querySelector("[data-copy-label]");
  let reset;
  button.addEventListener("click", async () => {
    const source = document.querySelector(button.dataset.copy);
    if (!source) return;
    try {
      await navigator.clipboard.writeText(source.innerText.trim() + "\n");
      button.setAttribute("data-copied", "");
      if (label) label.textContent = "Copied";
    } catch {
      // No clipboard access: select the text and say how to copy it here.
      if (label) label.textContent = isMac ? "Press ⌘C" : "Press Ctrl+C";
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
