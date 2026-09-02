// Vite's own ambient types.
//
// Here for one import: `?worker` is a Vite suffix, not a module that exists on
// disk, and without this `tsc` cannot type the worker Monaco needs. See
// panes/File.tsx for why that worker is not optional.
/// <reference types="vite/client" />
