<!--
  Root SvelteKit layout — imports the global stylesheet (`app.css`)
  so `:root` custom properties land on every page (main, /settings,
  /hud) without each route having to re-import.

  Also installs the appearance / theme override (#411 phase A).
  The stored preference (`localStorage["hush.theme"]`) is read
  synchronously at script-evaluation time and applied to `<html>`
  before the children mount, so there's no light → dark flicker
  on launch when a user has set a manual override. A Tauri event
  listener picks up changes from the Settings window and re-applies
  the attribute live.
-->
<script lang="ts">
  import "../app.css";

  import { onDestroy, onMount } from "svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import {
    applyThemeAttribute,
    listenForThemeChanges,
    readStoredTheme,
  } from "$lib/theme";

  // Apply at script-evaluation time so the very first paint
  // already reflects the user's choice. Guarded by feature
  // detection — server-side rendering would have neither
  // localStorage nor document, but the Tauri build is SPA so
  // the guard is just defensive.
  applyThemeAttribute(readStoredTheme());

  type Props = {
    children?: import("svelte").Snippet;
  };
  let { children }: Props = $props();

  let unlistenTheme: UnlistenFn | null = null;

  onMount(async () => {
    unlistenTheme = await listenForThemeChanges(applyThemeAttribute);
  });

  onDestroy(() => {
    unlistenTheme?.();
    unlistenTheme = null;
  });
</script>

{@render children?.()}
