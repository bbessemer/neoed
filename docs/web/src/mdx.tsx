import type { MDXComponents } from "mdx/types";
import type { ComponentProps } from "react";
import { Link } from "react-router";

function MdxLink({ href = "", ...props }: ComponentProps<"a">) {
  if (href.startsWith("/")) return <Link to={href} {...props} />;
  return <a href={href} {...props} />;
}

export const components: MDXComponents = { a: MdxLink };
