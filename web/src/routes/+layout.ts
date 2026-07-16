// Flaborful is a logged-in app: the session (and therefore every data fetch) lives in
// localStorage, which doesn't exist on the server. Render entirely on the client so
// adapter-static emits a pure SPA shell rather than trying to prerender authed routes.
export const ssr = false;
export const prerender = false;
