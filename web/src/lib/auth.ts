import { derived, get, writable } from 'svelte/store';
import { api, onUnauthorized, setCsrf } from './api';
import type { User } from './types';

export interface AuthState {
  /** false until the first /auth/state answer arrives */
  loaded: boolean;
  /** the last attempt to reach the server failed */
  unreachable: string | null;
  setup_required: boolean;
  user: User | null;
  permissions: string[];
  csrf: string | null;
}

const initial: AuthState = {
  loaded: false,
  unreachable: null,
  setup_required: false,
  user: null,
  permissions: [],
  csrf: null
};

export const auth = writable<AuthState>(initial);

auth.subscribe((a) => setCsrf(a.csrf));
// A 401 from anywhere but /auth/* means the session is gone.
onUnauthorized(() => {
  if (get(auth).user) auth.update((a) => ({ ...a, user: null, permissions: [], csrf: null }));
});

export function hasPermission(state: Pick<AuthState, 'permissions'>, permission: string): boolean {
  return state.permissions.includes(permission);
}

/** `$can('users.manage')` in components. */
export const can = derived(auth, (a) => (permission: string) => hasPermission(a, permission));

export const user = derived(auth, (a) => a.user);

export async function refreshAuth(): Promise<void> {
  try {
    const s = await api.auth.state();
    auth.set({
      loaded: true,
      unreachable: null,
      setup_required: s.setup_required,
      user: s.user,
      permissions: s.permissions ?? [],
      csrf: s.csrf
    });
  } catch (e) {
    auth.update((a) => ({ ...a, loaded: true, unreachable: e instanceof Error ? e.message : String(e) }));
  }
}

export async function login(username: string, password: string): Promise<void> {
  const r = await api.auth.login(username, password);
  auth.set({
    loaded: true,
    unreachable: null,
    setup_required: false,
    user: r.user,
    permissions: r.permissions,
    csrf: r.csrf
  });
}

/** Setup answers with only `{ user }` and a cookie; fetch the full state afterwards. */
export async function setup(token: string, username: string, password: string): Promise<void> {
  await api.auth.setup(token, username, password);
  await refreshAuth();
}

export async function logout(): Promise<void> {
  try {
    await api.auth.logout();
  } finally {
    await refreshAuth();
  }
}
