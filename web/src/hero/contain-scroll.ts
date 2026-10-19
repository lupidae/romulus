// Inside the island, bringing something into view moves the island's own
// scrollers (the comments panel, a change list), never the homepage: opening
// a thread must not carry the visitor away from where they are reading.

type ScrollIntoView = (this: Element, options?: boolean | ScrollIntoViewOptions) => void;
const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollIntoView")
  ?.value as ScrollIntoView;

/**
 * Brings `el` into view in the page, as the browser does (the page's scroll
 * padding kept clear of its sticky header): for what the visitor asked to
 * see, such as the result of a step of the guide.
 */
export function revealInPage(el: Element, block: ScrollLogicalPosition = "nearest") {
  const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  native.call(el, { block, inline: "nearest", behavior: still ? "auto" : "smooth" });
}

/**
 * `scrollIntoView` for elements inside `selector`: the nearest scrollable
 * ancestor within it scrolls, the page does not. Elsewhere, unchanged.
 */
export function containScroll(selector: string) {
  Element.prototype.scrollIntoView = function (
    this: Element,
    options?: boolean | ScrollIntoViewOptions,
  ) {
    const island = this.closest(selector);
    if (!island) {
      native.call(this, options);
      return;
    }
    const block =
      typeof options === "object"
        ? (options.block ?? "start")
        : options === false
          ? "end"
          : "start";
    const behavior = typeof options === "object" ? options.behavior : undefined;
    for (let el = this.parentElement; el && el !== island; el = el.parentElement) {
      const overflow = getComputedStyle(el).overflowY;
      if (!/(auto|scroll)/.test(overflow) || el.scrollHeight <= el.clientHeight) continue;
      const box = el.getBoundingClientRect();
      const rect = this.getBoundingClientRect();
      let delta = 0;
      if (block === "nearest") {
        if (rect.top < box.top) delta = rect.top - box.top;
        else if (rect.bottom > box.bottom)
          delta = Math.min(rect.bottom - box.bottom, rect.top - box.top);
      } else if (block === "center")
        delta = rect.top + rect.height / 2 - (box.top + box.height / 2);
      else if (block === "end") delta = rect.bottom - box.bottom;
      else delta = rect.top - box.top;
      if (delta) el.scrollBy({ top: delta, behavior });
      return;
    }
  };
}
