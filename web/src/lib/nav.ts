import type { AuthState } from './auth';
import { hasPermission } from './auth';

export interface NavItem {
  href: string;
  label: string;
  /** any one of these is enough; empty means any signed-in user */
  anyOf: string[];
}

// Labels follow the existing site: say / ytpmv / splice / corpus.
export const NAV: NavItem[] = [
  { href: '/', label: 'studio', anyOf: ['studio.use'] },
  { href: '/ytp', label: 'say', anyOf: ['generate.ytp'] },
  { href: '/ytpmv', label: 'ytpmv', anyOf: ['generate.ytpmv'] },
  { href: '/splice', label: 'splice', anyOf: ['corpus.read'] },
  { href: '/editor', label: 'corpus', anyOf: ['corpus.read'] },
  { href: '/admin', label: 'admin', anyOf: ['users.manage', 'roles.manage', 'audit.view', 'settings.manage'] }
];

export function allowed(state: Pick<AuthState, 'permissions'>, item: Pick<NavItem, 'anyOf'>): boolean {
  return item.anyOf.length === 0 || item.anyOf.some((p) => hasPermission(state, p));
}

export function itemFor(pathname: string): NavItem | undefined {
  return NAV.find((n) => (n.href === '/' ? pathname === '/' : pathname === n.href || pathname.startsWith(n.href + '/')));
}
