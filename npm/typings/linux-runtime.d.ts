/**
 * Additions of the NativeScript Linux runtime (github.com/NativeScript/linux)
 * to the shared `interop` API. C declarations come from the c!*.d.ts files,
 * generated from the runtime's metadata.
 */
declare module interop {
	/**
	 * Zero-copy view of native memory. `free` (e.g. `g_free`) runs when the buffer is collected.
	 */
	function bufferFromData(data: Pointer, length: number, free?: Function): ArrayBuffer;

	/**
	 * C function pointer that calls `func`, cached until `release(func)`.
	 */
	function callback(func: Function, signature: { returns: Type<any>; args: Type<any>[] }): Pointer;

	/**
	 * NUL-terminated copy of `value`, garbage collected with the returned Pointer.
	 */
	function cstring(value: string): Pointer;

	/**
	 * Frees the C function pointers made for `func`; returns how many were freed.
	 */
	function release(func: Function): number;
}

declare namespace nativescript {
	/** Keeps the event loop alive (e.g. while waiting on callbacks from other threads). */
	function ref(): void;
	function unref(): void;
}

/** A GVariant as plain JS values: numbers, strings, booleans, arrays, objects (`a{sv}`), `ay` as a Uint8Array. */
declare function g_variant_unpack(variant: interop.Pointer | null): any;

/** A D-Bus object (GDBusProxy) whose cached properties read as JS properties. */
interface DBusObject {
  readonly proxy: interop.Pointer;
  /** Calls a method; `parameters` is a GVariant tuple. Returns the unpacked reply. */
  call(method: string, parameters?: interop.Pointer | null): any;
  /** Runs `listener` when the service changes a property; returns the handler id. */
  onChanged(listener: () => void): number;
  /** Properties by their D-Bus (PascalCase) or camelCase name. */
  readonly [property: string]: any;
}

declare const DBus: {
  system(name: string, path: string, iface?: string): DBusObject;
  session(name: string, path: string, iface?: string): DBusObject;
};
