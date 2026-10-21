# site/legal/: the hosted service's legal documents

The terms of service, the privacy policy and the data processing agreement of
the hosted service (romulus.lupidae.com), as approved. They are the one source
of what is published, word for word. There are two editions (the legal
session's notes call them the professional-only variant and the consumer
version):

**The professional edition, in force** (romulus is sold to professionals
only), at the canonical addresses, in the sitemap, linked from every footer
and from the legal notice:

| source | page | PDF the API attaches to its confirmation emails |
|---|---|---|
| `terms-pro.en.md` | `/legal/terms` | `crates/romulus-api/legal/terms-pro-<version>-en.pdf` |
| `conditions-pro.fr.md` | `/fr/legal/conditions` | `crates/romulus-api/legal/terms-pro-<version>-fr.pdf` |
| `privacy-pro.en.md` | `/legal/privacy` | |
| `confidentialite-pro.fr.md` | `/fr/legal/confidentialite` | |
| `dpa.en.md` | `/legal/dpa` (English only; one DPA for both editions) | |

**The full edition, consumers included, not in force**: published for later
under `/legal/full/`, with `noindex`, out of the sitemap, linked from nowhere,
and a notice above the document (page chrome, not part of it) saying it is not
in force and linking the Terms and the privacy policy that are:

| source | page | PDF |
|---|---|---|
| `terms.en.md` | `/legal/full/terms` | `crates/romulus-api/legal/terms-<version>-en.pdf` |
| `conditions.fr.md` | `/fr/legal/full/conditions` | `crates/romulus-api/legal/terms-<version>-fr.pdf` |
| `privacy.en.md` | `/legal/full/privacy` | |
| `confidentialite.fr.md` | `/fr/legal/full/confidentialite` | |

Each document is one entry of `documents` in `site/tools/legal.mjs` (source,
language, path, its version in the other language, `inForce: false` for an
edition published for later, and for an edition of the Terms the name of its
PDFs: `<pdf>-<version>-<lang>.pdf`); a new one is an entry there, a line in
`public/sitemap.xml` if it is in force (the check says so either way) and its
case in `e2e/legal.spec.ts`. The footer's legal links are `legalLinks` there
too: the tool writes them into the legal pages and `404.html` (the homepage's
footer is `index.html`'s own, kept in step by hand).

Edit the texts here, never the pages, then render:

```bash
node site/tools/legal.mjs            # just site-legal: the pages, and the Terms PDFs whose text or page changed
node site/tools/legal.mjs --pdf      # print the PDFs again anyway (after a change to the print styles)
node site/tools/legal.mjs --check    # just check and just site-check: everything made from these files?
```

and commit what it writes. `manifest.json` records which text and page each
PDF was printed from (the check compares hashes: PDF bytes are not
reproducible).

A new version of a document is a new `**Version:**` line (and effective date)
in it, so new Terms PDFs, and the API's `ROMULUS_TERMS_VERSION` once it is in
force. The previous PDFs stay: every version remains available on request.

**When consumer sales open**:
the full edition becomes a new version (1.1, say) of the documents at the
canonical addresses. Update `terms.en.md`, `conditions.fr.md`, `privacy.en.md`
and `confidentialite.fr.md` as counsel says (carrying over what the
professional edition improved), then in `documents` point `/legal/terms`,
`/fr/legal/conditions`, `/legal/privacy` and `/fr/legal/confidentialite` at
them, and drop the `/legal/full/` entries and the professional ones (keep their
sources and PDFs: the versions people accepted stay available). Run the tool,
then remove the stale `public/legal/full/` and `public/fr/legal/full/` pages it
no longer writes, and set the API's Terms version and edition to match.

Markdown as GitHub writes it (tables, bare URLs and addresses become links);
a line break inside a paragraph is kept, and raw HTML is shown as text.
