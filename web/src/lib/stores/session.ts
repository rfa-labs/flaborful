import { writable, derived, get } from 'svelte/store';
import { browser } from '$app/environment';

export type Portal = 'professional' | 'company';

interface Session {
  token: string | null;
  userId: string | null;
  role: string | null;
  portal: Portal;
  professionalId: string | null;
  agentId: string | null;
  companyId: string | null;
}

const STORAGE_KEY = 'flaborful_session';

function loadFromStorage(): Session {
  if (!browser) return defaultSession();
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw);
  } catch {}
  return defaultSession();
}

function defaultSession(): Session {
  return { token: null, userId: null, role: null, portal: 'professional', professionalId: null, agentId: null, companyId: null };
}

function createSession() {
  const { subscribe, set, update } = writable<Session>(loadFromStorage());

  function persist(s: Session) {
    if (browser) localStorage.setItem(STORAGE_KEY, JSON.stringify(s));
    return s;
  }

  return {
    subscribe,
    setToken: (token: string, userId: string, role: string) =>
      update(s => persist({ ...s, token, userId, role, portal: role === 'company' ? 'company' : 'professional' })),
    setPortal: (portal: Portal) => update(s => persist({ ...s, portal })),
    setIds: (ids: Partial<Session>) => update(s => persist({ ...s, ...ids })),
    clear: () => {
      if (browser) localStorage.removeItem(STORAGE_KEY);
      set(defaultSession());
    },
  };
}

export const session = createSession();
export const portal = derived(session, $s => $s.portal);
export const isLoggedIn = derived(session, $s => !!$s.token);

/** Read the current token synchronously (for use in API client). */
export function getToken(): string | null {
  return get(session).token;
}
