import { Link } from "react-router";

export default function NotFound() {
  return (
    <div className="prose not-found">
      <h1>Page not found</h1>
      <p>
        There is nothing at this address. The <Link to="/">home page</Link>{" "}
        links to everything on the site.
      </p>
    </div>
  );
}
