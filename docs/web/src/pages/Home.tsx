import { useState } from "react";
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
          <h1>Edit code by naming it.</h1>
          <p className="lede">
            ned is a line editor that finds code by its syntax. You give it
            files and a short script, and it makes the edit, fixes the
            indentation, runs your formatter and prints a diff. It was built for
            AI coding agents, and it works just as well in your own terminal.
          </p>
          <InstallCommand />
          <div className="hero-links">
            <a className="button primary" href="#refactoring-by-hand">
              See examples
            </a>
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
        <HomeContent components={components} />
      </div>
    </>
  );
}
