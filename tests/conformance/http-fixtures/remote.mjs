import { dep } from './dep.mjs';
export const value = `remote+${dep}`;
export const url = import.meta.url;
