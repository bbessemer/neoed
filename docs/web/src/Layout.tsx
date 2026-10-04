import { Suspense, useEffect } from "react";
import { Link, NavLink, Outlet, useLocation } from "react-router";
import { LICENSE, RELEASES, REPO } from "./links";

const pages = [
  { to: "/tutorial", label: "Tutorial" },
  { to: "/reference", label: "Reference" },
];

export default function Layout() {
  const { pathname, hash } = useLocation();

  useEffect(() => {
    if (hash) {
      document
        .getElementById(decodeURIComponent(hash.slice(1)))
        ?.scrollIntoView();
    } else {
      window.scrollTo(0, 0);
    }
  }, [pathname, hash]);

  return (
    <>
      <header className="site-header">
        <div className="site-header-inner">
          <Link to="/" className="brand">
            <span className="brand-mark">ned</span>
            <span className="brand-name">Neoed</span>
          </Link>
          <nav className="site-nav">
            {pages.map(({ to, label }) => (
              <NavLink key={to} to={to}>
                {label}
              </NavLink>
            ))}
            <a href={RELEASES}>Releases</a>
            <a href={REPO}>GitHub</a>
          </nav>
        </div>
      </header>
      <main>
        <Suspense fallback={null}>
          <Outlet />
        </Suspense>
      </main>
      <footer className="site-footer">
        <p>
          Neoed is free software under the <a href={LICENSE}>MIT license</a>.
          The source is on <a href={REPO}>GitHub</a>, and binaries for Linux and
          macOS are on the <a href={RELEASES}>releases page</a>.
        </p>
      </footer>
    </>
  );
}
