# Zitadel login branding

What `../provision.sh` uploads into Zitadel's label policy, so the hosted
sign-in and sign-up look like romulus:

- `logo-light.png`, `logo-dark.png`: the wordmark per theme, drawn 250px wide
  (the login's logo box) with the mark at 28px.
- `icon-light.png`, `icon-dark.png`: the mark alone.
- `brand.json`: the colors (primary, background, destructive, foreground per
  theme), read by the provisioner.
- `inter.woff2`: Inter, the app's font (SIL Open Font License, `inter-LICENSE.txt`).

All of it is generated from `web/src/styles/globals.css` and
`web/src/components/brand/logo.tsx`:

```bash
npm --prefix web run zitadel:assets            # regenerate, then `just up` to upload
npm --prefix web run zitadel:assets -- --check # part of `just check`
```
