import mdx from "@mdx-js/rollup";
import rehypeShiki from "@shikijs/rehype";
import react from "@vitejs/plugin-react";
import rehypeSlug from "rehype-slug";
import remarkGfm from "remark-gfm";
import { defineConfig } from "vite";
import ned from "./src/ned.tmLanguage.json" with { type: "json" };

export default defineConfig({
  base: "/neoed/",
  plugins: [
    {
      enforce: "pre",
      ...mdx({
        remarkPlugins: [remarkGfm],
        rehypePlugins: [
          rehypeSlug,
          [
            rehypeShiki,
            {
              themes: { light: "github-light", dark: "github-dark" },
              langs: ["sh", "rust", "python", "diff", "toml", "text", ned],
            },
          ],
        ],
      }),
    },
    react({ include: /\.(mdx|tsx?)$/ }),
  ],
});
