import { b, hits, hit } from './cycle-b.mjs';
export const a = 'a';
export function fromB() { return b(); }
// `hits` is a live binding
export function liveCount() { hit(); return hits; }
