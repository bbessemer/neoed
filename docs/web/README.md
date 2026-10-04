# Project website

The site at <https://bbessemer.github.io/neoed/>, built with React, Vite and
MDX. The prose lives in `src/content/*.mdx`. Code blocks are highlighted at
build time by Shiki; `ned` blocks use the grammar in `src/ned.tmLanguage.json`.

```sh
pnpm install
pnpm dev      # http://localhost:5173/neoed/
pnpm build    # type-check, then build into dist/
```

`.github/workflows/pages.yml` builds the site on pull requests that touch it and
deploys it to GitHub Pages from `main`. The repository's Pages source must be
set to GitHub Actions.
