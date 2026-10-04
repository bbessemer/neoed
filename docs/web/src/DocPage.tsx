import type { MDXContent } from "mdx/types";
import { useEffect, useRef, useState } from "react";
import { components } from "./mdx";

type Heading = { id: string; text: string; level: number };

export default function DocPage({
  Content,
  title,
  tocDepth = 2,
}: {
  Content: MDXContent;
  title: string;
  tocDepth?: 2 | 3;
}) {
  const article = useRef<HTMLElement>(null);
  const [headings, setHeadings] = useState<Heading[]>([]);
  const [active, setActive] = useState("");

  useEffect(() => {
    document.title = `${title} · Neoed`;
    return () => {
      document.title = "Neoed";
    };
  }, [title]);

  useEffect(() => {
    const selector = tocDepth === 3 ? "h2, h3" : "h2";
    const elements = [
      ...article.current!.querySelectorAll<HTMLElement>(selector),
    ];
    setHeadings(
      elements.map((h) => ({
        id: h.id,
        text: h.textContent ?? "",
        level: Number(h.tagName[1]),
      })),
    );

    const target = decodeURIComponent(window.location.hash.slice(1));
    if (target) document.getElementById(target)?.scrollIntoView();

    const onScroll = () => {
      const passed = elements.filter(
        (h) => h.getBoundingClientRect().top < 120,
      );
      setActive(passed.at(-1)?.id ?? "");
    };
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, [Content, tocDepth]);

  return (
    <div className="doc">
      <nav className="toc" aria-label="On this page">
        <p className="toc-title">{title}</p>
        <ul>
          {headings.map((h) => (
            <li
              key={h.id}
              className={`toc-level-${h.level}${h.id === active ? " active" : ""}`}
            >
              <a href={`#${h.id}`}>{h.text}</a>
            </li>
          ))}
        </ul>
      </nav>
      <article ref={article} className="prose">
        <Content components={components} />
      </article>
    </div>
  );
}
