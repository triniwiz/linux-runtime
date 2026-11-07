import { a } from './cycle-a.mjs';
export let hits = 0;
export function hit() { hits++; }
export function b() { return `b sees ${a}`; }
