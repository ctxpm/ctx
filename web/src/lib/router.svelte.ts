// A two-route router on the History API: "/" and "/package/<name>".

export type Route = { page: 'home' } | { page: 'package'; name: string } | { page: 'missing' };

function parse(path: string): Route {
  if (path === '/' || path === '') {
    return { page: 'home' };
  }
  const match = /^\/package\/([a-z0-9-]+)\/?$/.exec(path);
  return match?.[1] ? { page: 'package', name: match[1] } : { page: 'missing' };
}

export const router = $state({ route: parse(location.pathname) });

export function navigate(path: string): void {
  if (path !== location.pathname) {
    history.pushState(null, '', path);
  }
  router.route = parse(path);
  window.scrollTo(0, 0);
}

window.addEventListener('popstate', () => {
  router.route = parse(location.pathname);
});

/** Lets plain links to pages of this app navigate without a page load. */
export function interceptLinks(event: MouseEvent): void {
  if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
    return;
  }
  const anchor = (event.target as Element | null)?.closest?.('a[href]');
  if (!(anchor instanceof HTMLAnchorElement) || anchor.target || anchor.origin !== location.origin) {
    return;
  }
  if (parse(anchor.pathname).page !== 'missing') {
    event.preventDefault();
    navigate(anchor.pathname);
  }
}
