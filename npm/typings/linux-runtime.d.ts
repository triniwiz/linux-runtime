/**
 * Additions of the NativeScript Linux runtime (github.com/NativeScript/linux)
 * to the shared `interop` API. C declarations come from the c!*.d.ts files,
 * generated from the runtime's metadata.
 */
declare module interop {
	/**
	 * A Pointer to the same memory that the garbage collector frees once the returned
	 * Pointer is collected, by calling `free` with it: a native function such as `g_free`
	 * or `g_object_unref` (default: libc `free`). The call runs after the GC, from the
	 * event loop. Use it for memory or references C hands over to the caller
	 * (`transfer full`), and keep the returned Pointer, not `ptr`.
	 * `interop.free` on it does nothing; adopting it again throws.
	 *
	 * ```js
	 * const buffer = interop.adopt(g_malloc0(64), g_free);
	 * const label = interop.adopt(g_object_ref_sink(gtk_label_new('hi')), g_object_unref);
	 * ```
	 */
	function adopt(ptr: Pointer, free?: Function): Pointer;

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

	interface StructType<T> {
		/**
		 * A GObject subclass of this class, like `class extends`: methods override the
		 * vfuncs and interface methods of the same name (or its snake_case form), `init`
		 * runs after construction with the constructor's arguments, and `this.super`
		 * calls the parent's implementation.
		 *
		 * ```js
		 * const Swatch = GtkWidget.extend('Swatch', {
		 *   snapshot(snapshot) { ... },
		 * });
		 * ```
		 */
		extend(name: string, members: GObjectClassMembers & ThisType<any>): any;
		extend(members: GObjectClassMembers & ThisType<any>): any;

		/** The GType registered for a JavaScript GObject class. */
		readonly $gtype: number | bigint;
	}
}

/** A property of a JavaScript GObject class, in `static properties`. */
interface GObjectPropertySpec {
	/** A G_TYPE_* constant, a GType, a GObject class, or an enum / flags / boxed type. Default G_TYPE_STRING. */
	type?: number | bigint | Function | string;
	default?: any;
	minimum?: number | bigint;
	maximum?: number | bigint;
	/** GParamFlags, default G_PARAM_READWRITE. */
	flags?: number;
	nick?: string;
	blurb?: string;
	/** GVariant properties: the variant type string, default 'v'. */
	variantType?: string;
	/** Implements a property declared by a parent class or an interface. */
	override?: boolean;
}

/** A signal of a JavaScript GObject class, in `static signals`. */
interface GObjectSignalSpec {
	/** Parameter GTypes (G_TYPE_* constants, GTypes or GObject classes). */
	params?: (number | bigint | Function | string)[];
	returnType?: number | bigint | Function | string;
	/** GSignalFlags, default G_SIGNAL_RUN_LAST. */
	flags?: number;
}

interface GObjectClassMembers {
	/** Interfaces the class implements: interface structs (`GListModelInterface`), GTypes or type names. */
	interfaces?: any[];
	properties?: Record<string, GObjectPropertySpec>;
	signals?: Record<string, GObjectSignalSpec>;
	/** Name of the GType, default the class name. */
	GTypeName?: string;
	[method: string]: any;
}

/** Fundamental GTypes (the G_TYPE_* C macros). */
declare const G_TYPE_INVALID: number;
declare const G_TYPE_NONE: number;
declare const G_TYPE_INTERFACE: number;
declare const G_TYPE_CHAR: number;
declare const G_TYPE_UCHAR: number;
declare const G_TYPE_BOOLEAN: number;
declare const G_TYPE_INT: number;
declare const G_TYPE_UINT: number;
declare const G_TYPE_LONG: number;
declare const G_TYPE_ULONG: number;
declare const G_TYPE_INT64: number;
declare const G_TYPE_UINT64: number;
declare const G_TYPE_ENUM: number;
declare const G_TYPE_FLAGS: number;
declare const G_TYPE_FLOAT: number;
declare const G_TYPE_DOUBLE: number;
declare const G_TYPE_STRING: number;
declare const G_TYPE_POINTER: number;
declare const G_TYPE_BOXED: number;
declare const G_TYPE_PARAM: number;
declare const G_TYPE_OBJECT: number;
declare const G_TYPE_VARIANT: number;

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

/** An app file or folder. */
interface NSBundleStat {
  readonly isFile: boolean;
  readonly isDirectory: boolean;
  readonly size: number;
  /** Served from app.nsbundle rather than a plain file. */
  readonly bundled: boolean;
  /** Extracted to app.nsbundle.unpacked (native code opens it by path). */
  readonly unpacked: boolean;
}

/**
 * Read-only app files. Release builds can ship the app folder as one archive (app.nsbundle,
 * optionally encrypted); paths under `root` read from it. Paths are absolute, `~/` or
 * relative to the app root. Without a bundle, reads go to the app folder on disk.
 */
declare const __nsBundle: {
  /** An app.nsbundle is mounted. */
  readonly mounted: boolean;
  /** The app folder path (virtual when mounted). */
  readonly root: string | undefined;
  /** The bundle is encrypted. */
  readonly sealed: boolean;
  /** Where unpacked files live on disk. */
  readonly unpackedRoot: string | undefined;
  /** `path` is inside the mounted bundle, so it can't be written. */
  contains(path: string): boolean;
  /** File contents. Unencrypted bundles map the archive without copying: writes to the buffer
   * are private to this process but seen by later reads of the same file. */
  read(path: string): ArrayBuffer;
  readText(path: string): string;
  exists(path: string): boolean;
  stat(path: string): NSBundleStat | undefined;
  /** Entry names of a folder, folders with a trailing `/`. */
  list(path: string): string[];
  /** A path native code can open (unpacked files), or null for files only in the archive. */
  realpath(path: string): string | null;
};
