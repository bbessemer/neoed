import { useState } from "react";
import { Link } from "react-router";
import HeroExample from "../content/hero-example.mdx";
import HomeContent from "../content/home.mdx";
import { RELEASES, REPO } from "../links";
import { components } from "../mdx";

const INSTALL =
  "curl -fsSL https://raw.githubusercontent.com/bbessemer/neoed/main/install.sh | sh";

function InstallCommand() {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    await navigator.clipboard.writeText(INSTALL);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };
  return (
    <div className="install-command">
      <code>{INSTALL}</code>
      <button type="button" onClick={copy}>
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

export default function Home() {
  return (
    <>
      <section className="hero">
        <div className="hero-text">
          <h1>A smart editor for you and your agents</h1>
          <p className="lede">
            Neoed (<code>ned</code>) is a line editor that can address code by
            its syntax, not just line numbers and regex patterns. It brings all
            the features you&rsquo;d expect from a modern text editor, including
            LSP integration, to an interface that AI agents can use, and it
            works well in your own terminal, too.
          </p>
          <InstallCommand />
          <p className="install-more">
            <Link to="/tutorial#installing">Other ways to install</Link>
          </p>
          <div className="hero-links">
            <Link className="button primary" to="/tutorial">
              Tutorial
            </Link>
            <a className="button" href={RELEASES}>
              Download
            </a>
            <a className="button" href={REPO}>
              Source code
            </a>
          </div>
        </div>
        <div className="hero-example">
          <HeroExample />
        </div>
      </section>
      <div className="prose home">
        <aside className="home-note">
          <p>
            Modern text editors include language-aware tools that allow power
            users to perform complex edits with a few keystrokes. In comparison,
            the tools used by most AI agents are a step backwards: they require
            agents to waste tokens quoting code they already wrote, or can
            silently fail or introduce syntax errors. Neoed gives agents the
            tools humans already have in Vim, Emacs, or VS Code. For more, see{" "}
            <Link to="/why">Why ned?</Link>
          </p>
          <p>
            Neoed&rsquo;s name comes from{" "}
            <Link to="https://en.wikipedia.org/wiki/Ed_(text_editor)">
              <code>ed</code>
            </Link>
            , part of the original Unix operating system, which was designed for
            an{" "}
            <Link to="https://en.wikipedia.org/wiki/Teleprinter">
              interface
            </Link>{" "}
            with limitations similar to today&rsquo;s LLMs.
          </p>
        </aside>
        <HomeContent components={components} />
      </div>
    </>
  );
}
