import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri drives this; the fixed port is what `devUrl` in tauri.conf.json expects,
// and `strictPort` makes a clash an error rather than a silently different port
// the window would then fail to load -- which is a failure with no error
// attached to it, so it is worth making loud here.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // **`src-tauri` is not this dev server's business, and on Windows watching
    // it is fatal.** Vite walks the project and watches everything under it,
    // which includes `src-tauri/target` -- tens of thousands of build
    // artefacts, and one `sbx_desktop.exe` that the linker holds open. Windows
    // refuses a watch on a locked file, so the watcher throws `EBUSY` and takes
    // the dev server down with it, in the same second as a successful build:
    //
    //     Error: EBUSY: resource busy or locked, watch
    //     '...\\src-tauri\\target\\debug\\deps\\sbx_desktop.exe'
    //
    // On Linux the same watch is merely wasteful -- an inotify handle per file
    // and a rebuild of the frontend every time cargo writes -- which is why it
    // went unnoticed. Nothing in `src-tauri` is served to the webview anyway;
    // changes there are `cargo`'s to notice, and it does.
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
