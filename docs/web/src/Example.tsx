import { Children, isValidElement, type ReactNode } from "react";

/** Lays out an MDX example: its prose beside its code blocks. */
export default function Example({ children }: { children: ReactNode }) {
  const items = Children.toArray(children);
  // MDX passes headings and paragraphs as plain elements, and highlighted code
  // blocks as components.
  const isCode = (child: ReactNode) =>
    isValidElement(child) && typeof child.type !== "string";
  return (
    <section className="example">
      <div className="example-text">{items.filter((c) => !isCode(c))}</div>
      <div className="example-code">{items.filter(isCode)}</div>
    </section>
  );
}
