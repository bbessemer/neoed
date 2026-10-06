import type { MDXContent } from "mdx/types";
import { lazy, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Route, Routes } from "react-router";
import DocPage from "./DocPage";
import Layout from "./Layout";
import Home from "./pages/Home";
import NotFound from "./pages/NotFound";
import "./styles.css";

function docPage(
  load: () => Promise<{ default: MDXContent }>,
  title: string,
  tocDepth?: 2 | 3,
) {
  return lazy(async () => {
    const { default: Content } = await load();
    return {
      default: () => (
        <DocPage Content={Content} title={title} tocDepth={tocDepth} />
      ),
    };
  });
}

const Why = docPage(() => import("./content/why.mdx"), "Why ned?", 3);
const Tutorial = docPage(() => import("./content/tutorial.mdx"), "Tutorial");
const Reference = docPage(
  () => import("./content/reference.mdx"),
  "Reference",
  3,
);

const Agents = docPage(
  () => import("./content/agents.mdx"),
  "Using ned with AI agents",
);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <BrowserRouter basename={import.meta.env.BASE_URL}>
      <Routes>
        <Route element={<Layout />}>
          <Route index element={<Home />} />
          <Route path="why" element={<Why />} />
          <Route path="tutorial" element={<Tutorial />} />
          <Route path="reference" element={<Reference />} />
          <Route path="agents" element={<Agents />} />
          <Route path="*" element={<NotFound />} />
        </Route>
      </Routes>
    </BrowserRouter>
  </StrictMode>,
);
